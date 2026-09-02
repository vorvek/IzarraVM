// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Hard AVX2 gate.
//!
//! Per the owner's 2026-09-04 ruling (closing out
//! `dev_docs/2026-09-04-avx2-divergence-diagnosis.md`), the release build
//! targets `x86-64-v3` (`.cargo/config.toml`'s `[build] rustflags`). That
//! makes the old `--interpreter` fallback unreachable: a non-AVX2 host
//! takes `STATUS_ILLEGAL_INSTRUCTION` during CRT startup, before `main`'s
//! own code -- including `requested_execution_backend`'s error string --
//! ever runs. AVX2 is now a hard requirement, and this module is the
//! replacement contract: report the requirement in a message the user can
//! read, then exit cleanly, instead of crashing silently.
//!
//! [`require_avx2`] MUST be the first statement of every user-facing
//! `fn main` in this workspace. Nothing may run ahead of it: no argument
//! parsing, no logging setup, no eframe/winit setup. Its body is
//! deliberately minimal -- a single runtime feature test -- because until
//! that test has run, we do not yet know whether the host can execute a
//! VEX-encoded instruction at all. Under `-C target-cpu=x86-64-v3` the
//! compiler is free to lower an ordinary 32-byte (or larger) copy, a
//! `String` build, or a `format!` as `vmovdqu`/`vmovups`; any of those
//! ahead of the feature test would fault on exactly the host this
//! function exists to catch.
//!
//! This does NOT use `std::is_x86_feature_detected!("avx2")`. That macro
//! special-cases the case where the CURRENT crate is itself compiled with
//! the feature statically enabled (`#[cfg(target_feature = "avx2")]`) and
//! expands straight to the constant `true`, skipping the runtime `cpuid`
//! check entirely -- exactly this crate's situation under
//! `-C target-cpu=x86-64-v3`. Confirmed empirically: with the macro, the
//! whole `if` folded away at compile time (proven false to LLVM), and
//! `require_avx2`/`report_and_exit` vanished from the release PDB with no
//! symbol at all, i.e. the "hard requirement" checked nothing. Below,
//! [`avx2_present_at_runtime`] issues `cpuid` directly (via
//! `core::arch::x86_64::__cpuid[_count]`, which is not gated by any
//! `target_feature` and always emits the real instruction) so the check
//! cannot be constant-folded away by the same static assumption it exists
//! to catch a violation of.

/// Exact wording the owner approved: ASD-STE100 style (short sentences,
/// active voice, no idioms). Kept as a single `&'static str` so nothing
/// downstream needs to build or copy it before use.
pub static AVX2_REQUIRED_MESSAGE: &str = "This computer's processor does not support AVX2. IzarraVM needs AVX2 to run. \
     The program will close.";

/// Title for the Windows message box.
static AVX2_REQUIRED_TITLE: &str = "IzarraVM";

/// Convert an ASCII `&'static str` to a NUL-terminated UTF-16 array, entirely
/// at compile time. The array is emitted as a `.rodata` blob; nothing about
/// building it costs an instruction at runtime, so it carries no risk of an
/// AVX-lowered copy. `N` must be `s.len() + 1`; that invariant is asserted
/// at compile time by the `const` evaluation itself (an out-of-bounds write
/// or size mismatch is a compile error, not a runtime panic).
const fn ascii_to_utf16_nul<const N: usize>(s: &str) -> [u16; N] {
    let bytes = s.as_bytes();
    assert!(bytes.len() + 1 == N, "wide-string buffer size mismatch");
    let mut out = [0u16; N];
    let mut i = 0;
    while i < bytes.len() {
        assert!(bytes[i] < 0x80, "AVX2_REQUIRED_MESSAGE/TITLE must be ASCII");
        out[i] = bytes[i] as u16;
        i += 1;
    }
    out
}

#[cfg(windows)]
static AVX2_REQUIRED_MESSAGE_W: [u16; AVX2_REQUIRED_MESSAGE.len() + 1] =
    ascii_to_utf16_nul(AVX2_REQUIRED_MESSAGE);

#[cfg(windows)]
static AVX2_REQUIRED_TITLE_W: [u16; AVX2_REQUIRED_TITLE.len() + 1] =
    ascii_to_utf16_nul(AVX2_REQUIRED_TITLE);

/// Exit the process now if this host lacks AVX2. Must run before anything
/// else in `main`. See the module comment for why the body must stay this
/// small: a single feature test, and nothing else, ahead of the branch.
#[inline(never)]
pub fn require_avx2() {
    if !avx2_present_at_runtime() {
        report_and_exit();
    }
}

/// Real, uncachable, un-const-foldable `cpuid`-based AVX2 detection. See
/// the module comment for why `std::is_x86_feature_detected!` cannot be
/// used here. Checks the same three things the standard library's own
/// runtime detector does, because CPUID reporting AVX2 silicon support is
/// not sufficient on its own -- the OS must also have opted the SSE and
/// AVX register state into `XSAVE` (`CR4.OSXSAVE`), or using a YMM
/// register still raises `#UD`:
/// 1. CPUID leaf 1, ECX.OSXSAVE (bit 27): the OS enabled `xgetbv`/`xsave`.
/// 2. `XGETBV(XCR0)` bits 1 (SSE) and 2 (AVX): the OS saves/restores that
///    state across context switches.
/// 3. CPUID leaf 7 subleaf 0, EBX.AVX2 (bit 5): the CPU itself has AVX2.
#[inline(never)]
fn avx2_present_at_runtime() -> bool {
    // SAFETY: `cpuid` and `xgetbv` are plain, always-available x86_64
    // instructions (baseline since long before AVX2 existed); calling
    // them carries no precondition beyond running on x86_64, which this
    // module is `cfg`-gated to by virtue of `core::arch::x86_64` existing
    // in the target at all.
    unsafe {
        let leaf1 = std::arch::x86_64::__cpuid(1);
        let osxsave = (leaf1.ecx >> 27) & 1 != 0;
        if !osxsave {
            return false;
        }
        let xcr0 = xgetbv0();
        let sse_and_avx_state_saved = xcr0 & 0b110 == 0b110;
        if !sse_and_avx_state_saved {
            return false;
        }
        let leaf7 = std::arch::x86_64::__cpuid_count(7, 0);
        (leaf7.ebx >> 5) & 1 != 0
    }
}

/// `XGETBV` with `ecx = 0` (select `XCR0`), returning `EDX:EAX`. Not
/// exposed as a `core::arch` intrinsic, so this hand-codes the one
/// instruction via inline `asm!`.
///
/// # Safety
/// `xgetbv` is available whenever CPUID leaf 1 ECX.OSXSAVE is set, which
/// every caller here has already checked.
#[inline(never)]
unsafe fn xgetbv0() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        std::arch::asm!(
            "xgetbv",
            in("ecx") 0u32,
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack, preserves_flags),
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

/// Split out of [`require_avx2`] so the compiler has no reason to hoist any
/// of this -- formatting, the message box call, process teardown -- into
/// `require_avx2`'s own prologue, ahead of the feature test. Everything
/// here runs only after we already know the host lacks AVX2, and every
/// operation in it is either a call into precompiled `std`/`user32.dll`
/// code or a load of a precomputed `static`, never a copy this crate's own
/// `-C target-cpu=x86-64-v3` codegen could lower to a VEX instruction.
#[inline(never)]
fn report_and_exit() -> ! {
    eprintln!("{AVX2_REQUIRED_MESSAGE}");
    #[cfg(windows)]
    show_message_box();
    std::process::exit(1);
}

#[cfg(windows)]
fn show_message_box() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

    // SAFETY: both pointers are NUL-terminated `static` UTF-16 buffers that
    // outlive the call; `hwnd` is deliberately null (no parent window is
    // available, or needed, this early). `MessageBoxW` is a plain user32
    // FFI call, present in the `windows-sys` dependency this crate already
    // has for its Raw Input hook.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            AVX2_REQUIRED_MESSAGE_W.as_ptr(),
            AVX2_REQUIRED_TITLE_W.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(test)]
#[path = "preflight_test.rs"]
mod tests;
