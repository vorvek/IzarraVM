// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Offline generator for the Direct-JIT coverage matrix.
//!
//! Enumerates the decoder's cell space — opcode x ModRM digit x register/memory form x operand
//! size — and asks the REAL compile path whether each cell joins a block or ends it. The oracle
//! is `jit::direct::compile` over a warmed `inc; inc; inc; <candidate>` stream, the same idiom as
//! the allowlist fixtures, so a cell reports `native` only when decode, `classify` AND the
//! compile loop's admission gates (the stack-width matrix, the control-target clamp) all accept
//! it. A cell the block includes through an interpreter call-out slot reports `callout`.
//!
//! `#[ignore]` because it writes a report file instead of asserting. Run it explicitly:
//!
//!     cargo test -p izarravm-cpu generate_direct_coverage_matrix -- --ignored --nocapture
//!
//! The output path is `IZARRAVM_COVERAGE_MATRIX_OUT`, defaulting to
//! `dev_docs/coverage-matrix.tsv` at the workspace root.
//!
//! Axes deliberately NOT enumerated, because each is a blanket policy rather than a per-cell
//! decision: LOCK/REP/REPNE and the address-size override (always refused), segment-override
//! prefixes (admitted per data segment as a block property), and a true 16-bit CS (the 0x66 path
//! reaches the same Word classify cells; the CS.D=0 gate is dispatch policy). The sweep runs at
//! Gsw586, the widest decode surface; the 386/486 personas remove rows at the decode #UD gate,
//! never add them.

use super::*;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
    /// The opcode takes no ModRM byte; the digit axis collapses with it.
    None,
    /// ModRM mod=3, rm=ECX.
    Reg,
    /// ModRM mod=1, rm=ECX, disp8=0 — a plain `[ecx+0]` that never needs a SIB.
    Mem,
}

impl Form {
    fn label(self) -> &'static str {
        match self {
            Self::None => "-",
            Self::Reg => "reg",
            Self::Mem => "mem",
        }
    }
}

enum CellOutcome {
    Native,
    Callout,
    Refused,
    Retry,
    Structural,
    Panicked,
}

impl CellOutcome {
    fn label(&self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Callout => "callout",
            Self::Refused => "refused",
            Self::Retry => "retry",
            Self::Structural => "structural",
            Self::Panicked => "PANIC",
        }
    }
}

/// The candidate encoding for one cell. The tail is zero-filled: the decoder consumes whatever
/// displacement/immediate bytes the form still wants, and zero keeps relative branch targets at
/// the fall-through and immediates inert, so a control transfer is never misreported as refused
/// because a garbage displacement tripped the control-target clamp.
fn cell_bytes(opcode: u16, digit: u8, form: Form, word: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(12);
    if word {
        bytes.push(0x66);
    }
    if opcode >= 0x0f00 {
        bytes.push(0x0f);
    }
    bytes.push((opcode & 0xff) as u8);
    match form {
        Form::None => {}
        Form::Reg => bytes.push(0xc0 | (digit << 3) | 0x01),
        Form::Mem => {
            bytes.push(0x40 | (digit << 3) | 0x01);
            bytes.push(0x00);
        }
    }
    bytes.extend_from_slice(&[0u8; 8]);
    bytes
}

fn probe_decode(bytes: &[u8]) -> Option<DecodedInsn> {
    let (mut cpu, mut bus) = flat_fixture(ENTRY, bytes);
    cpu.begin_instruction();
    cpu.fetch_decoded(&mut bus, ENTRY).ok()
}

/// Compile `inc eax; inc ecx; inc edx; <candidate>` and read the admission off the span: the
/// fillers are unconditionally admitted, and `warm` seeds the decode cache for exactly these four
/// starts, so growth stops at the candidate and `instructions >= 4` means it joined the block —
/// as body or as terminator — while `3` means it was the barrier.
fn probe_compile(candidate: &[u8]) -> (CellOutcome, u8) {
    let mut code = vec![0x40, 0x41, 0x42];
    code.extend_from_slice(candidate);
    let probed = catch_unwind(AssertUnwindSafe(|| {
        let (mut cpu, mut bus) = flat_fixture(ENTRY, &code);
        warm(
            &mut cpu,
            &mut bus,
            &[ENTRY, ENTRY + 1, ENTRY + 2, ENTRY + 3],
        );
        match jit::direct::compile(&mut cpu, ENTRY, true) {
            jit::direct::CompileOutcome::Compiled(compilation) => {
                let insns = compilation.span.instructions;
                if insns < 4 {
                    (CellOutcome::Refused, insns)
                } else if compilation.callout_slots > 0 {
                    (CellOutcome::Callout, insns)
                } else {
                    (CellOutcome::Native, insns)
                }
            }
            jit::direct::CompileOutcome::StructuralReject(_) => (CellOutcome::Structural, 0),
            jit::direct::CompileOutcome::Retry => (CellOutcome::Retry, 0),
        }
    }));
    probed.unwrap_or((CellOutcome::Panicked, 0))
}

fn opcode_label(opcode: u16) -> String {
    if opcode >= 0x0f00 {
        format!("0f{:02x}", opcode & 0xff)
    } else {
        format!("{opcode:02x}")
    }
}

#[test]
#[ignore = "offline report generator, not an assertion — writes dev_docs/coverage-matrix.tsv"]
fn generate_direct_coverage_matrix() {
    /// Prefix bytes and the 0x0f escape never open an instruction cell of their own.
    const PREFIXES: [u8; 11] = [
        0x26, 0x2e, 0x36, 0x3e, 0x64, 0x65, 0x66, 0x67, 0xf0, 0xf2, 0xf3,
    ];

    let mut report = String::from("opcode\tdigit\tform\tosize\tgroup\tlen\toutcome\tinsns\n");
    let mut totals: [u32; 6] = [0; 6];

    // The compile probe panicking IS a recorded outcome (a classify/emit disagreement); silence
    // the per-panic backtrace spam for the duration of the sweep.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let mut opcodes: Vec<u16> = (0x00u16..=0xff)
        .filter(|opcode| *opcode != 0x0f && !PREFIXES.contains(&(*opcode as u8)))
        .collect();
    opcodes.extend(0x0f00u16..=0x0fff);

    for opcode in opcodes {
        // Learn the opcode's shape from neutral probes. Memory form first; the register-form and
        // bare fallbacks catch opcodes whose digit-0 memory encoding is itself invalid.
        let shape = probe_decode(&cell_bytes(opcode, 0, Form::Mem, false))
            .or_else(|| probe_decode(&cell_bytes(opcode, 0, Form::Reg, false)))
            .or_else(|| probe_decode(&cell_bytes(opcode, 0, Form::None, false)));
        let Some(shape) = shape else {
            let _ = writeln!(
                report,
                "{}\t-\t-\t-\t-\t-\tnodecode\t0",
                opcode_label(opcode)
            );
            continue;
        };

        let cells: Vec<(u8, Form)> = if shape.modrm.is_some() {
            (0u8..8)
                .flat_map(|digit| [(digit, Form::Reg), (digit, Form::Mem)])
                .collect()
        } else {
            vec![(0, Form::None)]
        };

        for (digit, form) in cells {
            for word in [false, true] {
                let osize = if word { "word" } else { "dword" };
                let digit_label = if form == Form::None {
                    "-".to_string()
                } else {
                    format!("/{digit}")
                };
                let bytes = cell_bytes(opcode, digit, form, word);
                let Some(insn) = probe_decode(&bytes) else {
                    let _ = writeln!(
                        report,
                        "{}\t{digit_label}\t{}\t{osize}\t-\t-\tnodecode\t0",
                        opcode_label(opcode),
                        form.label()
                    );
                    continue;
                };
                let (outcome, insns) = probe_compile(&bytes[..insn.len as usize]);
                totals[match outcome {
                    CellOutcome::Native => 0,
                    CellOutcome::Callout => 1,
                    CellOutcome::Refused => 2,
                    CellOutcome::Retry => 3,
                    CellOutcome::Structural => 4,
                    CellOutcome::Panicked => 5,
                }] += 1;
                let _ = writeln!(
                    report,
                    "{}\t{digit_label}\t{}\t{osize}\t{:?}\t{}\t{}\t{insns}",
                    opcode_label(opcode),
                    form.label(),
                    insn.group,
                    insn.len,
                    outcome.label()
                );
            }
        }
    }

    std::panic::set_hook(previous_hook);

    let out = std::env::var("IZARRAVM_COVERAGE_MATRIX_OUT").unwrap_or_else(|_| {
        format!(
            "{}/../../dev_docs/coverage-matrix.tsv",
            env!("CARGO_MANIFEST_DIR")
        )
    });
    std::fs::write(&out, &report).expect("write coverage matrix");
    println!(
        "coverage matrix: native {} | callout {} | refused {} | retry {} | structural {} | \
         panicked {} -> {out}",
        totals[0], totals[1], totals[2], totals[3], totals[4], totals[5]
    );
}
