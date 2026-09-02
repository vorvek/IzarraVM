// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Hard `x86-64-v3` gate, shown to the user as "AVX2" (the owner-approved
//! wording).
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
//! This module gates the **shipped GUI/CLI binary** (`crates/izarravm`)
//! only. `izarravm-corpus` and `izarravm-exodos` are internal, read-only
//! developer tooling built under the same `-C target-cpu=x86-64-v3`
//! rustflags but not distributed as the product; they are not gated here.
//!
//! [`require_avx2`] MUST be the first statement of every user-facing
//! `fn main` this module gates. Nothing may run ahead of it: no argument
//! parsing, no logging setup, no eframe/winit setup. Its body is
//! deliberately minimal -- a single runtime feature test -- because until
//! that test has run, we do not yet know whether the host can execute a
//! VEX-encoded instruction at all. Under `-C target-cpu=x86-64-v3` the
//! compiler is free to lower an ordinary 32-byte (or larger) copy, a
//! `String` build, or a `format!` as `vmovdqu`/`vmovups`; any of those
//! ahead of the feature test would fault on exactly the host this
//! function exists to catch.
//!
//! **`fn main` itself must own no locals.** The check being first in
//! source is not enough: under `-C target-cpu=x86-64-v3`, a `main` with
//! any local that forces callee-save xmm spills gets a prologue of VEX
//! `vmovapd`/`vmovaps` instructions before the first statement runs, and
//! those fault on a non-AVX host exactly like the bug this module exists
//! to fix. `main` must therefore be a locals-free shim that calls
//! [`require_avx2`] and then an `#[inline(never)]` `real_main` that holds
//! everything else -- verified by disassembly, not assumed.
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
//! [`x86_64_v3_present_at_runtime`] issues `cpuid`/`xgetbv` directly (via
//! `core::arch::x86_64::__cpuid[_count]`, which is not gated by any
//! `target_feature` and always emits the real instruction) so the check
//! cannot be constant-folded away by the same static assumption it exists
//! to catch a violation of.
//!
//! **The gate covers the full `x86-64-v3` feature set, not just AVX2.**
//! `x86-64-v3` is AVX2 + BMI1 + BMI2 + F16C + FMA + LZCNT + MOVBE, and LLVM
//! emits `bzhi`/`shlx`/`tzcnt`/`lzcnt`/`movbe` throughout this binary's
//! `.text` wherever it helps, not only inside AVX2-shaped code. A host
//! with AVX2 in silicon but a masked or absent BMI2/LZCNT leaf (a
//! partially-featured microarchitecture, or a hypervisor CPUID mask) would
//! pass an AVX2-only probe and then take `#UD` on the first such
//! instruction, with no message -- the same failure this module exists to
//! replace. [`AVX2_REQUIRED_MESSAGE`] keeps the owner-approved wording,
//! which names AVX2 as the user-facing shorthand for this requirement;
//! nothing here changes that text.

/// Exact wording the owner approved: ASD-STE100 style (short sentences,
/// active voice, no idioms). Kept as a single `&'static str` so nothing
/// downstream needs to build or copy it before use. Names AVX2 as the
/// shorthand for the full `x86-64-v3` requirement this module actually
/// checks; see the module comment.
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

/// Exit the process now if this host lacks the full `x86-64-v3` feature
/// set this build was compiled for. Must run before anything else in
/// `main` -- and `main` itself must be a locals-free shim; see the module
/// comment for both halves of that contract.
#[inline(never)]
pub fn require_avx2() {
    if !x86_64_v3_present_at_runtime() {
        report_and_exit();
    }
}

/// Pure decision: does this CPUID/XCR0 evidence show AVX2? Split out of
/// the `cpuid`/`xgetbv`-issuing code so it can be driven with synthetic
/// vectors in tests -- a mutation that always returns `true`, or that
/// drops the `xgetbv` gate, is otherwise invisible to the test suite. Bit
/// numbers per Intel SDM / AMD APM, `CPUID.1:ECX`:
/// - bit 27: `OSXSAVE` (the OS enabled `xsave`/`xgetbv`).
/// - bit 28: `AVX` (architecturally required for `AVX2`).
///
/// `XCR0` bits 1 (SSE state) and 2 (AVX state) must both be set for the OS
/// to actually save/restore that state across a context switch.
/// `CPUID.7.0:EBX` bit 5 is `AVX2` itself.
fn avx2_from_cpuid(leaf1_ecx: u32, xcr0: u64, leaf7_ebx: u32) -> bool {
    let osxsave = leaf1_ecx & (1 << 27) != 0;
    let avx = leaf1_ecx & (1 << 28) != 0;
    if !osxsave || !avx {
        return false;
    }
    let sse_and_avx_state_saved = xcr0 & 0b110 == 0b110;
    if !sse_and_avx_state_saved {
        return false;
    }
    leaf7_ebx & (1 << 5) != 0
}

/// Pure decision: does this CPUID/XCR0 evidence show the full `x86-64-v3`
/// feature set (AVX2 + BMI1 + BMI2 + F16C + FMA + LZCNT + MOVBE)? Builds
/// on [`avx2_from_cpuid`] and adds the bits LLVM's `x86-64-v3` codegen
/// also relies on:
/// - `CPUID.1:ECX` bit 12 (`FMA`), bit 22 (`MOVBE`), bit 29 (`F16C`).
/// - `CPUID.7.0:EBX` bit 3 (`BMI1`), bit 8 (`BMI2`).
/// - `CPUID.8000_0001h:ECX` bit 5 (`LZCNT`/`ABM`); `0` (never set) if the
///   host's extended CPUID range does not reach that leaf, which correctly
///   fails the gate rather than reading garbage.
fn x86_64_v3_from_cpuid(leaf1_ecx: u32, xcr0: u64, leaf7_ebx: u32, leaf80000001_ecx: u32) -> bool {
    if !avx2_from_cpuid(leaf1_ecx, xcr0, leaf7_ebx) {
        return false;
    }
    let fma = leaf1_ecx & (1 << 12) != 0;
    let movbe = leaf1_ecx & (1 << 22) != 0;
    let f16c = leaf1_ecx & (1 << 29) != 0;
    let bmi1 = leaf7_ebx & (1 << 3) != 0;
    let bmi2 = leaf7_ebx & (1 << 8) != 0;
    let lzcnt = leaf80000001_ecx & (1 << 5) != 0;
    fma && movbe && f16c && bmi1 && bmi2 && lzcnt
}

/// Real, uncachable, un-const-foldable `cpuid`-based `x86-64-v3` detection.
/// See the module comment for why `std::is_x86_feature_detected!` cannot
/// be used here. Gathers the raw CPUID/XCR0 evidence and hands the
/// decision to [`x86_64_v3_from_cpuid`], which is what the tests exercise
/// directly with synthetic inputs.
#[inline(never)]
fn x86_64_v3_present_at_runtime() -> bool {
    // SAFETY: `cpuid` is a plain, always-available x86_64 instruction
    // (baseline since long before AVX2 existed); calling it carries no
    // precondition beyond running on x86_64, which this module is
    // `cfg`-gated to by virtue of `core::arch::x86_64` existing in the
    // target at all. `xgetbv` is different -- it faults with `#UD` unless
    // `CR4.OSXSAVE` is set -- so it is called only after `leaf1.ecx` bit
    // 27 (`OSXSAVE`) has been checked; on a host without it, `xcr0` here
    // is left `0`, which correctly fails every downstream bit test rather
    // than reading real state.
    unsafe {
        let leaf1 = std::arch::x86_64::__cpuid(1);
        let osxsave = leaf1.ecx & (1 << 27) != 0;
        let xcr0 = if osxsave { xgetbv0() } else { 0 };
        let leaf7 = std::arch::x86_64::__cpuid_count(7, 0);
        let max_extended_leaf = std::arch::x86_64::__cpuid(0x8000_0000).eax;
        let leaf80000001_ecx = if max_extended_leaf >= 0x8000_0001 {
            std::arch::x86_64::__cpuid(0x8000_0001).ecx
        } else {
            0
        };
        x86_64_v3_from_cpuid(leaf1.ecx, xcr0, leaf7.ebx, leaf80000001_ecx)
    }
}

/// `XGETBV` with `ecx = 0` (select `XCR0`), returning `EDX:EAX`. Not
/// exposed as a `core::arch` intrinsic, so this hand-codes the one
/// instruction via inline `asm!`.
///
/// # Safety
/// Callers must have already confirmed `CPUID.1:ECX` bit 27 (`OSXSAVE`)
/// is set. `xgetbv` raises `#UD` when `CR4.OSXSAVE` is clear; the sole
/// caller, [`x86_64_v3_present_at_runtime`], only reaches this call on the
/// branch where that bit is set.
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
/// here runs only after we already know the host lacks the required
/// feature set, and every operation in it is either a call into
/// precompiled `std`/`kernel32`/`user32` code or a load of a precomputed
/// `static`, never a copy this crate's own `-C target-cpu=x86-64-v3`
/// codegen could lower to a VEX instruction.
///
/// The `stderr` write is deliberately infallible: `eprintln!` panics on a
/// write error (a closed handle, a broken pipe on the other end of a
/// redirect), which would unwind out of a `-> !` function and skip both
/// the dialog and `process::exit(1)` -- the host would see nothing at all
/// instead of at least the intended non-zero exit. `writeln!`'s `Result`
/// is discarded on purpose: there is no better fallback for a stderr
/// write that failed than to still show the dialog (if one is wanted) and
/// still exit non-zero.
#[inline(never)]
fn report_and_exit() -> ! {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{AVX2_REQUIRED_MESSAGE}");
    #[cfg(windows)]
    if dialog_wanted(no_dialog_env_is_set(), console_is_attached()) {
        show_message_box();
    }
    std::process::exit(1);
}

/// Pure decision: given whether `IZARRAVM_NO_DIALOG` is set and whether a
/// console is attached, should the modal dialog be shown? Split out of the
/// environment/`GetConsoleWindow` I/O so it is testable with plain `bool`s,
/// the same reasoning as [`x86_64_v3_from_cpuid`] over raw CPUID/XCR0
/// evidence: a mutation of the real condition (inverting it, or dropping
/// one half) is otherwise invisible to the test suite. The binary is
/// `IMAGE_SUBSYSTEM_WINDOWS_CUI`: it always has a console, either its own
/// or one it inherited from the launching shell. A modal `MessageBoxW` is
/// only the right move when nobody is positioned to read stderr --
/// launched from Explorer by double-click, where no console is attached.
/// With a console attached (an interactive shell, a CI runner, a
/// scoreboard harness), stderr already reached someone; showing a dialog
/// there would hang a headless run on a non-AVX2 CI box until the harness
/// timeout instead of failing fast. `IZARRAVM_NO_DIALOG` is an explicit
/// escape for the case where no console is attached at all (some CI
/// launchers spawn with none) but a human still is not there to click OK.
#[cfg(windows)]
fn dialog_wanted(no_dialog_env_set: bool, has_console: bool) -> bool {
    !no_dialog_env_set && !has_console
}

#[cfg(windows)]
fn no_dialog_env_is_set() -> bool {
    std::env::var_os("IZARRAVM_NO_DIALOG").is_some_and(|v| !v.is_empty())
}

/// `GetConsoleWindow` returning non-null is the actual "console attached"
/// evidence; wrapped here so [`dialog_wanted`] can stay pure.
#[cfg(windows)]
fn console_is_attached() -> bool {
    use windows_sys::Win32::System::Console::GetConsoleWindow;

    // SAFETY: `GetConsoleWindow` takes no arguments and has no precondition.
    !unsafe { GetConsoleWindow().is_null() }
}

#[cfg(windows)]
fn show_message_box() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

    // SAFETY: both pointers are NUL-terminated `static` UTF-16 buffers that
    // outlive the call; `hwnd` is deliberately null (no parent window is
    // available, or needed, this early). `MessageBoxW` is a plain user32
    // FFI call, present in the `windows-sys` dependency this crate already
    // has for its Raw Input hook. The return value is intentionally
    // ignored: in session 0 (no interactive desktop) it is 0 and there is
    // no better fallback than the stderr line already printed above.
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
