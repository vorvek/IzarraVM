// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Slice 0's red-first test: a synthetic reflected trip (a protected-mode
//! client's `INT n`, reflected through a same-privilege interrupt gate to a
//! small handler that pushes and pops one dword on the client's own stack,
//! then `IRET`s) is journaled with the expected write class and the
//! "restored" classification.
//!
//! Drives the module's PRIVATE `*_on` functions (`on_int_entry_on`,
//! `note_write_on`, `note_read_on`, `on_far_return_on`) against a LOCALLY
//! constructed `State`, rather than the public `armed()`-gated wrappers and
//! the process-global singleton. See the "Hooks" section comment in
//! `reflected_call_diag.rs` for why: `armed()` caches
//! `IZARRAVM_REFLECTED_CALL_DIAGNOSTIC` in a process-wide `OnceLock`, and this
//! crate's huge test suite runs in parallel in one binary under
//! `--all-features` -- an unrelated test's `CpuGsw::software_interrupt` call
//! would very likely resolve that `OnceLock` to "off" before this test's own
//! `std::env::set_var` ever ran, and even if arming won the race, every other
//! concurrently running test that touches `INT n` would feed the SAME global
//! `Mutex<State>` this test reads. The `_on` split makes the module testable
//! without either hazard: it exercises the identical trip-matching,
//! classification and restored-vs-net logic the real hooks run, just not the
//! `#[cfg(feature = ...)]` call sites themselves (those are a few one-line
//! delegations at each seam, reviewed by hand where they were added).
//!
//! The architecture setup (a real GDT with a flat code and a flat data
//! descriptor, a real IDT with one same-privilege 32-bit interrupt gate) is
//! deliberately minimal: CPL 0 throughout, one shared flat code segment for
//! both the client and the handler, no privilege crossing, no V86. The
//! design's own vocabulary calls the workload's shape "a pm16 INT to a
//! real-mode reflector"; this fixture stands in for that shape to validate
//! the INSTRUMENT (trip boundary detection, address classification,
//! restored-vs-net accounting), not to reproduce DPMI/V86 architecture,
//! which the crate's `cpu_v86_test.rs` already covers exhaustively.

use super::*;

const GDT_BASE: u32 = 0x1000;
const IDT_BASE: u32 = 0x2000;
const CODE_SELECTOR: u16 = 0x08;
const DATA_SELECTOR: u16 = 0x10;
const VECTOR: u8 = 0x21;
const CLIENT_RETURN_EIP: u32 = 0x0500;
const HANDLER_EIP: u32 = 0x0600;
const STACK_TOP: u32 = 0x4000;
const MEM_SIZE: usize = 0x8000;

/// A flat 4 GiB descriptor: base 0, limit 0xFFFFFFFF (G=1), D/B=1 (32-bit).
/// `access` distinguishes code (0x9B) from data (0x93).
fn write_flat_descriptor(mem: &mut [u8], at: u32, access: u8) {
    let at = at as usize;
    let limit_low: u16 = 0xffff;
    let limit_high_nibble: u8 = 0x0f;
    let g_and_d: u8 = 0b1100; // G=1, D/B=1, L=0, AVL=0
    mem[at] = (limit_low & 0xff) as u8;
    mem[at + 1] = (limit_low >> 8) as u8;
    mem[at + 2] = 0; // base low
    mem[at + 3] = 0;
    mem[at + 4] = 0; // base mid
    mem[at + 5] = access;
    mem[at + 6] = (g_and_d << 4) | limit_high_nibble;
    mem[at + 7] = 0; // base high
}

/// A same-privilege (DPL 0), 32-bit interrupt gate (type 0xE) naming
/// `CODE_SELECTOR:offset`.
fn write_interrupt_gate(mem: &mut [u8], vector: u8, offset: u32) {
    let at = (IDT_BASE + u32::from(vector) * 8) as usize;
    const ACCESS: u8 = 0x8e; // P=1, DPL=00, S=0, TYPE=1110 (32-bit interrupt gate)
    mem[at] = (offset & 0xff) as u8;
    mem[at + 1] = ((offset >> 8) & 0xff) as u8;
    mem[at + 2] = (CODE_SELECTOR & 0xff) as u8;
    mem[at + 3] = (CODE_SELECTOR >> 8) as u8;
    mem[at + 4] = 0;
    mem[at + 5] = ACCESS;
    mem[at + 6] = ((offset >> 16) & 0xff) as u8;
    mem[at + 7] = ((offset >> 24) & 0xff) as u8;
}

/// A minimal flat-memory `CpuBus`: no devices, no fast map, no charge
/// tracking -- just enough for the interrupt-delivery / push / pop / IRET
/// machinery to read and write real bytes, and for `peek_direct_ram` to serve
/// the instrument's pre-value peeks (the default `CpuBus::peek_direct_ram`
/// always declines, which would leave every `WriteRecord::pre` `None` and
/// defeat the whole point of this fixture).
struct FlatMemBus {
    mem: Vec<u8>,
}

impl FlatMemBus {
    fn new(size: usize) -> Self {
        Self {
            mem: vec![0u8; size],
        }
    }

    fn read_raw(&self, address: u32, width: BusWidth) -> u32 {
        let a = address as usize;
        match width {
            BusWidth::Byte => u32::from(self.mem[a]),
            BusWidth::Word => u32::from(u16::from_le_bytes([self.mem[a], self.mem[a + 1]])),
            BusWidth::Dword => u32::from_le_bytes([
                self.mem[a],
                self.mem[a + 1],
                self.mem[a + 2],
                self.mem[a + 3],
            ]),
        }
    }

    fn write_raw(&mut self, address: u32, width: BusWidth, value: u32) {
        let a = address as usize;
        match width {
            BusWidth::Byte => self.mem[a] = value as u8,
            BusWidth::Word => self.mem[a..a + 2].copy_from_slice(&(value as u16).to_le_bytes()),
            BusWidth::Dword => self.mem[a..a + 4].copy_from_slice(&value.to_le_bytes()),
        }
    }
}

impl CpuBus for FlatMemBus {
    fn read_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        _kind: BusAccessKind,
    ) -> Result<u32, izarravm_bus::BusError> {
        Ok(self.read_raw(address, width))
    }

    fn write_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        _kind: BusAccessKind,
    ) -> Result<(), izarravm_bus::BusError> {
        self.write_raw(address, width, value);
        Ok(())
    }

    fn peek_direct_ram(&self, address: u32, width: BusWidth) -> Option<u32> {
        Some(self.read_raw(address, width))
    }

    fn prefetch_memory(
        &mut self,
        address: u32,
        out: &mut [u8],
    ) -> Result<usize, izarravm_bus::BusError> {
        let a = address as usize;
        let n = out.len().min(self.mem.len().saturating_sub(a));
        out[..n].copy_from_slice(&self.mem[a..a + n]);
        Ok(n)
    }

    fn charge_instruction_fetch(&mut self, _address: u32) -> Result<(), izarravm_bus::BusError> {
        Ok(())
    }

    fn read_io(
        &mut self,
        _port: u16,
        _width: BusWidth,
        _core_clocks_so_far: u64,
        _cpu_is_ring0_pm: bool,
    ) -> Result<u32, izarravm_bus::BusError> {
        Ok(0xffff_ffff)
    }

    fn write_io(
        &mut self,
        _port: u16,
        _width: BusWidth,
        _value: u32,
        _core_clocks_so_far: u64,
        _cpu_is_ring0_pm: bool,
    ) -> Result<(), izarravm_bus::BusError> {
        Ok(())
    }

    fn interrupt_acknowledge(
        &mut self,
        _vector: u8,
        _ax: u16,
    ) -> Result<(), izarravm_bus::BusError> {
        Ok(())
    }
}

/// Build the CPU and bus: a client at `CS:CLIENT_RETURN_EIP` (already
/// advanced past its own `INT VECTOR`, matching the convention every INT hook
/// in this crate uses -- the decoder has always moved EIP by the time the
/// delivery runs), `SS:ESP = DATA_SELECTOR:STACK_TOP`, CPL 0, paging off, a
/// real GDT (null / flat code / flat data) and a real IDT with one
/// same-privilege gate at `VECTOR` targeting `CODE_SELECTOR:HANDLER_EIP`.
fn synthetic_reflected_client() -> (CpuGsw, FlatMemBus) {
    let mut bus = FlatMemBus::new(MEM_SIZE);
    write_flat_descriptor(&mut bus.mem, GDT_BASE + u32::from(CODE_SELECTOR), 0x9b);
    write_flat_descriptor(&mut bus.mem, GDT_BASE + u32::from(DATA_SELECTOR), 0x93);
    write_interrupt_gate(&mut bus.mem, VECTOR, HANDLER_EIP);

    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE; // protected mode, paging off
    cpu.cpl = 0;
    cpu.gdtr = DescriptorTable {
        base: GDT_BASE,
        limit: 0x17, // 3 x 8-byte entries: null, code, data
    };
    cpu.idtr = DescriptorTable {
        base: IDT_BASE,
        limit: 0x07ff, // 256 x 8-byte gates
    };
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(CODE_SELECTOR, 0x9b));
    for segment in [SegmentIndex::Ds, SegmentIndex::Ss, SegmentIndex::Es] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(DATA_SELECTOR, 0x93));
    }
    cpu.registers.set_esp(STACK_TOP);
    cpu.set_eip(CLIENT_RETURN_EIP);
    (cpu, bus)
}

/// THE RED PROOF. Runs the synthetic trip end to end -- `INT`, one push, one
/// matching pop, `IRET` -- through the REAL `CpuGsw::software_interrupt`,
/// `push`, `pop` and `iret`, journaling each step the same way the compiled-in
/// hooks would (see the module doc above for why this test drives `*_on`
/// directly rather than those hooks).
///
/// **Mutation bite** (the row's whole reason to exist): in `finish_trip`
/// (`reflected_call_diag.rs`), the write's restored-vs-net comparison is
///
///     if pre_masked == post_masked { write_restored += 1 } else { write_net_change += 1 }
///
/// Flip that equality to `!=` and this test goes red on the
/// `write_restored`/`write_net_change` assertions below -- a push immediately
/// followed by a matching pop is the textbook "restored" case, and the
/// inverted comparison reports it as a net change instead.
#[test]
fn a_synthetic_reflected_trip_is_journaled_with_the_expected_write_class_and_restored_verdict() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    let mut state = State::default();

    // 1. The client's `INT 0x21`. `on_int_entry_on` captures the entry image
    //    (CS:CLIENT_RETURN_EIP, SS:STACK_TOP) BEFORE delivery moves EIP/CS to
    //    the handler, exactly where the real hook sits in `software_interrupt`.
    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);
    assert!(state.open.is_some(), "the outer predicate must open a trip");

    cpu.software_interrupt(&mut bus, VECTOR).expect(
        "delivery must succeed: DPL 0 gate, DPL 0 target, CPL 0, no privilege \
         crossing, paging off",
    );
    assert_eq!(cpu.registers.cs().selector, CODE_SELECTOR);
    assert_eq!(
        cpu.registers.eip, HANDLER_EIP,
        "the handler must be entered"
    );

    // 2. The handler pushes one dword on the CLIENT's own stack (SS never
    //    changed: same-privilege entry). Journaled manually with the exact
    //    address `push` used -- `note_write_on` cannot see `push`'s internal
    //    address arithmetic, only the address a real seam call site would
    //    hand it.
    const PUSHED: u32 = 0xdead_beef;
    cpu.push(&mut bus, PUSHED, OperandSize::Dword)
        .expect("the push must succeed: flat data segment, plenty of room");
    let push_addr = cpu.registers.segment(SegmentIndex::Ss).base + cpu.registers.esp();
    note_write_on(
        &mut state,
        &mut cpu,
        &bus,
        push_addr,
        BusWidth::Dword,
        PUSHED,
        false,
        None,
    );

    // 3. The handler pops the SAME dword back off (the matching pop that
    //    makes this a "restored" write, not a net change).
    let popped = cpu
        .pop(&mut bus, OperandSize::Dword)
        .expect("the pop must succeed");
    assert_eq!(
        popped, PUSHED,
        "the fixture's own pop must read back its own push"
    );
    note_read_on(&mut state, &mut cpu, push_addr);

    // 4. The handler returns. `IRET` restores CS:EIP and SS:ESP to the
    //    client's entry values exactly (386 PRM: same-privilege IRET pops
    //    EFLAGS/CS/EIP and nothing else).
    cpu.iret(&mut bus, OperandSize::Dword)
        .expect("the matching IRET must succeed");
    assert_eq!(cpu.registers.cs().selector, CODE_SELECTOR);
    assert_eq!(
        cpu.registers.eip, CLIENT_RETURN_EIP,
        "the client resumes at its own return site"
    );
    assert_eq!(
        cpu.registers.esp(),
        STACK_TOP,
        "ESP must be exactly restored"
    );
    on_far_return_on(&mut state, &mut cpu);

    // ---- The trip closed, matched, exactly once. ----
    assert_eq!(
        state.trips_total, 1,
        "exactly one trip must have been recorded"
    );
    assert_eq!(
        state.trips_unmatched, 0,
        "the trip's own IRET must be the match, not a timeout"
    );
    assert!(
        state.open.is_none(),
        "the trip must be closed, not left open"
    );

    let key = state
        .keys
        .get(&(VECTOR, 0))
        .expect("the (vector, AH) key must be present (AH is 0: EAX was never touched)");
    assert_eq!(key.trips, 1);
    assert_eq!(key.unmatched, 0);

    // ---- The read excluded the trip's own earlier write (design vocabulary,
    // section 2): the pop's read of `push_addr` must NOT appear in the read
    // set, because the trip itself wrote that address first. ----
    assert_eq!(
        key.reads_total, 0,
        "a read of the trip's own earlier write is not an input and must be excluded"
    );

    // ---- The write set: exactly one entry, classified ClientStack (it is
    // below the client's own SP, on the client's own SS selector), dead under
    // BOTH the design's literal 8 KB cap AND the derived low-water-mark rule
    // (a single push/pop pair never leaves the trip's own excursion), and
    // RESTORED (the mutation bite this test exists for). ----
    let client_stack_index = ALL_CLASSES
        .iter()
        .position(|c| *c == AddressClass::ClientStack)
        .expect("ClientStack is one of ALL_CLASSES");
    assert_eq!(
        key.write_class_counts[client_stack_index], 1,
        "the push must classify as client_stack: {:?}",
        key.write_class_counts
    );
    assert_eq!(key.write_set_size.values.len(), 1);
    assert_eq!(
        key.write_dead_8kb, 1,
        "one push/pop pair is trivially within 8 KB of the final SP"
    );
    assert_eq!(
        key.write_dead_derived, 1,
        "one push/pop pair never leaves the trip's own observed SP excursion"
    );
    assert_eq!(key.write_live, 0);
    assert_eq!(
        key.write_restored, 1,
        "a push immediately popped back must classify RESTORED, not a net change"
    );
    assert_eq!(
        key.write_net_change, 0,
        "the mutation bite: inverting the pre/post comparison in finish_trip flips this to 1"
    );
    assert_eq!(
        key.write_unknown_pre, 0,
        "FlatMemBus serves every peek, so pre is always known"
    );
}

/// A trip whose real outcome is not a return (its INT is never matched) is
/// closed out as UNMATCHED by the next outer-predicate INT, not silently
/// dropped or left open forever (design section on trip identity; finding A3
/// -- a `^C` spawning `INT 23h` and never returning is exactly this shape).
#[test]
fn an_int_with_no_matching_return_is_recorded_as_unmatched_once_it_goes_stale() {
    let (mut cpu, bus) = synthetic_reflected_client();
    let mut state = State::default();

    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);
    assert!(state.open.is_some());

    // The "handler" never returns (imagine `spawn_int23`). A NESTED INT
    // arriving before the staleness bound is crossed must NOT close the
    // trip -- it is the common case (finding A4; see `on_int_entry_on`'s
    // comment for the measurement that made this the rule rather than "any
    // fresh INT abandons the open trip").
    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);
    assert_eq!(state.trips_total, 0, "a nested INT must not close the trip");
    assert!(state.open.is_some());
    assert_eq!(
        state.open.as_ref().unwrap().nested_int_count,
        1,
        "the nested INT is counted on the still-open trip"
    );

    // Now the trip goes stale: far more instructions than
    // `MAX_TRIP_INSNS` have retired since its `INT` with no matching
    // return. The NEXT INT (nested or not) must discover this and close
    // the trip out as UNMATCHED, then start its own fresh trip.
    cpu.perf.instructions += MAX_TRIP_INSNS + 1;
    cpu.set_eip(CLIENT_RETURN_EIP + 0x100);
    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);

    assert_eq!(state.trips_total, 1, "the stale trip must close out");
    assert_eq!(state.trips_unmatched, 1, "and it must be counted UNMATCHED");
    assert!(
        state.open.is_some(),
        "the INT that discovered the staleness starts its own trip"
    );
}
