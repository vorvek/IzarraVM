// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Slice 0b's tests (`dev_docs/2026-09-04-reflected-call-slice0b-plan.md` §10).
//! Drives the module's PRIVATE `*_on` functions against a LOCALLY constructed
//! `State`, exactly as slice 0's test file did -- see `reflected_call_diag.rs`'s
//! "Hooks" section comment for why: this crate's test suite runs many tests
//! in parallel in one binary, and the process-wide `armed()`/`journal_mode()`
//! `OnceLock`s cannot be re-armed per test. The one exception is
//! `one_production_write_seam_reaches_the_instrument` (N4's row), which uses
//! the module's `thread_local` test override instead of those `OnceLock`s --
//! see that test's own doc comment.

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
fn write_flat_descriptor(mem: &mut [u8], at: u32, access: u8) {
    write_flat_descriptor_sized(mem, at, access, true);
}

/// As `write_flat_descriptor`, with the D/B bit chosen explicitly -- `false`
/// builds a 16-bit-stack descriptor (plan §2.1: the entry stack's OWN `B`
/// bit is what decides the SP-compare width).
fn write_flat_descriptor_sized(mem: &mut [u8], at: u32, access: u8, big: bool) {
    let at = at as usize;
    let limit_low: u16 = 0xffff;
    let limit_high_nibble: u8 = 0x0f;
    let d_bit: u8 = if big { 1 } else { 0 };
    let g_and_d: u8 = 0b1000 | (d_bit << 2); // G=1, D/B as given, L=0, AVL=0
    mem[at] = (limit_low & 0xff) as u8;
    mem[at + 1] = (limit_low >> 8) as u8;
    mem[at + 2] = 0;
    mem[at + 3] = 0;
    mem[at + 4] = 0;
    mem[at + 5] = access;
    mem[at + 6] = (g_and_d << 4) | limit_high_nibble;
    mem[at + 7] = 0;
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
/// the instrument's pre-value peeks.
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
        let a = address as usize;
        let bytes = match width {
            BusWidth::Byte => 1,
            BusWidth::Word => 2,
            BusWidth::Dword => 4,
        };
        if a.checked_add(bytes)? > self.mem.len() {
            return None;
        }
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

/// Build the CPU and bus, parameterised over memory size, the client's own
/// `ESP` at entry and whether SS is a 32-bit ("big") stack. A client at
/// `CS:CLIENT_RETURN_EIP` (already advanced past its own `INT VECTOR`),
/// `SS:ESP = DATA_SELECTOR:stack_top`, CPL 0, paging off, a real GDT (null /
/// flat code / flat data) and a real IDT with one same-privilege gate at
/// `VECTOR` targeting `CODE_SELECTOR:HANDLER_EIP`.
fn synthetic_reflected_client_with(
    mem_size: usize,
    stack_top: u32,
    ss_big: bool,
) -> (CpuGsw, FlatMemBus) {
    let mut bus = FlatMemBus::new(mem_size);
    write_flat_descriptor(&mut bus.mem, GDT_BASE + u32::from(CODE_SELECTOR), 0x9b);
    write_flat_descriptor_sized(
        &mut bus.mem,
        GDT_BASE + u32::from(DATA_SELECTOR),
        0x93,
        ss_big,
    );
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
    let ss_register = SegmentRegister {
        selector: DATA_SELECTOR,
        base: 0,
        limit: 0xffff_ffff,
        access: 0x93,
        default_size_32: ss_big,
    };
    cpu.registers.set_segment(SegmentIndex::Ss, ss_register);
    for segment in [SegmentIndex::Ds, SegmentIndex::Es] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(DATA_SELECTOR, 0x93));
    }
    cpu.registers.set_esp(stack_top);
    cpu.set_eip(CLIENT_RETURN_EIP);
    (cpu, bus)
}

fn synthetic_reflected_client() -> (CpuGsw, FlatMemBus) {
    synthetic_reflected_client_with(MEM_SIZE, STACK_TOP, true)
}

/// THE RED PROOF. Runs the synthetic trip end to end -- `INT`, one push, one
/// matching pop, `IRET` -- through the REAL `CpuGsw::software_interrupt`,
/// `push`, `pop` and `iret`, journaling each step the same way the compiled-in
/// hooks would.
///
/// **Mutation bite**: in `classify_disposition` (`reflected_call_diag.rs`),
/// flip `restored == Some(true)` to `restored == Some(false)` and this test
/// goes red on the `write_class_r`/`write_class_d` assertions below -- a push
/// immediately followed by a matching pop is the textbook "restored" case.
#[test]
fn a_synthetic_reflected_trip_is_journaled_with_the_expected_write_class_and_restored_verdict() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    let mut state = State::default();

    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);
    assert!(state.open.is_some(), "the outer predicate must open a trip");

    cpu.software_interrupt(&mut bus, VECTOR, &mut CommittedCore::default())
        .expect(
            "delivery must succeed: DPL 0 gate, DPL 0 target, CPL 0, no privilege \
         crossing, paging off",
        );
    assert_eq!(cpu.registers.cs().selector, CODE_SELECTOR);
    assert_eq!(
        cpu.registers.eip, HANDLER_EIP,
        "the handler must be entered"
    );

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

    let popped = cpu
        .pop(&mut bus, OperandSize::Dword)
        .expect("the pop must succeed");
    assert_eq!(
        popped, PUSHED,
        "the fixture's own pop must read back its own push"
    );
    note_read_on(&mut state, &mut cpu, &bus, push_addr);

    cpu.iret(&mut bus, OperandSize::Dword, &mut CommittedCore::default())
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
    on_far_return_on(&mut state, &mut cpu, &bus);

    // ---- The trip closed, matched, exactly once. ----
    assert_eq!(state.trips_total, 1);
    assert_eq!(state.trips_unmatched, 0);
    assert!(state.open.is_none());

    let key = state
        .keys
        .get(&(VECTOR, 0))
        .expect("the (vector, AH) key must be present (AH is 0: EAX was never touched)");
    assert_eq!(key.trips, 1);
    assert_eq!(key.unmatched, 0);
    assert_eq!(key.closed_by[CloseRule::ReturnMatch.index()], 1);

    assert_eq!(
        key.reads_total, 0,
        "a read of the trip's own earlier write is not an input and must be excluded"
    );

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
        "one push/pop pair is trivially within 8 KB of the final SP (cross-check only)"
    );
    assert_eq!(
        key.write_class_r, 1,
        "a push immediately popped back must classify Restored"
    );
    assert_eq!(key.write_class_d, 0);
    assert_eq!(key.write_class_n, 0);
    assert_eq!(key.write_unknown_pre, 0);
}

/// A trip whose real outcome is not a return (its INT is never matched) is
/// closed out as STALE by the next outer-predicate INT, not silently dropped
/// or left open forever.
#[test]
fn an_int_with_no_matching_return_is_recorded_as_unmatched_once_it_goes_stale() {
    let (mut cpu, bus) = synthetic_reflected_client();
    let mut state = State::default();

    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);
    assert!(state.open.is_some());

    // A genuinely NESTED INT is issued from somewhere INSIDE the handler
    // body, not from the client's own return site -- move EIP so this
    // second INT does not also satisfy rule 3's re-entry signature (same
    // vector, CS.selector and EIP as the trip's own entry), which is a
    // different, correctly-handled case (see
    // `frame_gone_and_re_entry_close_the_trip_without_counting_a_match`).
    cpu.set_eip(HANDLER_EIP + 0x10);
    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);
    assert_eq!(state.trips_total, 0, "a nested INT must not close the trip");
    assert!(state.open.is_some());
    assert_eq!(
        state.open.as_ref().unwrap().nested_int_count,
        1,
        "the nested INT is counted on the still-open trip"
    );

    cpu.perf.instructions += MAX_TRIP_INSNS + 1;
    cpu.set_eip(CLIENT_RETURN_EIP + 0x100);
    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);

    assert_eq!(state.trips_total, 1, "the stale trip must close out");
    assert_eq!(state.trips_unmatched, 1);
    let key = state.keys.get(&(VECTOR, 0)).unwrap();
    assert_eq!(key.closed_by[CloseRule::Stale.index()], 1);
    assert!(
        state.open.is_some(),
        "the INT that discovered the staleness starts its own trip"
    );
}

/// Test 1 (plan §10 item 1), the whole reason 0b exists: a synthetic pm16
/// trip whose entry stack is NOT 32-bit (`SS.B == 0`) matches even though the
/// upper half of `ESP` is left dirty by a real-mode/V86 excursion the
/// architecture never reads on a 16-bit stack.
///
/// **Mutation bite**: in `Trip::sp_at_entry_width`, replace the body with
/// `esp` unconditionally (restoring slice 0's full 32-bit compare) and this
/// test goes red -- this is the slice-0 defect and this test is the
/// deliverable.
#[test]
fn sp_is_compared_at_sixteen_bits_when_the_entry_stack_is_not_big() {
    let (mut cpu, bus) = synthetic_reflected_client_with(MEM_SIZE, STACK_TOP, false);
    let mut state = State::default();

    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);
    let entry_esp = cpu.registers.esp();
    assert!(
        !state.open.as_ref().unwrap().entry_ss_big,
        "the fixture's SS must be 16-bit for this test to mean anything"
    );

    // The excursion leaves the upper ESP half dirty; the lower 16 bits (the
    // only ones a 16-bit stack's architecture ever reads) are exactly
    // restored, and CS:EIP land back on the client's own return site.
    cpu.registers.set_esp((entry_esp & 0xffff) | 0x0001_0000);

    on_far_return_on(&mut state, &mut cpu, &bus);

    assert_eq!(state.trips_total, 1);
    assert_eq!(
        state.trips_unmatched, 0,
        "rule 1 must MATCH despite a dirty upper ESP half on a 16-bit stack"
    );
    let key = state.keys.get(&(VECTOR, 0)).unwrap();
    assert_eq!(key.closed_by[CloseRule::ReturnMatch.index()], 1);
}

/// Test 2 (plan §10 item 2): on a 32-bit ("big") stack, where the upper ESP
/// half IS architecturally significant, a differing upper half correctly
/// fails to match -- and is recorded in the `near_match[]` `sp_high16`
/// bucket (candidate C1's signature), not silently dropped.
#[test]
fn a_stale_upper_esp_half_is_recorded_in_near_match_sp_high16() {
    // The entry ESP needs bit 16 SET so the "dirty" half can be CLEARED
    // (making the corrupted ESP smaller than the entry, so rule 2's
    // frame-gone condition -- SP > entry SP -- does not also fire and
    // swallow this as a real close; that is a separate, correctly-handled
    // case, not the near-miss this test means to isolate).
    const BIG_STACK_TOP: u32 = 0x0001_4000;
    let (mut cpu, bus) = synthetic_reflected_client_with(0x0002_0000, BIG_STACK_TOP, true);
    let mut state = State::default();

    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);
    let entry_esp = cpu.registers.esp();
    assert!(state.open.as_ref().unwrap().entry_ss_big);

    // Only the upper half differs (cleared); CS/EIP/SS and the lower 16 bits
    // of ESP all still agree with the entry.
    cpu.registers.set_esp(entry_esp & 0x0000_ffff);

    on_far_return_on(&mut state, &mut cpu, &bus);

    assert_eq!(state.trips_total, 0, "a near-miss must NOT close the trip");
    assert!(state.open.is_some(), "a near-miss keeps the trip open");

    let key = state
        .keys
        .get(&(VECTOR, 0))
        .expect("a near-miss folds into key stats immediately, even though the trip stays open");
    let idx = NearMissByBoundary::idx(BoundaryKind::FarReturn);
    assert_eq!(
        key.near_miss.near_match[idx][2], 1,
        "the sp_high16 bucket must fire"
    );
    assert_eq!(
        key.near_miss.near_match[idx][1], 0,
        "sp_low16 must NOT fire"
    );
    assert_eq!(
        key.near_miss.near_match[idx][0], 0,
        "ss_selector must NOT fire"
    );
    assert_eq!(key.near_miss.near_match[idx][3], 0, "cs_base must NOT fire");
}

/// Test 3 (plan §10 item 3): rules 2 (frame-gone) and 3 (re-entry) close the
/// trip but never count as a match.
///
/// **Mutation bite**: in `Trip::close_rule`, change the frame-gone arm to
/// `return Some(CloseRule::ReturnMatch)` and this test goes red on the
/// frame-gone half's `trips_unmatched` assertion.
#[test]
fn frame_gone_and_re_entry_close_the_trip_without_counting_a_match() {
    // --- Rule 2: frame-gone. ---
    let (mut cpu, bus) = synthetic_reflected_client();
    let mut state = State::default();
    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);
    let entry_esp = cpu.registers.esp();
    // CS/SS still match the entry; SP has moved PAST it (higher, at the
    // entry width) -- the client's own frame is already gone.
    cpu.registers.set_esp(entry_esp + 4);
    on_far_return_on(&mut state, &mut cpu, &bus);

    assert_eq!(state.trips_total, 1);
    assert_eq!(
        state.trips_unmatched, 1,
        "frame-gone must not count as a match"
    );
    let key = state.keys.get(&(VECTOR, 0)).unwrap();
    assert_eq!(key.closed_by[CloseRule::FrameGone.index()], 1);
    assert_eq!(key.closed_by[CloseRule::ReturnMatch.index()], 0);

    // --- Rule 3: re-entry. ---
    let (mut cpu2, bus2) = synthetic_reflected_client();
    let mut state2 = State::default();
    on_int_entry_on(&mut state2, &mut cpu2, &bus2, VECTOR);
    // A fresh INT with the SAME (vector, CS.selector, EIP) and SP restored
    // to the entry value: the identical call site firing again while the
    // first trip is still open.
    on_int_entry_on(&mut state2, &mut cpu2, &bus2, VECTOR);

    assert_eq!(
        state2.trips_total, 1,
        "the re-entry must close the FIRST trip"
    );
    assert_eq!(state2.trips_unmatched, 1);
    let key2 = state2.keys.get(&(VECTOR, 0)).unwrap();
    assert_eq!(key2.closed_by[CloseRule::ReEntry.index()], 1);
    assert!(
        state2.open.is_some(),
        "the re-entry starts its own fresh trip"
    );
}

/// Test 4 (plan §10 item 4), closing C3: a far `JMP`/`CALL` landing exactly
/// on the client's own return site now counts as `return_match` -- slice 0's
/// counter-only `on_far_transfer` could never see this.
#[test]
fn a_far_jmp_can_close_a_trip_as_a_return_match() {
    let (mut cpu, bus) = synthetic_reflected_client();
    let mut state = State::default();
    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);

    // The registers already sit exactly at the entry return site: stands in
    // for "a far JMP landed exactly on the client's own return site".
    on_far_transfer_boundary_on(&mut state, &mut cpu, &bus);

    assert_eq!(state.trips_total, 1);
    assert_eq!(
        state.trips_unmatched, 0,
        "a far transfer landing on the return site must count as return_match"
    );
    let key = state.keys.get(&(VECTOR, 0)).unwrap();
    assert_eq!(key.closed_by[CloseRule::ReturnMatch.index()], 1);
    assert_eq!(key.far_transfer_trips, 1);
}

/// Test 5 (plan §10 item 5 / §3.1): a byte-identical (pre == post) write to
/// the legacy VGA aperture is Class N, never Class R -- the CRTC reads guest
/// memory every scanline with no arming step, so even an intermediate value
/// there is observable on screen.
#[test]
fn a_restored_write_to_the_framebuffer_aperture_is_class_n_not_class_r() {
    let (mut cpu, mut bus) = synthetic_reflected_client_with(0x000c_0000, STACK_TOP, true);
    let mut state = State::default();
    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);

    const FB_ADDR: u32 = 0x000a_1234;
    const VALUE: u32 = 0x42;
    bus.write_raw(FB_ADDR, BusWidth::Byte, VALUE);
    note_write_on(
        &mut state,
        &mut cpu,
        &bus,
        FB_ADDR,
        BusWidth::Byte,
        VALUE,
        false,
        None,
    );

    on_far_return_on(&mut state, &mut cpu, &bus);
    assert_eq!(state.trips_total, 1);
    let key = state.keys.get(&(VECTOR, 0)).unwrap();
    assert_eq!(
        key.write_class_r, 0,
        "the framebuffer aperture must NEVER be Class R even though pre == post"
    );
    assert_eq!(key.write_class_n, 1);
    let fb_idx = ALL_CLASSES
        .iter()
        .position(|c| *c == AddressClass::FramebufferAperture)
        .unwrap();
    assert_eq!(key.write_class_counts[fb_idx], 1);
}

/// Test 6 (plan §10 item 6 / B2's deletion bite): a write 16 KB below the
/// FINAL SP, but inside the trip's own observed low-water excursion, is
/// Class D -- the literal 8 KB cap plays no part in the decision (it is
/// reported separately, as a cross-check only, and must NOT have admitted
/// this write).
///
/// **Mutation bite**: restore the 8 KB cap as the GATE in
/// `classify_disposition` (i.e. call `is_dead_stack_8kb` instead of
/// `is_dead_stack_derived`) and this test goes red on `write_class_d`.
#[test]
fn a_write_below_the_low_water_mark_is_class_d_with_no_constant_cap() {
    const BIG_STACK_TOP: u32 = 0x0001_0000;
    let (mut cpu, mut bus) = synthetic_reflected_client_with(0x0002_0000, BIG_STACK_TOP, true);
    let mut state = State::default();
    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);
    let base = cpu.registers.segment(SegmentIndex::Ss).base;
    let entry_esp = cpu.registers.esp();

    // The trip's own excursion reaches far below entry SP at some point (a
    // deep nested call), tracked purely by observation -- no constant is
    // consulted here.
    cpu.registers.set_esp(entry_esp - 20_000);
    let low_water_addr = base + cpu.registers.esp();
    note_read_on(&mut state, &mut cpu, &bus, low_water_addr);

    // Unwind most of the way back up before the write under test.
    cpu.registers.set_esp(entry_esp);

    let write_addr = base + (entry_esp - 16_000);
    bus.write_raw(write_addr, BusWidth::Byte, 0x00);
    note_write_on(
        &mut state,
        &mut cpu,
        &bus,
        write_addr,
        BusWidth::Byte,
        0x11,
        false,
        None,
    );

    on_far_return_on(&mut state, &mut cpu, &bus);
    assert_eq!(state.trips_total, 1);
    let key = state.keys.get(&(VECTOR, 0)).unwrap();
    assert_eq!(
        key.write_class_d, 1,
        "16 KB below the final SP, but within the trip's own observed low-water excursion"
    );
    assert_eq!(key.write_class_r, 0);
    assert_eq!(key.write_class_n, 0);
    assert_eq!(
        key.write_dead_8kb, 0,
        "the literal 8 KB cap must NOT have admitted this write (cross-check only)"
    );
}

/// Test 7 (plan §10 item 7 / plan §4): the non-charging walker resolves a
/// physical address on a cold TLB (paging enabled, no prior translation has
/// ever run, so `probe_linear_read_physical` alone -- slice 0's only path --
/// would decline).
///
/// **Mutation bite**: revert `probe_physical` to `cpu.probe_linear_read_physical(linear).map(|p| (p, None))`
/// alone and this test goes red.
#[test]
fn the_walker_resolves_a_physical_address_on_a_tlb_miss() {
    const CR3: u32 = 0x0000_2000;
    const LINEAR: u32 = 0x0040_1000; // pde index 1, pte index 1
    const PAGE_TABLE_PHYS: u32 = 0x0000_3000;
    const PAGE_PHYS: u32 = 0x0009_0000;

    let mut bus = FlatMemBus::new(0x000a_0000);
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = CR3;

    let pde_index = LINEAR >> 22;
    let pte_index = (LINEAR >> 12) & 0x3ff;
    let pde_addr = CR3 + pde_index * 4;
    bus.write_raw(pde_addr, BusWidth::Dword, PAGE_TABLE_PHYS | 0x1);
    let pte_addr = PAGE_TABLE_PHYS + pte_index * 4;
    bus.write_raw(pte_addr, BusWidth::Dword, PAGE_PHYS | 0x1);

    // No translation has EVER run against this CPU: the TLB is cold.
    assert!(
        cpu.probe_linear_read_physical(LINEAR).is_none(),
        "the TLB-hit-only seam must miss here -- that miss is this test's whole premise"
    );

    let resolved = probe_physical(&cpu, &bus, LINEAR);
    let expected_phys = PAGE_PHYS | (LINEAR & 0x0fff);
    match resolved {
        Some((phys, Some(_walk))) => assert_eq!(phys, expected_phys),
        other => panic!(
            "the walker must resolve a physical address on a TLB miss, got {:?}",
            other.map(|(p, w)| (p, w.is_some()))
        ),
    }
}

/// Test 8 (plan §10 item 8 / N4's row): one production write seam (driven
/// through `CpuGsw::write_memory_u8`, which reaches the private
/// `write_linear_u8`'s `note_write` call) reaches the instrument's OPEN
/// trip in the process-global singleton. Today deleting every hook turns no
/// OTHER test in this file red -- they all drive `note_write_on` directly --
/// so this is the one test that actually exercises the `#[cfg(...)]` call
/// site itself.
///
/// Uses the module's `thread_local` test override (`test_force_armed`), NOT
/// the process-wide `ARMED`/`MODE` `OnceLock`s: a `#[test]` fn runs to
/// completion on one worker thread before that thread is reused for a
/// different test, so this cannot leak into a concurrently running test the
/// way flipping the real env-cached `OnceLock`s would (see the module's
/// `TEST_OVERRIDE` doc comment). This is also the ONLY test in this file
/// that touches the process-global `state()` singleton; every other test
/// here uses a locally constructed `State`.
#[test]
fn one_production_write_seam_reaches_the_instrument() {
    let (mut cpu, mut bus) = synthetic_reflected_client();

    // Seed the global singleton's open trip directly (not through
    // `on_int_entry`'s `armed()`-gated wrapper -- this test does not need
    // the real INT/IDT machinery, only an already-open trip to write into).
    let trip = Trip::start(&mut cpu, &bus, VECTOR, 0);
    {
        let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
        guard.open = Some(trip);
    }

    test_force_armed(true); // journal mode

    const OFFSET: u32 = 0x0100;
    const VALUE: u8 = 0x77;
    let result = cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ss,
        OFFSET,
        VALUE,
        BusAccessKind::DataWrite,
    );

    test_clear_armed();

    assert!(result.is_ok(), "the write itself must succeed: {result:?}");
    let mut guard = state().lock().unwrap_or_else(|e| e.into_inner());
    let trip = guard
        .open
        .take()
        .expect("the trip must still be open -- this test never closed it");
    // The fixture's SS is a flat descriptor (base 0), so the linear address
    // is the offset directly.
    assert!(
        trip.writes.contains_key(&OFFSET),
        "CpuGsw::write_memory_u8 (which reaches write_linear_u8's note_write call) must have \
         driven the write through to the instrument's open trip; recorded addresses: {:?}",
        trip.writes.keys().collect::<Vec<_>>()
    );
    assert_eq!(trip.writes[&OFFSET].latest, u32::from(VALUE));
}

// ---------------------------------------------------------------------------
// Slice 0c tests (dev_docs/2026-09-04-reflected-call-slice0b-review.md, D1-D6)
// ---------------------------------------------------------------------------

/// D1: a far RETURN that lands exactly on the entry's own CS:EIP:SS, but with
/// SP sitting at the entry width MINUS TWO -- the shape of a handler that
/// returns by `RETF`, popping only CS:IP and leaving the `INT`-pushed FLAGS
/// word behind on the caller's own stack -- must close as the NEW
/// `return_match_retf_flags` bucket, and must count as a MATCH (not go
/// through rule 2's `frame_gone`, which needs SP > entry, not SP < entry).
///
/// **Mutation bite**: delete the RETF-with-flags arm from `Trip::close_rule`
/// and this test goes red: with SP below the entry value, neither rule 1 nor
/// rule 2 fires, the boundary is a near-miss, and the trip never closes.
#[test]
fn a_retf_landing_at_entry_minus_two_closes_as_return_match_retf_flags() {
    let (mut cpu, bus) = synthetic_reflected_client();
    let mut state = State::default();
    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);
    let entry_esp = cpu.registers.esp();

    // CS/EIP/SS already sit at the entry's own return site (the fixture
    // never actually executed the INT). Only SP moves: exactly 2 below the
    // entry value, as a `RETF` that popped CS:IP but left FLAGS behind would
    // leave it.
    cpu.registers.set_esp(entry_esp - 2);

    on_far_return_on(&mut state, &mut cpu, &bus);

    assert_eq!(state.trips_total, 1);
    assert_eq!(
        state.trips_unmatched, 0,
        "the RETF-with-flags landing must count as a match"
    );
    let key = state.keys.get(&(VECTOR, 0)).unwrap();
    assert_eq!(
        key.closed_by[CloseRule::ReturnMatchRetfFlags.index()],
        1,
        "must close via the NEW bucket, not the plain return_match one"
    );
    assert_eq!(key.closed_by[CloseRule::ReturnMatch.index()], 0);
    assert_eq!(key.closed_by[CloseRule::FrameGone.index()], 0);
}

/// D1: the RETF-with-flags arm is defined only for a far RETURN boundary
/// (`BoundaryKind::FarReturn`) -- a far `JMP`/`CALL` has no FLAGS word of its
/// own to leave behind, so the SAME "SP = entry - 2" shape at a far
/// TRANSFER must NOT match.
#[test]
fn the_retf_with_flags_arm_never_fires_on_a_far_transfer() {
    let (mut cpu, bus) = synthetic_reflected_client();
    let mut state = State::default();
    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);
    let entry_esp = cpu.registers.esp();
    cpu.registers.set_esp(entry_esp - 2);

    on_far_transfer_boundary_on(&mut state, &mut cpu, &bus);

    assert_eq!(
        state.trips_total, 0,
        "a far transfer at SP = entry - 2 must NOT close the trip"
    );
    assert!(state.open.is_some());
}

/// D2: `peek_direct_ram(phys, width)` declines on a misaligned word/dword
/// access even over ordinary RAM (`should_split`). A misaligned RAM write
/// must still classify plain RAM -- never `not_plain_ram` -- once the
/// byte-wise fallback is in place.
///
/// **Mutation bite**: in `note_write_on`, replace `peek_ram_width_safe(bus,
/// p, width)` with `bus.peek_direct_ram(p, width)` (0b's original call) and
/// this test goes red: the odd-address word peek declines, `plain_ram`
/// becomes `false`, and the write classifies `NotPlainRam`.
#[test]
fn a_misaligned_ram_write_is_not_classified_not_plain_ram() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    let mut state = State::default();
    on_int_entry_on(&mut state, &mut cpu, &bus, VECTOR);

    // An ODD word address, ordinary RAM, well clear of the fixture's GDT
    // (0x1000), IDT (0x2000), BDA (0x400..0x500) and stack (around 0x4000).
    const ODD_ADDR: u32 = 0x0141;
    bus.write_raw(ODD_ADDR, BusWidth::Word, 0x1234);
    note_write_on(
        &mut state,
        &mut cpu,
        &bus,
        ODD_ADDR,
        BusWidth::Word,
        0x5678,
        false,
        None,
    );

    on_far_return_on(&mut state, &mut cpu, &bus);
    assert_eq!(state.trips_total, 1);
    let key = state.keys.get(&(VECTOR, 0)).unwrap();
    assert_eq!(
        key.write_not_plain_ram, 0,
        "a misaligned write to ordinary RAM must not classify not_plain_ram"
    );
    assert_eq!(
        key.write_unknown_pre, 0,
        "the byte-wise fallback must also resolve a `pre` value"
    );
}

/// D3: `TSS.ESP0`/`SS0` must be read at the offset for the TR descriptor's
/// ACTUAL type -- a 16-bit (286-style) TSS has SP0 at offset 2 and SS0 at
/// offset 4, NOT ESP0 at offset 4 as a 32-bit TSS does.
///
/// **Mutation bite**: hardcode `esp0_off`/`ss0_off`/`esp0_width` to the
/// 32-bit TSS's `(4, 8, Dword)` regardless of `cpu.tr.access` (0b's original
/// behaviour) and this test goes red: the dword read at offset 4 combines
/// this fixture's SS0 word with the two (zero) bytes after it, not `ESP0`.
#[test]
fn tss_esp0_is_read_from_the_right_offset_for_a_sixteen_bit_tss() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    const TSS_BASE: u32 = 0x0500;
    const SP0: u16 = 0x1234;
    const SS0: u16 = 0x0018;
    bus.write_raw(TSS_BASE + 2, BusWidth::Word, u32::from(SP0));
    bus.write_raw(TSS_BASE + 4, BusWidth::Word, u32::from(SS0));
    cpu.tr.base = TSS_BASE;
    cpu.tr.limit = 0x2c;
    // P=1, DPL=00, S=0, TYPE=0001 (16-bit available TSS) -- bit 3 of the
    // type nibble (0x08) is the 32-bit/16-bit discriminator this fixture
    // means to exercise.
    cpu.tr.access = 0x81;

    let trip = Trip::start(&mut cpu, &bus, VECTOR, 0);
    assert_eq!(
        trip.tss_esp0_at_entry,
        Some(u32::from(SP0)),
        "SP0 must come from offset 2 on a 16-bit TSS"
    );
    assert_eq!(
        trip.tss_ss0_selector_at_entry,
        Some(SS0),
        "SS0 must come from offset 4 on a 16-bit TSS"
    );
}

/// D5: `on_batch_boundary` must only advance `batch_boundaries_seen` when
/// the caller tags the boundary `real_boundary: true` -- a `false` tag (the
/// trip's own IF-enable edge) must be a no-op, so `batch_straddle_trips`
/// stops being tautological over a reflected trip's own nested `IRET`s.
/// Drives the private `on_batch_boundary_on` against a LOCALLY constructed
/// `State`, same as every other test in this file except
/// `one_production_write_seam_reaches_the_instrument` (see this file's own
/// header comment for why).
///
/// **Mutation bite**: delete the `if !real_boundary { return; }` guard from
/// `on_batch_boundary_on` and this test goes red: all three calls count.
#[test]
fn on_batch_boundary_only_counts_real_boundaries() {
    let (mut cpu, bus) = synthetic_reflected_client();
    let mut state = State {
        open: Some(Trip::start(&mut cpu, &bus, VECTOR, 0)),
        ..State::default()
    };

    on_batch_boundary_on(&mut state, false);
    on_batch_boundary_on(&mut state, false);
    on_batch_boundary_on(&mut state, true);

    let trip = state
        .open
        .as_ref()
        .expect("this test never closes the trip");
    assert_eq!(
        trip.batch_boundaries_seen, 1,
        "only the one real_boundary=true call may count"
    );
}
