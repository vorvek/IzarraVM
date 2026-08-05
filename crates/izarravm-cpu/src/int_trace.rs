// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Guest `INT n` tracer, compiled in only under the `int-trace` feature.
//!
//! Prints both halves of a guest's call into a driver: the arguments at the
//! `INT` instruction, and the answer when execution returns to the instruction
//! after it. Both halves matter — the TOKAEMM shared-pool defect was a handler
//! returning `AH=88h` where a reference manager returned success, which the
//! argument side alone could not have shown.
//!
//! `IZARRAVM_INT_TRACE=67,21` traces INT 67h and INT 21h. Vectors are hex.
//!
//! READING THE OUTPUT. Two properties bound what a trace can be used to claim,
//! and neither is visible in the log itself:
//!
//! - ONE call is outstanding at a time. A handler that issues its own traced
//!   `INT` takes over the pending slot, so the OUTER call's answer is never
//!   printed. This is not a corner case: `INT 21h` opening a file calls
//!   `INT 13h`, and a bare FreeDOS boot traced at `10,13,21` loses 2 of 704
//!   answers exactly that way. A traced `INT` with no `  -> ` under it means
//!   the call nested or the handler never returned — never that the handler
//!   answered with nothing. A slot stack would close this and is deliberately
//!   not built; the pairs that do survive are what a driver conversation gets
//!   read from.
//! - The address printed after `ret=` is the RETURN site, one instruction past
//!   the `INT`, because that is the value the pending slot must match. Subtract
//!   the instruction's length to find the call in a disassembly.
//!
//! The slot is one process-global pair, so a process driving more than one
//! `CpuGsw` would interleave two guests into one trace. That is the
//! instrument's assumption, not an oversight — it is armed by hand for a
//! single-machine run.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Sentinel for "no call is outstanding". A real site is `(cs << 32) | eip`,
/// and `eip` is never `u32::MAX` at an instruction boundary here.
const NO_PENDING: u64 = u64::MAX;

/// `Relaxed` on both statics below. They carry their own values and publish no
/// other memory, so there is no happens-before for an acquire/release pair to
/// establish; the same reasoning `WRITE_WATCH` records in `lib.rs`. Observation
/// only — nothing here feeds back into guest state.
static PENDING: AtomicU64 = AtomicU64::new(NO_PENDING);
/// Mirrors `PENDING != NO_PENDING` so the per-instruction hook reads one
/// relaxed bool instead of a 64-bit compare against a value it usually ignores.
static ARMED: AtomicBool = AtomicBool::new(false);

/// Parse `IZARRAVM_INT_TRACE`'s value: comma-separated hex vectors, whitespace
/// around a token ignored, empty tokens (a bare `,,` or a trailing comma)
/// silently skipped, anything else that doesn't parse as hex reported to
/// stderr and otherwise ignored so one typo doesn't drop the whole set.
fn parse_vectors(spec: &str) -> [bool; 256] {
    let mut set = [false; 256];
    for token in spec.split(',') {
        match u8::from_str_radix(token.trim(), 16) {
            Ok(vector) => set[usize::from(vector)] = true,
            Err(_) if token.trim().is_empty() => {}
            Err(_) => eprintln!("int-trace: ignoring vector {token:?} (want hex)"),
        }
    }
    set
}

fn traced() -> &'static [bool; 256] {
    static SET: OnceLock<[bool; 256]> = OnceLock::new();
    SET.get_or_init(|| {
        std::env::var("IZARRAVM_INT_TRACE")
            .map(|spec| parse_vectors(&spec))
            .unwrap_or([false; 256])
    })
}

/// True when this vector is in the traced set.
pub(crate) fn is_traced(vector: u8) -> bool {
    traced()[usize::from(vector)]
}

/// Record the arguments of an `INT n` and arm the return-site report.
/// `return_eip` is the address the handler will come back to — the caller has
/// already advanced EIP past the instruction, so it is simply the current EIP.
#[allow(clippy::too_many_arguments)]
pub(crate) fn on_entry(
    vector: u8,
    cs: u16,
    return_eip: u32,
    regs: [u32; 8],
    ds: u16,
    es: u16,
    v86: bool,
    iopl: u8,
) {
    let [eax, ecx, edx, ebx, esp, _ebp, esi, edi] = regs;
    eprintln!(
        "INT {vector:02X} ret={cs:04X}:{return_eip:08X} EAX={eax:08X} EBX={ebx:08X} \
         ECX={ecx:08X} EDX={edx:08X} ESP={esp:08X} ESI={esi:08X} EDI={edi:08X} DS={ds:04X} \
         ES={es:04X} v86={v86} iopl={iopl}"
    );
    PENDING.store(
        (u64::from(cs) << 32) | u64::from(return_eip),
        Ordering::Relaxed,
    );
    ARMED.store(true, Ordering::Relaxed);
}

/// Cheap gate for the per-instruction hook.
#[inline]
pub(crate) fn armed() -> bool {
    ARMED.load(Ordering::Relaxed)
}

/// Report the handler's answer if this instruction is the outstanding return
/// site. Call only when [`armed`] is true.
pub(crate) fn on_instruction(cs: u16, eip: u32, regs: [u32; 8], cf: bool) {
    let site = (u64::from(cs) << 32) | u64::from(eip);
    if PENDING.load(Ordering::Relaxed) != site {
        return;
    }
    PENDING.store(NO_PENDING, Ordering::Relaxed);
    ARMED.store(false, Ordering::Relaxed);
    let [eax, ecx, edx, ebx, esp, _ebp, esi, edi] = regs;
    eprintln!(
        "  -> EAX={eax:08X} EBX={ebx:08X} ECX={ecx:08X} EDX={edx:08X} ESP={esp:08X} \
         ESI={esi:08X} EDI={edi:08X} CF={}",
        u8::from(cf),
    );
}

#[cfg(test)]
#[path = "int_trace_test.rs"]
mod tests;
