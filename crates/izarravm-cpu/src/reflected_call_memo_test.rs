// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Slice 1's tests (`dev_docs/2026-09-04-reflected-call-slice1-plan.md`
//! Revision 2, "Revision 2 amendments" item 6): section 8's tests 1, 2, 3, 8,
//! 13, 14, 15, plus the two new tests item 6 names. Unlike
//! `reflected_call_diag_test.rs` (which drives private `*_on` functions
//! against a locally constructed `State` because the diagnostic's `armed()`
//! is a process-global `OnceLock`), this module's state lives on `CpuGsw`
//! itself, so a test simply builds a `CpuGsw`, sets `cpu.reflected_call =
//! Some(...)` directly, and drives the real hook functions -- no process
//! state, no test-only override needed.

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

fn write_flat_descriptor(mem: &mut [u8], at: u32, access: u8) {
    write_flat_descriptor_sized(mem, at, access, true);
}

fn write_flat_descriptor_sized(mem: &mut [u8], at: u32, access: u8, big: bool) {
    let at = at as usize;
    let limit_low: u16 = 0xffff;
    let limit_high_nibble: u8 = 0x0f;
    let d_bit: u8 = if big { 1 } else { 0 };
    let g_and_d: u8 = 0b1000 | (d_bit << 2);
    mem[at] = (limit_low & 0xff) as u8;
    mem[at + 1] = (limit_low >> 8) as u8;
    mem[at + 2] = 0;
    mem[at + 3] = 0;
    mem[at + 4] = 0;
    mem[at + 5] = access;
    mem[at + 6] = (g_and_d << 4) | limit_high_nibble;
    mem[at + 7] = 0;
}

fn write_interrupt_gate(mem: &mut [u8], vector: u8, offset: u32) {
    let at = (IDT_BASE + u32::from(vector) * 8) as usize;
    const ACCESS: u8 = 0x8e;
    mem[at] = (offset & 0xff) as u8;
    mem[at + 1] = ((offset >> 8) & 0xff) as u8;
    mem[at + 2] = (CODE_SELECTOR & 0xff) as u8;
    mem[at + 3] = (CODE_SELECTOR >> 8) as u8;
    mem[at + 4] = 0;
    mem[at + 5] = ACCESS;
    mem[at + 6] = ((offset >> 16) & 0xff) as u8;
    mem[at + 7] = ((offset >> 24) & 0xff) as u8;
}

struct FlatMemBus {
    mem: Vec<u8>,
    /// `Some(allowance)` arms `reflected_call_gate` with that many CPU clocks of room;
    /// `None` (the DEFAULT, and what every pre-existing test in this file gets) leaves
    /// the trait's own refuse-by-default answer standing, which is the property
    /// `an_unarmed_test_double_can_never_fake_a_hit` pins.
    gate_allowance: Option<u64>,
    /// Every `interrupt_acknowledge` this bus saw, in order -- the replayed nested acks
    /// land here, so their ORDER and their `(vector, ax)` pairs are assertable.
    acks: Vec<(u8, u16)>,
    /// What `reflected_call_commit_bus` was handed, and whether
    /// `note_reflected_call_answered` fired.
    committed_bus: u64,
    answered_flag: bool,
    /// The answer's observer test (screen 7) asks this; `false` admits the Class R skip.
    dma_visible: bool,
    /// Test-controllable cumulative raw bus clock counter (Fable review 2026-09-03, finding
    /// 1's regression test): unlike `in_batch_raw_bus_clocks`, this must NEVER reset, even
    /// across a simulated machine-batch re-entry -- the test drives it directly to prove the
    /// memo samples `cumulative_raw_bus_clocks`, not the batch-relative figure.
    bus_clock: u64,
}

impl FlatMemBus {
    fn new(size: usize) -> Self {
        Self {
            mem: vec![0u8; size],
            bus_clock: 0,
            gate_allowance: None,
            acks: Vec::new(),
            committed_bus: 0,
            answered_flag: false,
            dma_visible: false,
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

    fn interrupt_acknowledge(&mut self, vector: u8, ax: u16) -> Result<(), izarravm_bus::BusError> {
        self.acks.push((vector, ax));
        Ok(())
    }

    fn cumulative_raw_bus_clocks(&self) -> u64 {
        self.bus_clock
    }

    fn reflected_call_gate(
        &self,
        req: &izarravm_bus::ReflectedCallGateRequest,
    ) -> Result<(), izarravm_bus::ReflectedCallDecline> {
        let Some(allowance) = self.gate_allowance else {
            return Err(izarravm_bus::ReflectedCallDecline::NotArmed);
        };
        if req.scaled_core_clocks + req.raw_bus_clocks > allowance {
            return Err(izarravm_bus::ReflectedCallDecline::DeviceEdge);
        }
        Ok(())
    }

    fn reflected_call_commit_bus(&mut self, raw: u64) -> u64 {
        self.committed_bus += raw;
        self.bus_clock += raw;
        raw
    }

    fn reflected_call_dma_visible(&self, _lo: u32, _hi: u32) -> bool {
        self.dma_visible
    }

    fn note_reflected_call_answered(&mut self) {
        self.answered_flag = true;
    }
}

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
    cpu.control.cr0 |= CR0_PE;
    cpu.cpl = 0;
    cpu.gdtr = DescriptorTable {
        base: GDT_BASE,
        limit: 0x17,
    };
    cpu.idtr = DescriptorTable {
        base: IDT_BASE,
        limit: 0x07ff,
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

fn arm(cpu: &mut CpuGsw) {
    cpu.reflected_call = Some(Box::new(ReflectedCallMemoState::default()));
}

fn blank_entry_image() -> EntryImage {
    EntryImage {
        eax: 0,
        ebx: 0,
        ecx: 0,
        edx: 0,
        esp: 0,
        ebp: 0,
        esi: 0,
        edi: 0,
        eflags_masked: 0,
        cs: CachedSegment::default(),
        ss: CachedSegment::default(),
        ds: CachedSegment::default(),
        es: CachedSegment::default(),
        fs: CachedSegment::default(),
        gs: CachedSegment::default(),
        cr0: 0,
        cr3: 0,
        cr4: 0,
        cpl: 0,
        vm: false,
        idtr_base: 0,
        idtr_limit: 0,
        gdtr_base: 0,
        gdtr_limit: 0,
        ldtr_selector: 0,
        ldtr_base: 0,
        ldtr_limit: 0,
        ldtr_access: 0,
        tr_selector: 0,
        tr_base: 0,
        tr_limit: 0,
        tr_access: 0,
        dr7: 0,
    }
}

// ---------------------------------------------------------------------------
// Test 1: a RETF-with-flags return closes as a return match.
// ---------------------------------------------------------------------------

/// **Mutation bite**: delete the `RETF`-with-flags arm in `is_return_match`
/// (make it require `sp_here == entry_sp` only) and this test fails: 0b
/// defect D1 reappears and no `AH=0Bh`-shaped trip (handler returns by
/// `RETF`, leaving the `INT`-pushed FLAGS word on the stack) is ever
/// recognised as a match.
#[test]
fn a_retf_return_leaving_the_flags_word_closes_as_a_return_match() {
    let (mut cpu, _bus) = synthetic_reflected_client();
    let open = OpenTrip {
        key: MemoKey {
            epoch: 1,
            vector: VECTOR,
            ax: 0,
            cs_selector: CODE_SELECTOR,
            int_eip: CLIENT_RETURN_EIP,
            ss_selector: DATA_SELECTOR,
            ss_big: true,
            cpl: 0,
            vm: false,
        },
        slot: Slot::Warm,
        entry_image: blank_entry_image(),
        return_cs_selector: CODE_SELECTOR,
        return_eip: CLIENT_RETURN_EIP,
        entry_ss_selector: DATA_SELECTOR,
        entry_esp: STACK_TOP,
        entry_ss_big: true,
        entry_persona: cpu.persona(),
        open_elapsed_clocks: 0,
        open_timing_rem: 0,
        open_instructions: 0,
        open_bus_raw: 0,
        stacks: [None; MAX_STACK_SEGMENTS],
        stack_segments_over_cap: false,
        journaling: false,
        reads: HashMap::new(),
        writes: HashMap::new(),
        translations: HashMap::new(),
        read_set_over_cap: false,
        translation_set_over_cap: false,
        hazard: None,
        nested_int_count: 0,
        nested_acks: Vec::new(),
        nested_acks_over_cap: false,
        control_effects: Vec::new(),
        control_effects_over_cap: false,
        hw_interrupt_seen: false,
        entry_tail: None,
    };
    // The RETF popped only CS:IP, leaving the `INT`-pushed FLAGS word:
    // SP == entry SP - 2 (a 32-bit stack, so 4 bytes actually pop for a
    // dword IRET-shape return but only 2 for a 16-bit RETF pop -- the
    // synthetic client here uses a 32-bit stack and models the FLAGS word as
    // 2 bytes per the plan's own "-2" constant, which is generic (the
    // client's own operand size, not hardcoded per-vector).
    cpu.registers.set_esp(STACK_TOP - 2);
    let m = open.is_return_match(&cpu);
    assert_eq!(m, Some(true), "must recognise the RETF-with-flags arm");
}

// ---------------------------------------------------------------------------
// Test 2: an IRET return with SP == entry closes as a return match.
// ---------------------------------------------------------------------------

/// **Mutation bite**: require the `-2` arm always (delete the `sp_here ==
/// entry_sp` branch) and this test fails: an `INT 33h`-shaped trip (which
/// returns by `IRET`, SP == entry SP exactly) would never match.
#[test]
fn an_iret_return_with_sp_equal_to_entry_closes_as_a_return_match() {
    let (cpu, _bus) = synthetic_reflected_client();
    let open = test_open_trip(&cpu);
    let m = open.is_return_match(&cpu);
    assert_eq!(
        m,
        Some(false),
        "SP == entry SP must match with no flags word left behind"
    );
}

fn test_open_trip(cpu: &CpuGsw) -> OpenTrip {
    OpenTrip {
        key: MemoKey {
            epoch: 1,
            vector: VECTOR,
            ax: 0,
            cs_selector: CODE_SELECTOR,
            int_eip: CLIENT_RETURN_EIP,
            ss_selector: DATA_SELECTOR,
            ss_big: true,
            cpl: 0,
            vm: false,
        },
        slot: Slot::Warm,
        entry_image: blank_entry_image(),
        return_cs_selector: CODE_SELECTOR,
        return_eip: CLIENT_RETURN_EIP,
        entry_ss_selector: DATA_SELECTOR,
        entry_esp: STACK_TOP,
        entry_ss_big: true,
        entry_persona: cpu.persona(),
        open_elapsed_clocks: 0,
        open_timing_rem: 0,
        open_instructions: 0,
        open_bus_raw: 0,
        stacks: [None; MAX_STACK_SEGMENTS],
        stack_segments_over_cap: false,
        journaling: false,
        reads: HashMap::new(),
        writes: HashMap::new(),
        translations: HashMap::new(),
        read_set_over_cap: false,
        translation_set_over_cap: false,
        hazard: None,
        nested_int_count: 0,
        nested_acks: Vec::new(),
        nested_acks_over_cap: false,
        control_effects: Vec::new(),
        control_effects_over_cap: false,
        hw_interrupt_seen: false,
        entry_tail: None,
    }
}

// ---------------------------------------------------------------------------
// Test 3: rule 2 (frame-gone) and rule 3 (re-entry) never produce a "learned"
// outcome -- they close the trip, but only as `closed_without_return`.
// ---------------------------------------------------------------------------

/// **Mutation bite**: let a frame-gone or re-entry close increment `learned`
/// (or skip incrementing `learn_refused[closed_without_return]`): a memo
/// captured in a different activation of the same code (review A.3).
#[test]
fn frame_gone_and_re_entry_never_produce_a_memo() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    arm(&mut cpu);
    let key = key_for(&cpu, VECTOR, 0);
    // Frame-gone: CS/SS match the entry but SP has moved PAST it.
    let open = test_open_trip(&cpu);
    cpu.reflected_call.as_mut().unwrap().open = Some(open);
    let ss = cpu.registers.segment(SegmentIndex::Ss);
    let regs = &cpu.registers;
    let _ = (ss, regs);
    cpu.registers.set_esp(STACK_TOP + 4); // past the entry SP: frame gone
    on_far_transfer(&mut cpu, &bus);
    let ks = cpu
        .reflected_call
        .as_ref()
        .unwrap()
        .keys
        .get(&key)
        .expect("recorded");
    assert_eq!(ks.learned, 0);
    assert_eq!(
        ks.learn_refused[LearnRefused::ClosedWithoutReturn.index()],
        1
    );

    // Re-entry: a fresh INT with the same signature and SP back at entry.
    let mut cpu2 = synthetic_reflected_client().0;
    arm(&mut cpu2);
    let open2 = test_open_trip(&cpu2);
    cpu2.reflected_call.as_mut().unwrap().open = Some(open2);
    cpu2.registers.set_esp(STACK_TOP); // back at entry: re-entry shape
    let _ = on_int(&mut cpu2, &mut bus, VECTOR);
    let ks2 = cpu2
        .reflected_call
        .as_ref()
        .unwrap()
        .keys
        .get(&key_for(&cpu2, VECTOR, 0))
        .expect("recorded");
    assert_eq!(ks2.learned, 0);
    assert_eq!(
        ks2.learn_refused[LearnRefused::ClosedWithoutReturn.index()],
        1
    );
}

// ---------------------------------------------------------------------------
// Test 8: a write whose post-value varies between trips 2 and 3 is Class N
// (`learn_refused[write_class_n]`), even though the STRUCTURE (address set)
// agrees.
// ---------------------------------------------------------------------------

/// **Mutation bite**: learn from one trip (skip the trip-2-vs-3 compare
/// entirely): a per-trip host counter frozen by the memo (design 4.9's
/// hole) would be silently treated as deterministic.
#[test]
fn a_write_whose_post_value_varies_between_trips_two_and_three_is_class_n() {
    let addr = 0x9000u32;
    let baseline = JournalSnapshot {
        entry_image: blank_entry_image(),
        reads: HashMap::new(),
        translations: HashMap::new(),
        writes: HashMap::from([(
            addr,
            WriteObs {
                linear: addr,
                ss_selector: DATA_SELECTOR,
                pinned_pre: None,
                latest: 1,
                class: AddressClass::Other,
                phys_addr: addr,
                width_bytes: 4,
            },
        )]),
        insns: 10,
        exit_image: blank_entry_image(),
    };
    let mut writes_b = HashMap::new();
    writes_b.insert(
        addr,
        WriteObs {
            linear: addr,
            ss_selector: DATA_SELECTOR,
            pinned_pre: None,
            latest: 2, // differs from trip A's 1
            class: AddressClass::Other,
            phys_addr: addr,
            width_bytes: 4,
        },
    );
    let result = compare_journal(
        &baseline,
        &blank_entry_image(),
        &writes_b,
        &HashMap::new(),
        &HashMap::new(),
        10,
        &blank_entry_image(),
    );
    assert_eq!(result, Err(LearnRefused::WriteClassN));
}

// ---------------------------------------------------------------------------
// New test: a write of a constant the trip never read is Class W, not
// Class R (R2.3's fix).
// ---------------------------------------------------------------------------

/// **Mutation bite**: drop the pinned-pre-value requirement (classify any
/// `mask_to_width(latest) == mask_to_width(pre)` as R regardless of whether
/// the trip ever read the address): a coincidental restoration diverges
/// silently at an eventual answer time.
#[test]
fn a_write_of_a_constant_the_trip_never_read_is_class_w_not_class_r() {
    let (cpu, _bus) = synthetic_reflected_client();
    let open = test_open_trip(&cpu);
    let mut key_state = KeyState::default();
    let mut writes = HashMap::new();
    writes.insert(
        0x9000u32,
        WriteObs {
            linear: 0x9000,
            ss_selector: DATA_SELECTOR,
            pinned_pre: None, // never read before the write: NOT pinned
            latest: 0x1234,
            class: AddressClass::Other,
            phys_addr: 0x9000,
            width_bytes: 4,
        },
    );
    tally_write_classes(&mut key_state, &open, &writes);
    assert_eq!(
        key_state.write_class_r_pinned, 0,
        "must not count as pinned-restored"
    );
    assert_eq!(
        key_state.write_class_r_unpinned, 1,
        "an unpinned write is Class W (deterministic), not Class R"
    );
}

// ---------------------------------------------------------------------------
// New test: the raw-clock recovery is exact even when the forward-scaled
// total is not a multiple of the scaler's denominator on every step.
// ---------------------------------------------------------------------------

/// **Mutation bite**: feed the SCALED (post-`scale_clocks`) total to the
/// recovery formula instead of the raw one, or drop the `rem_after -
/// rem_before` correction: a silent half-charge that also breaks
/// `timing_rem`'s carry (finding 2's double-scale).
#[test]
fn the_charged_total_and_timing_rem_match_the_real_trip_when_raw_is_not_a_multiple_of_the_denominator()
 {
    // I386: level_timing = (num=2, den=5) -- the one persona where `num` is
    // not 1, so the forward scaling actually carries a remainder step to
    // step (I486/I586 have num=1, where every raw*num is trivially a
    // multiple of nothing interesting).
    let persona = CpuPersona::I386;
    let (num, den) = crate::level_timing(persona);
    assert_eq!((num, den), (2, 5));

    // Two instructions inside one "trip": raw 7 then raw 3, opening with
    // rem_before = 0.
    let rem0 = 0u64;
    let scaled1 = 7u64 * u64::from(num) + rem0;
    let charged1 = scaled1 / u64::from(den);
    let rem1 = scaled1 % u64::from(den);
    assert_eq!((charged1, rem1), (2, 4), "14/5 = 2 remainder 4");

    let scaled2 = 3u64 * u64::from(num) + rem1;
    let charged2 = scaled2 / u64::from(den);
    let rem2 = scaled2 % u64::from(den);
    assert_eq!((charged2, rem2), (2, 0), "(6+4)/5 = 2 remainder 0");

    let open_elapsed = 1_000u64;
    let close_elapsed = open_elapsed + charged1 + charged2;
    let recovered =
        recover_raw_core_clocks(open_elapsed, rem0, close_elapsed, rem2, persona, persona);
    assert_eq!(
        recovered,
        Some(10),
        "must recover the exact raw total (7 + 3), not the scaled one (4)"
    );
}

// ---------------------------------------------------------------------------
// Test 13: port I/O, RDTSC/RDMSR, x87 and a pending soft INT inside a trip
// all refuse.
// ---------------------------------------------------------------------------

/// **Mutation bite**: delete any one of the four `refuse_open` call sites
/// (or the enum arm it feeds) and the corresponding assertion below fails.
#[test]
fn port_io_rdtsc_x87_and_a_pending_soft_int_inside_a_trip_all_refuse() {
    let cases = [
        (LearnRefused::PortIo, note_port_io as fn(&mut CpuGsw)),
        (LearnRefused::X87, note_x87 as fn(&mut CpuGsw)),
        (
            LearnRefused::NondeterministicRead,
            note_rdtsc_or_rdmsr as fn(&mut CpuGsw),
        ),
    ];
    for (reason, hook) in cases {
        let (mut cpu, bus) = synthetic_reflected_client();
        arm(&mut cpu);
        let open = test_open_trip(&cpu);
        cpu.reflected_call.as_mut().unwrap().open = Some(open);
        hook(&mut cpu);
        // Close the trip as a (non-)match; the hazard must dominate.
        on_far_transfer(&mut cpu, &bus); // no-op close path (not frame-gone) --
        // force the close directly instead, since the synthetic client's SP
        // has not moved:
        let open = cpu.reflected_call.as_mut().unwrap().open.take();
        if let Some(open) = open {
            finish_trip(&mut cpu, &bus, open, true);
        }
        let key = key_for(&cpu, VECTOR, 0);
        let ks = cpu
            .reflected_call
            .as_ref()
            .unwrap()
            .keys
            .get(&key)
            .expect("recorded");
        assert_eq!(
            ks.learn_refused[reason.index()],
            1,
            "{:?} must refuse the learn attempt",
            reason
        );
        assert_eq!(ks.learned, 0);
    }

    // Pending soft INT: no identified production seam (documented scope
    // cut); exercised via the test-only injector.
    let (mut cpu, bus) = synthetic_reflected_client();
    arm(&mut cpu);
    let open = test_open_trip(&cpu);
    cpu.reflected_call.as_mut().unwrap().open = Some(open);
    test_force_refuse(&mut cpu, LearnRefused::PendingSoftInt);
    let open = cpu.reflected_call.as_mut().unwrap().open.take().unwrap();
    finish_trip(&mut cpu, &bus, open, true);
    let key = key_for(&cpu, VECTOR, 0);
    let ks = cpu
        .reflected_call
        .as_ref()
        .unwrap()
        .keys
        .get(&key)
        .expect("recorded");
    assert_eq!(ks.learn_refused[LearnRefused::PendingSoftInt.index()], 1);
    assert_eq!(ks.learned, 0);
}

// ---------------------------------------------------------------------------
// Test 14: the knob spelling table.
// ---------------------------------------------------------------------------

/// **Mutation bite**: make `""` mean ON (or make `"0"`/`"off"` panic, or
/// accept an unlisted spelling silently): a mistyped ladder leg would
/// silently run the wrong arm.
#[test]
fn the_knob_spelling_table() {
    assert!(!parse_reflected_call_memo_arm(Err(
        std::env::VarError::NotPresent
    )));
    assert!(!parse_reflected_call_memo_arm(Ok(String::new())));
    assert!(!parse_reflected_call_memo_arm(Ok("0".to_string())));
    assert!(!parse_reflected_call_memo_arm(Ok("off".to_string())));
    assert!(parse_reflected_call_memo_arm(Ok("1".to_string())));
    assert!(parse_reflected_call_memo_arm(Ok("on".to_string())));
    assert!(parse_reflected_call_memo_arm(Ok("memo".to_string())));

    let result =
        std::panic::catch_unwind(|| parse_reflected_call_memo_arm(Ok("bogus".to_string())));
    assert!(
        result.is_err(),
        "an unrecognised spelling must panic, not default silently"
    );
}

// ---------------------------------------------------------------------------
// Test 15: the off arm is bit-identical -- no state moves when the knob is
// off.
// ---------------------------------------------------------------------------

/// **Mutation bite**: any unconditional state write on the OFF arm (e.g.
/// setting `reflected_call_journal` outside the `is_some()` guard): the OFF
/// arm's counter surface would no longer be all-zero, and a real run would
/// pay cost for an instrument the operator turned off.
#[test]
fn the_off_arm_is_bit_identical() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    assert!(
        cpu.reflected_call.is_none(),
        "unarmed by default in a fresh CpuGsw literal"
    );
    assert!(!cpu.reflected_call_journal);

    cpu.software_interrupt(&mut bus, VECTOR)
        .expect("delivery must succeed");
    assert!(
        cpu.reflected_call.is_none(),
        "the knob being off must never allocate the memo state"
    );
    assert!(!cpu.reflected_call_journal);

    cpu.iret(&mut bus, OperandSize::Dword)
        .expect("IRET must succeed");
    assert!(cpu.reflected_call.is_none());
    assert!(!cpu.reflected_call_journal);
}

// ---------------------------------------------------------------------------
// A20 hook is a documented no-op in this commit (deliberate scope cut); a
// smoke test that it does not panic when called, so a future slice's wiring
// starts from a known-good baseline.
// ---------------------------------------------------------------------------

#[test]
fn maybe_compare_bench_is_off_by_default_and_does_not_panic() {
    // No env var set by default in a test process; must be a silent no-op.
    maybe_run_compare_bench();
    let (ns_per_read, ns_total) = run_compare_bench(155);
    assert!(ns_per_read >= 0.0);
    assert!(ns_total >= 0.0);
}

// ---------------------------------------------------------------------------
// Fable review 2026-09-03, finding 1: raw bus clocks must be recovered from
// a cumulative (whole-run) counter, never a per-batch one that resets at
// every IF-edge machine-batch re-entry.
// ---------------------------------------------------------------------------

#[test]
fn raw_bus_clocks_recover_correctly_across_a_simulated_batch_restart() {
    let open_bus_raw = 1_000_000u64;
    let close_bus_raw = 1_000_150u64;
    assert_eq!(
        recover_raw_bus_clocks(open_bus_raw, close_bus_raw),
        Some(150)
    );

    let open_bus_raw_2 = 500u64;
    let close_bus_raw_2 = 500u64;
    assert_eq!(
        recover_raw_bus_clocks(open_bus_raw_2, close_bus_raw_2),
        Some(0)
    );
}

#[test]
fn open_trip_samples_the_cumulative_bus_accessor() {
    let (cpu, mut bus) = synthetic_reflected_client();
    bus.bus_clock = 42;
    let key = MemoKey {
        epoch: 1,
        vector: VECTOR,
        ax: 0,
        cs_selector: CODE_SELECTOR,
        int_eip: CLIENT_RETURN_EIP,
        ss_selector: DATA_SELECTOR,
        ss_big: true,
        cpl: 0,
        vm: false,
    };
    let open = OpenTrip::start(&cpu, &bus, key, Slot::Warm);
    assert_eq!(open.open_bus_raw, 42);
    bus.bus_clock = 42 + 12 + 8 + 108;
    assert_eq!(
        recover_raw_bus_clocks(open.open_bus_raw, bus.cumulative_raw_bus_clocks()),
        Some(128)
    );
}

// ---------------------------------------------------------------------------
// Fable review 2026-09-03, finding 2 (plan section 4.2 point 2): only
// `clocks_unstable` counts toward the consecutive-failure disarm budget.
// ---------------------------------------------------------------------------

#[test]
fn journal_mismatch_and_boundary_refusals_do_not_disarm_only_clocks_unstable_does() {
    let mut ks = KeyState::default();
    for _ in 0..10 {
        ks.record_failure(LearnRefused::JournalMismatch);
    }
    assert!(!ks.disarmed, "journal_mismatch must never disarm the key");
    assert_eq!(ks.learn_refused[LearnRefused::JournalMismatch.index()], 10);

    let mut ks2 = KeyState::default();
    for _ in 0..10 {
        ks2.record_failure(LearnRefused::ClosedWithoutReturn);
    }
    assert!(
        !ks2.disarmed,
        "a boundary refusal must never disarm the key"
    );

    let mut ks3 = KeyState::default();
    for _ in 0..(MEMO_LEARN_BUDGET - 1) {
        ks3.record_failure(LearnRefused::ClocksUnstable);
    }
    assert!(
        !ks3.disarmed,
        "must not disarm before the budget is reached"
    );
    ks3.record_failure(LearnRefused::ClocksUnstable);
    assert!(
        ks3.disarmed,
        "MEMO_LEARN_BUDGET consecutive clocks_unstable refusals must disarm"
    );

    let mut ks4 = KeyState::default();
    ks4.record_failure(LearnRefused::ClocksUnstable);
    ks4.record_failure(LearnRefused::JournalMismatch);
    ks4.record_success_and_reset();
    ks4.record_failure(LearnRefused::ClocksUnstable);
    ks4.record_failure(LearnRefused::ClocksUnstable);
    ks4.record_failure(LearnRefused::ClocksUnstable);
    assert!(
        !ks4.disarmed,
        "the reset-by-success streak must not carry across an intervening success"
    );
}

// ---------------------------------------------------------------------------
// Fable review 2026-09-03, finding 5: Class D must be tested against the SS
// selector actually in force at the write, and HostStack writes are
// eligible for Class D exactly like ClientStack ones.
// ---------------------------------------------------------------------------

const HOST_SS_SELECTOR: u16 = 0x30;

#[test]
fn a_write_on_the_hosts_own_stack_segment_is_eligible_for_class_d() {
    let (cpu, _bus) = synthetic_reflected_client();
    let mut open = test_open_trip(&cpu);
    open.stacks[1] = Some(StackTrack {
        selector: HOST_SS_SELECTOR,
        base: 0x9000,
        limit: 0xffff,
        low_water_esp: 0x9100,
        last_esp: 0x9200,
    });
    let mut key_state = KeyState::default();
    let mut writes = HashMap::new();
    writes.insert(
        0x9080u32, // below low_water_esp (0x9100): a deeper push than ever reached again
        WriteObs {
            linear: 0x9080,
            ss_selector: HOST_SS_SELECTOR,
            pinned_pre: None,
            latest: 0xdead_beef,
            class: AddressClass::HostStack,
            phys_addr: 0x9080,
            width_bytes: 4,
        },
    );
    tally_write_classes(&mut key_state, &open, &writes);
    assert_eq!(
        key_state.write_class_d, 1,
        "a HostStack write below its OWN segment's low-water mark must classify Dead"
    );
    assert_eq!(key_state.write_class_r_pinned, 0);
    assert_eq!(key_state.write_class_r_unpinned, 0);
    let _ = &mut open;
}

#[test]
fn a_live_host_stack_write_above_the_low_water_mark_is_not_class_d() {
    let (cpu, _bus) = synthetic_reflected_client();
    let mut open = test_open_trip(&cpu);
    open.stacks[1] = Some(StackTrack {
        selector: HOST_SS_SELECTOR,
        base: 0x9000,
        limit: 0xffff,
        low_water_esp: 0x9100,
        last_esp: 0x9200,
    });
    let mut key_state = KeyState::default();
    let mut writes = HashMap::new();
    writes.insert(
        0x9150u32,
        WriteObs {
            linear: 0x9150,
            ss_selector: HOST_SS_SELECTOR,
            pinned_pre: None,
            latest: 0x1234,
            class: AddressClass::HostStack,
            phys_addr: 0x9150,
            width_bytes: 4,
        },
    );
    tally_write_classes(&mut key_state, &open, &writes);
    assert_eq!(key_state.write_class_d, 0);
    assert_eq!(key_state.write_class_r_unpinned, 1);
    let _ = &mut open;
}

// ---------------------------------------------------------------------------
// Fable review 2026-09-03, finding 5 (the aperture/device-window classes):
// classify_write must actually return them.
// ---------------------------------------------------------------------------

#[test]
fn classify_write_returns_the_two_never_restored_device_classes() {
    let (cpu, bus) = synthetic_reflected_client();
    let fb_physical = crate::reflected_call::FRAMEBUFFER_APERTURE_LO + 0x100;
    let class = classify_write(
        &cpu,
        &bus,
        fb_physical,
        fb_physical,
        DATA_SELECTOR,
        DATA_SELECTOR,
    );
    assert_eq!(class, AddressClass::FramebufferAperture);
    assert!(class.never_restored());

    let unmapped_physical = (MEM_SIZE as u32) + 0x1000;
    let class2 = classify_write(
        &cpu,
        &bus,
        unmapped_physical,
        unmapped_physical,
        DATA_SELECTOR,
        DATA_SELECTOR,
    );
    assert_eq!(class2, AddressClass::NotPlainRam);
    assert!(class2.never_restored());
}

// ---------------------------------------------------------------------------
// Fable review 2026-09-03, finding 6(iii): trips_seen/disarmed_returns must
// be visible per key.
// ---------------------------------------------------------------------------

#[test]
fn disarmed_key_still_counts_trips_seen_and_disarmed_returns() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    arm(&mut cpu);
    let key = key_for(&cpu, VECTOR, 0);
    {
        let state = cpu.reflected_call.as_mut().unwrap();
        state.keys.entry(key).or_default().disarmed = true;
    }
    let _ = on_int(&mut cpu, &mut bus, VECTOR);
    let state = cpu.reflected_call.as_ref().unwrap();
    let ks = state.keys.get(&key).expect("recorded");
    assert_eq!(ks.trips_seen, 1);
    assert_eq!(ks.disarmed_returns, 1);
    assert!(state.open.is_none());
}

// ---------------------------------------------------------------------------
// Fable re-review 2026-09-03, nit (i): a hardware interrupt inside a trip
// must report as `hardware_interrupt`, not whatever hazard its own EOI
// (an `OUT`) happens to also set.
// ---------------------------------------------------------------------------

/// **Mutation bite**: swap the order back (test `open.hazard` before
/// `open.hw_interrupt_seen`) and this test fails: the trip reports
/// `port_io` instead of `hardware_interrupt`, exactly the defect the
/// re-review traced (447 misattributed refusals on one recipe-A run).
#[test]
fn a_hardware_interrupt_inside_a_trip_reports_as_hardware_interrupt_not_port_io() {
    let (mut cpu, bus) = synthetic_reflected_client();
    arm(&mut cpu);
    let mut open = test_open_trip(&cpu);
    open.hw_interrupt_seen = true;
    open.hazard = Some(LearnRefused::PortIo); // the IRQ's own EOI, an OUT
    finish_trip(&mut cpu, &bus, open, true);
    let key = key_for(&cpu, VECTOR, 0);
    let ks = cpu
        .reflected_call
        .as_ref()
        .unwrap()
        .keys
        .get(&key)
        .expect("recorded");
    assert_eq!(ks.learn_refused[LearnRefused::HardwareInterrupt.index()], 1);
    assert_eq!(ks.learn_refused[LearnRefused::PortIo.index()], 0);
}

// ---------------------------------------------------------------------------
// Fable re-review 2026-09-03, campaign verdict (2c): a disarmed key must
// re-arm after MEMO_REARM_TRIPS_SEEN more trips are seen, so a budget spent
// entirely in a menu phase cannot blind the dwell for the rest of the run.
// ---------------------------------------------------------------------------

/// **Mutation bite**: delete `maybe_rearm`'s call from `on_int` (or its threshold check): a
/// key disarmed early (e.g. in a menu phase) would drop every remaining trip for the rest of
/// the run, exactly INT 33h's `0x0003` key's fate before this fix.
#[test]
fn a_disarmed_key_rearms_after_trips_seen_advances_by_the_rearm_window() {
    let mut ks = KeyState::default();
    for _ in 0..MEMO_LEARN_BUDGET {
        ks.trips_seen += 1;
        ks.record_failure(LearnRefused::ClocksUnstable);
    }
    assert!(ks.disarmed);
    let disarmed_at = ks.trips_seen;

    // Just under the window: must stay disarmed.
    ks.trips_seen = disarmed_at + MEMO_REARM_TRIPS_SEEN - 1;
    ks.maybe_rearm();
    assert!(
        ks.disarmed,
        "must not re-arm before the full window has elapsed"
    );
    assert_eq!(ks.rearms, 0);

    // At the window: must re-arm, resetting the streak and slot.
    ks.trips_seen = disarmed_at + MEMO_REARM_TRIPS_SEEN;
    ks.maybe_rearm();
    assert!(!ks.disarmed);
    assert_eq!(ks.rearms, 1);
    assert_eq!(ks.slot, SlotState::Warm);
}

/// Integration-level companion: a key disarmed through the real `on_int` hook re-arms through
/// the real hook too, once `trips_seen` (bumped every occurrence, disarmed or not) reaches the
/// window.
#[test]
fn on_int_rearms_a_disarmed_key_through_the_real_hook() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    arm(&mut cpu);
    let key = key_for(&cpu, VECTOR, 0);
    {
        let state = cpu.reflected_call.as_mut().unwrap();
        let ks = state.keys.entry(key).or_default();
        ks.trips_seen = 1_000_000;
        // Disarm through the real path (four consecutive ClocksUnstable failures), so
        // `disarmed_at_trips_seen` is stamped exactly as production code stamps it.
        for _ in 0..MEMO_LEARN_BUDGET {
            ks.record_failure(LearnRefused::ClocksUnstable);
        }
        assert!(
            ks.disarmed,
            "test setup: four consecutive failures must disarm"
        );
    }
    let _ = on_int(&mut cpu, &mut bus, VECTOR); // trips_seen -> 1,000,001: still far short of the window
    {
        let ks = cpu.reflected_call.as_ref().unwrap().keys.get(&key).unwrap();
        assert!(
            ks.disarmed,
            "must still be disarmed one trip after the disarm"
        );
    }
    // Fast-forward trips_seen past the window and drive one more occurrence.
    {
        let state = cpu.reflected_call.as_mut().unwrap();
        let ks = state.keys.get_mut(&key).unwrap();
        ks.trips_seen = 1_000_001 + MEMO_REARM_TRIPS_SEEN;
    }
    let _ = on_int(&mut cpu, &mut bus, VECTOR);
    let ks = cpu.reflected_call.as_ref().unwrap().keys.get(&key).unwrap();
    assert!(
        !ks.disarmed,
        "must re-arm once trips_seen has advanced by the full window"
    );
    assert_eq!(ks.rearms, 1);
}

/// A memo learned under one guest-clock epoch must never answer under another.
///
/// The memo REPLAYS a trip's recorded `raw_core` and `raw_bus` instead of
/// re-running it, and slice 8 changes exactly the numbers a reflected trip is
/// made of -- the V86 monitor trip, the faulting instruction's own class,
/// `IRET`'s mode rows. Without the epoch in the key, a memo learned at epoch 1
/// would go on answering with a 16.7x-light trip after the model moved.
#[test]
fn the_epoch_is_part_of_the_memo_key() {
    let mut a = MemoKey {
        epoch: 1,
        vector: 0x21,
        ax: 0x3d00,
        cs_selector: 0x0170,
        int_eip: 0x0001_2340,
        ss_selector: 0x0178,
        ss_big: true,
        cpl: 3,
        vm: false,
    };
    let b = MemoKey { epoch: 2, ..a };
    assert_ne!(a, b, "two epochs must not share a memo bucket");

    // And the epoch is the ONLY thing separating them, so this is a test of
    // that field rather than of the seventeen bytes beside it.
    a.epoch = 2;
    assert_eq!(a, b);

    // Hash agreement follows from `Eq`, but the map is keyed on the hash, so
    // assert it where the map would see it.
    use std::collections::HashSet;
    let mut set = HashSet::new();
    set.insert(MemoKey { epoch: 1, ..b });
    set.insert(b);
    assert_eq!(set.len(), 2, "the two epochs must occupy two buckets");

// ---------------------------------------------------------------------------
// Amendment 1: A20 retire (plan Revision 2 amendments, item A, BLOCKING).
// ---------------------------------------------------------------------------

fn fake_memo() -> Arc<Memo> {
    Arc::new(Memo {
        image: blank_entry_image(),
        reads: Box::new([]),
        translations: Box::new([]),
        epilogue: blank_entry_image(),
        return_eip: CLIENT_RETURN_EIP,
        replay: Box::new([]),
        class_r_ranges: Box::new([]),
        raw_core_clocks: 1,
        raw_bus_clocks: 1,
        insns: 1,
        code_pages: Box::new([]),
        nested_acks: Box::new([]),
        control_effects: Box::new([]),
    })
}

/// **Mutation bite**: skip the retire call in `CpuGsw::note_a20_changed` (or leave the memo
/// cache untouched) and this test fails: a memo built while A20 was open (or closed) survives
/// a gate toggle and the answer path would go on comparing and replaying cells resolved under
/// the STALE gate state -- silent guest-memory corruption near the 1 MB wrap.
#[test]
fn an_a20_toggle_retires_every_memo() {
    let (mut cpu, _bus) = synthetic_reflected_client();
    arm(&mut cpu);
    let key_a = MemoKey {
        vector: VECTOR,
        ax: 0,
        cs_selector: CODE_SELECTOR,
        int_eip: CLIENT_RETURN_EIP,
        ss_selector: DATA_SELECTOR,
        ss_big: true,
        cpl: 0,
        vm: false,
    };
    let mut key_b = key_a;
    key_b.ax = 1;
    {
        let state = cpu.reflected_call.as_mut().unwrap();
        state.memos.insert(key_a, vec![fake_memo(), fake_memo()]);
        state.memos.insert(key_b, vec![fake_memo()]);
    }
    assert_eq!(
        cpu.reflected_call.as_ref().unwrap().a20_retires,
        0,
        "no toggle yet"
    );

    cpu.note_a20_changed();

    let state = cpu.reflected_call.as_ref().unwrap();
    assert!(
        state.memos.values().all(Vec::is_empty),
        "every key's memo list must be emptied by one A20 toggle"
    );
    assert_eq!(
        state.a20_retires, 3,
        "the counter must record the COUNT of memos retired, not the number of keys"
    );

    // A second toggle with an empty cache must not double-count or panic.
    cpu.note_a20_changed();
    assert_eq!(cpu.reflected_call.as_ref().unwrap().a20_retires, 3);
}

/// The A20 hook fires from `izarravm-machine`'s single production seam
/// (`izarravm-machine/src/run.rs:2023`) through `CpuGsw::note_a20_changed`; this test drives
/// that exact public method rather than a private helper, so a future refactor that stops
/// calling `retire_all_memos` from `note_a20_changed` (moving it to some other seam) is caught
/// here even though it would not be caught by a unit test against `retire_all_memos` alone.
#[test]
fn note_a20_changed_is_the_seam_that_retires() {
    let (mut cpu, _bus) = synthetic_reflected_client();
    arm(&mut cpu);
    let key = MemoKey {
        vector: VECTOR,
        ax: 0,
        cs_selector: CODE_SELECTOR,
        int_eip: CLIENT_RETURN_EIP,
        ss_selector: DATA_SELECTOR,
        ss_big: true,
        cpl: 0,
        vm: false,
    };
    cpu.reflected_call
        .as_mut()
        .unwrap()
        .memos
        .insert(key, vec![fake_memo()]);
    cpu.note_a20_changed();
    assert!(
        cpu.reflected_call
            .as_ref()
            .unwrap()
            .memos
            .get(&key)
            .unwrap()
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Amendment 2: the answer path skeleton and the fall-through invariant.
// ---------------------------------------------------------------------------

const CLASS_W_ADDR: u32 = 0x7000;

/// Drives one synthetic trip through the real hooks: `on_int` opens it, one write lands at
/// `CLASS_W_ADDR` (never read first, so it is Class W -- deterministic, not pinned-restored),
/// the `RETF`-with-flags shape closes it (`SP == entry SP - 2`). `journal` selects whether the
/// write is actually observed (matches `cpu.reflected_call_journal`, which `on_int` sets from
/// the key's own learn slot -- Warm trips never journal).
fn run_one_synthetic_trip(cpu: &mut CpuGsw, bus: &mut FlatMemBus, write_value: u32) {
    cpu.registers.set_esp(STACK_TOP);
    cpu.set_eip(CLIENT_RETURN_EIP);
    let _ = on_int(cpu, bus, VECTOR);
    bus.write_raw(CLASS_W_ADDR, BusWidth::Dword, write_value);
    note_write(
        cpu,
        bus,
        CLASS_W_ADDR,
        BusWidth::Dword,
        write_value,
        false,
        None,
    );
    // What real instruction execution would have charged between the `INT` and the return --
    // applied HERE, between open and close, is what the raw-clock recovery (an
    // open/close DELTA) actually measures.
    cpu.elapsed_clocks += 100;
    cpu.perf.instructions += 7;
    bus.bus_clock += 50;
    cpu.registers.set_esp(STACK_TOP - 2); // RETF-with-flags: leaves the FLAGS word behind.
    cpu.set_eip(CLIENT_RETURN_EIP);
    on_far_return(cpu, bus);
}

/// **Mutation bite**: skip the `MEMO_CLOCK_SAMPLES` natural-sample agreement requirement (arm
/// after Journal B alone): a memo could be built from a trip whose clocks never actually
/// stabilised, breaking the conservation identity the whole slice rests on.
#[test]
fn a_full_learn_cycle_produces_an_armed_memo_for_the_key() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    arm(&mut cpu);
    let key = key_for(&cpu, VECTOR, 0);

    // Trip 1: Warm (discarded). Trips 2-3: Journal A/B (must agree). Trips 4-11: Natural x8
    // (must all report identical raw clocks). Sixteen fixed, deterministic increments to
    // `elapsed_clocks`/`perf.instructions`/the bus clock between open and close, applied via
    // `run_one_synthetic_trip`'s bracketing below, standing in for what real instruction
    // execution would charge -- the SAME amount every trip is exactly what "the clocks are
    // stable" means.
    for _ in 0..11 {
        run_one_synthetic_trip(&mut cpu, &mut bus, 0xCAFEBABE);
    }

    let state = cpu.reflected_call.as_ref().unwrap();
    let ks = state.keys.get(&key).expect("key must be tracked");
    assert_eq!(ks.learned, 1, "one full learn cycle must have completed");
    let memos = state.memos.get(&key).expect("a memo must be cached");
    assert_eq!(memos.len(), 1);
    let memo = &memos[0];
    // The telescoping-carry scaler (R2.2/R2.15): `raw = den * elapsed_delta / num`. This test
    // drives the SAME 100-tick delta on every one of the 8 natural samples, so whatever the
    // scaler's ratio is for this persona, the recovered raw total must be identical across
    // all 8 and equal to this formula applied once -- the memo's own value proves it, since
    // `finish_trip` only reaches `Slot::Natural` success when all 8 samples agreed exactly.
    let (num, den) = crate::level_timing(cpu.persona());
    assert_eq!(memo.raw_core_clocks, 100 * u64::from(den) / u64::from(num));
    assert_eq!(memo.raw_bus_clocks, 50);
    assert_eq!(memo.insns, 7);
    assert_eq!(memo.return_eip, CLIENT_RETURN_EIP);
    assert!(
        memo.replay
            .iter()
            .any(|&(addr, width, value)| addr == CLASS_W_ADDR && width == 4 && value == 0xCAFEBABE),
        "the never-read write must be classified Class W and replayed verbatim: {:?}",
        memo.replay
    );
}

/// **Mutation bite**: skip the read-set compare in `screen_memo` (return `Ok(())` once the
/// entry image matches): the answer path would apply an epilogue built from a trip whose
/// inputs the CURRENT trip never actually presented -- the exact bug the plan's own worked
/// example (a re-vectored `INT`, or a pending key in the BDA tail) depends on this screen to
/// catch. This test drains the invariant to its purest form: a single poisoned read cell, with
/// nothing else about the machine touched, must be rejected before any state changes, and the
/// screening call itself must never write anything (registers, memory, or the memo cache).
#[test]
fn a_poisoned_read_cell_falls_through_with_zero_state_change() {
    let (cpu, mut bus) = synthetic_reflected_client();
    let addr = 0x7100u32;
    bus.write_raw(addr, BusWidth::Dword, 0x1111_1111);
    let memo = Memo {
        image: EntryImage::capture(&cpu),
        reads: Box::new([(addr, 0x1111_1111)]),
        translations: Box::new([]),
        epilogue: EntryImage::capture(&cpu),
        return_eip: CLIENT_RETURN_EIP,
        replay: Box::new([]),
        class_r_ranges: Box::new([]),
        raw_core_clocks: 42,
        raw_bus_clocks: 7,
        insns: 3,
        code_pages: Box::new([]),
        nested_acks: Box::new([]),
        control_effects: Box::new([]),
    };
    // Sanity: an UNPOISONED read set matches, so the screen would pass.
    assert_eq!(screen_memo(&cpu, &bus, &memo), Ok(()));

    // Poison the one read cell the memo depends on.
    bus.write_raw(addr, BusWidth::Dword, 0x2222_2222);
    let cpu_before = cpu.clone();
    let mem_before = bus.mem.clone();

    let result = screen_memo(&cpu, &bus, &memo);

    assert_eq!(result, Err(FellThrough::ReadSetMismatch));
    assert_eq!(cpu, cpu_before, "screening must never mutate CPU state");
    assert_eq!(bus.mem, mem_before, "screening must never mutate memory");
}

/// Same invariant, driven through the real `on_int` hook rather than calling `screen_memo`
/// directly, with a memo already cached for the key: proves the OBSERVATIONAL wiring in
/// `on_int` (amendment 2 does not yet apply anything) also never mutates guest state on a
/// screen miss, and that the right counter moves.
#[test]
fn on_int_screening_never_mutates_state_on_a_poisoned_cell() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    arm(&mut cpu);
    let key = key_for(&cpu, VECTOR, 0);
    let addr = 0x7100u32;
    bus.write_raw(addr, BusWidth::Dword, 0x1111_1111);
    let memo = Memo {
        image: EntryImage::capture(&cpu),
        reads: Box::new([(addr, 0x1111_1111)]),
        translations: Box::new([]),
        epilogue: EntryImage::capture(&cpu),
        return_eip: CLIENT_RETURN_EIP,
        replay: Box::new([]),
        class_r_ranges: Box::new([]),
        raw_core_clocks: 1,
        raw_bus_clocks: 1,
        insns: 1,
        code_pages: Box::new([]),
        nested_acks: Box::new([]),
        control_effects: Box::new([]),
    };
    cpu.reflected_call
        .as_mut()
        .unwrap()
        .memos
        .insert(key, vec![Arc::new(memo)]);
    bus.write_raw(addr, BusWidth::Dword, 0x2222_2222); // poison after caching the memo

    let regs_before = cpu.registers.clone();
    let mem_before = bus.mem.clone();
    let _ = on_int(&mut cpu, &mut bus, VECTOR);
    assert_eq!(
        cpu.registers, regs_before,
        "on_int must not move any register on a screen miss"
    );
    assert_eq!(
        bus.mem, mem_before,
        "on_int must not touch memory on a screen miss"
    );
    assert_eq!(
        cpu.reflected_call.as_ref().unwrap().read_set_mismatch,
        1,
        "the read-set-mismatch counter must have moved"
    );
    assert_eq!(cpu.reflected_call.as_ref().unwrap().would_answer, 0);
}

// ---------------------------------------------------------------------------
// Amendment 3: THE APPLY PATH (plan section 5.8 as amended by R2.16 item 2).
// ---------------------------------------------------------------------------

/// The scaler's `(num, den)` on the persona these fixtures run
/// (`GswMode::Gsw586` -> `(1, 12)`), so a test can state the expected charge as
/// arithmetic rather than as a copied constant.
fn scaler() -> (u64, u64) {
    let (num, den) = crate::level_timing(CpuPersona::I586);
    (u64::from(num), u64::from(den))
}

/// A memo whose ANSWER is distinguishable in every lane the apply path touches:
/// a different value in every GPR, a moved ESP, moved EFLAGS, a re-loaded DS, a
/// nested ack pair, a Class W replay write, a CR3 control effect, and a raw core
/// total that is deliberately NOT a multiple of the scaler denominator.
fn distinguishable_memo(cpu: &CpuGsw, replay_addr: u32, cr3_after: u32) -> Memo {
    let entry = EntryImage::capture(cpu);
    let mut epilogue = entry;
    epilogue.eax = 0xAAAA_0001;
    epilogue.ebx = 0xBBBB_0002;
    epilogue.ecx = 0xCCCC_0003;
    epilogue.edx = 0xDDDD_0004;
    epilogue.ebp = 0x0000_0BB0;
    epilogue.esi = 0x0000_0551;
    epilogue.edi = 0x0000_0DD1;
    // The `RETF`-with-flags shape: the answer's ESP is entry - 2, RECORDED not computed.
    epilogue.esp = entry.esp.wrapping_sub(2);
    // CF | ZF, both inside `EFLAGS_ARCH_MASK`.
    epilogue.eflags_masked = (entry.eflags_masked | 0x0041) & EFLAGS_ARCH_MASK;
    epilogue.ds.selector = CODE_SELECTOR;
    epilogue.ds.access = 0x9b;
    epilogue.cr3 = cr3_after;
    Memo {
        image: entry,
        reads: Box::new([]),
        translations: Box::new([]),
        epilogue,
        // `int_eip + insn_len`: DELIBERATELY not the EIP the fixture's CPU already
        // holds, or a mutation deleting the `set_eip` would pass unnoticed and the
        // real guest would re-execute the `INT`.
        return_eip: CLIENT_RETURN_EIP + 2,
        replay: Box::new([(replay_addr, 2, 0xBEEF)]),
        class_r_ranges: Box::new([(0x7000, 0x7003)]),
        // 1,000 is NOT a multiple of `den` (12) on this persona, so a committer that
        // fed a PRE-SCALED delta back through the scaler would leave a different
        // `timing_rem` and this fixture would see it (R2.11's test for finding 2).
        raw_core_clocks: 1_000,
        raw_bus_clocks: 500,
        insns: 1_579,
        code_pages: Box::new([]),
        nested_acks: Box::new([(0x16, 0x0100), (0x28, 0x0000)]),
        control_effects: Box::new([ControlEffect::Cr3Write(cr3_after)]),
    }
}

fn install_memo(cpu: &mut CpuGsw, key: MemoKey, memo: Memo) {
    cpu.reflected_call
        .as_mut()
        .unwrap()
        .memos
        .insert(key, vec![Arc::new(memo)]);
}

/// **Mutation bites, four, each verified red on its own**: (a) drop the epilogue's
/// segment restore -- DS keeps the caller's selector and the next reflected call runs
/// the DOS half against the wrong data segment (BLOCKING finding 1's shape); (b) drop
/// the `ESP` restore -- the `RETF`-with-flags SP delta is lost and the client stack
/// walks; (c) drop the EFLAGS restore -- the answer returns the wrong carry, which
/// `AH=0Bh` uses as its result; (d) drop the `set_eip` -- the guest re-executes the
/// `INT`.
#[test]
fn the_answer_reproduces_the_epilogue_registers_flags_segments_and_sp() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    arm(&mut cpu);
    bus.gate_allowance = Some(1_000_000);
    let key = key_for(&cpu, VECTOR, 0);
    let memo = distinguishable_memo(&cpu, 0x7200, cpu.control.cr3);
    let expected = memo.epilogue;
    install_memo(&mut cpu, key, memo);

    let outcome = on_int(&mut cpu, &mut bus, VECTOR).expect("no bus fault");
    assert_eq!(outcome, IntOutcome::Answered);

    let regs = &cpu.registers;
    assert_eq!(regs.eax(), expected.eax);
    assert_eq!(regs.ebx(), expected.ebx);
    assert_eq!(regs.ecx(), expected.ecx);
    assert_eq!(regs.edx(), expected.edx);
    assert_eq!(regs.ebp(), expected.ebp);
    assert_eq!(regs.esi(), expected.esi);
    assert_eq!(regs.edi(), expected.edi);
    assert_eq!(
        regs.esp(),
        expected.esp,
        "the recorded final SP, not a computed one"
    );
    assert_eq!(
        cpu.eflags() & EFLAGS_ARCH_MASK,
        expected.eflags_masked,
        "EFLAGS restored as a full architectural image, with the lazy descriptor torn down"
    );
    for (index, want) in [
        (SegmentIndex::Cs, expected.cs),
        (SegmentIndex::Ss, expected.ss),
        (SegmentIndex::Ds, expected.ds),
        (SegmentIndex::Es, expected.es),
        (SegmentIndex::Fs, expected.fs),
        (SegmentIndex::Gs, expected.gs),
    ] {
        let live = cpu.registers.segment(index);
        assert_eq!(live.selector, want.selector, "{index:?} selector");
        assert_eq!(live.base, want.base, "{index:?} cached base");
        assert_eq!(live.limit, want.limit, "{index:?} cached limit");
        assert_eq!(live.access, want.access, "{index:?} cached access");
        assert_eq!(
            live.default_size_32, want.default_size_32,
            "{index:?} cached D/B bit"
        );
    }
    assert_eq!(
        cpu.registers.eip,
        CLIENT_RETURN_EIP + 2,
        "EIP lands on the instruction AFTER the INT, from the memo's own recorded value"
    );
    assert_eq!(cpu.reflected_call.as_ref().unwrap().answered, 1);
}

/// **Mutation bite**: delete the `bus.interrupt_acknowledge` loop in `apply_answer` --
/// the trip's nested `INT 16h`/`INT 28h` acks vanish, so the bus trace, its I/O wait
/// states and `last_int_vector` all diverge from the real trip (BLOCKING finding 6).
/// A second bite: replay the list in reverse -- the ORDER assertion below fails.
#[test]
fn nested_acks_are_re_issued_in_trip_order() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    arm(&mut cpu);
    bus.gate_allowance = Some(1_000_000);
    let key = key_for(&cpu, VECTOR, 0);
    let memo = distinguishable_memo(&cpu, 0x7200, cpu.control.cr3);
    install_memo(&mut cpu, key, memo);
    bus.acks.clear();

    assert_eq!(
        on_int(&mut cpu, &mut bus, VECTOR).expect("no bus fault"),
        IntOutcome::Answered
    );
    assert_eq!(
        bus.acks,
        vec![(0x16u8, 0x0100u16), (0x28u8, 0x0000u16)],
        "the recorded nested acks, in trip order, with their own AX"
    );
}

/// **Mutation bite**: skip the `memo.replay` loop -- the deterministic net writes the
/// trip made (the DPMI host's register block, a DOS data byte) are never made, and the
/// guest reads stale data with nothing in the read set to catch it. A second bite:
/// write `BusWidth::Dword` regardless of the recorded width -- the two bytes above the
/// word are clobbered, which the neighbour assertion below catches.
#[test]
fn a_class_w_write_is_replayed_at_its_own_address_and_width() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    arm(&mut cpu);
    bus.gate_allowance = Some(1_000_000);
    bus.write_raw(0x7200, BusWidth::Dword, 0x1234_5678);
    let key = key_for(&cpu, VECTOR, 0);
    let memo = distinguishable_memo(&cpu, 0x7200, cpu.control.cr3);
    install_memo(&mut cpu, key, memo);

    assert_eq!(
        on_int(&mut cpu, &mut bus, VECTOR).expect("no bus fault"),
        IntOutcome::Answered
    );
    assert_eq!(bus.read_raw(0x7200, BusWidth::Word), 0xBEEF);
    assert_eq!(
        bus.read_raw(0x7202, BusWidth::Word),
        0x1234,
        "a WORD replay must not smear across the neighbouring bytes the trip never wrote"
    );
}

/// **Mutation bite**: drop `memo.control_effects` from the apply path -- an answered
/// trip leaves CR3 (and therefore the TLB and the decode ring) where the CALLER left
/// it, so a guest that edits a page table and relies on the reflected call's own mode
/// switch to publish the edit reads through a stale translation (BLOCKING finding 5).
#[test]
fn an_answered_trip_replays_the_cr3_writes() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    arm(&mut cpu);
    bus.gate_allowance = Some(1_000_000);
    let cr3_after = 0x0002_1000u32;
    assert_ne!(cpu.control.cr3, cr3_after);
    let key = key_for(&cpu, VECTOR, 0);
    let memo = distinguishable_memo(&cpu, 0x7200, cr3_after);
    let flushes_before = cpu.perf.decode_inval_cr3;
    install_memo(&mut cpu, key, memo);

    assert_eq!(
        on_int(&mut cpu, &mut bus, VECTOR).expect("no bus fault"),
        IntOutcome::Answered
    );
    assert_eq!(
        cpu.control.cr3, cr3_after,
        "the replayed CR3 write moved CR3"
    );
    assert_eq!(
        cpu.perf.decode_inval_cr3,
        flushes_before + 1,
        "and it ran the SAME teardown a real MOV CR3 runs, once per recorded effect"
    );
}

/// **Mutation bite**: feed `commit_reflected_call_core` a pre-scaled delta instead of
/// the RAW total (BLOCKING finding 2's double-scale). With `raw = 1,000` and
/// `den = 12` the charge is `1000/12 = 83` and the carry left behind is `1000 % 12 =
/// 4`; a double-scale charges 6 and leaves 11, and both halves of this assertion move.
/// The raw total is deliberately not a multiple of `den`, or the carry check is vacuous
/// (review item 20's complaint about the plan's own test 5).
#[test]
fn the_answer_charges_raw_core_clocks_through_the_carry_scaler() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    arm(&mut cpu);
    bus.gate_allowance = Some(1_000_000);
    let key = key_for(&cpu, VECTOR, 0);
    let memo = distinguishable_memo(&cpu, 0x7200, cpu.control.cr3);
    let raw_core = memo.raw_core_clocks;
    let raw_bus = memo.raw_bus_clocks;
    install_memo(&mut cpu, key, memo);

    let (num, den) = scaler();
    let elapsed_before = cpu.elapsed_clocks;
    let rem_before = cpu.reflected_call_timing_rem();
    let bus_before = bus.bus_clock;
    let insns_before = cpu.perf.instructions;

    assert_eq!(
        on_int(&mut cpu, &mut bus, VECTOR).expect("no bus fault"),
        IntOutcome::Answered
    );

    let scaled = raw_core * num + rem_before;
    assert_eq!(cpu.elapsed_clocks - elapsed_before, scaled / den);
    assert_eq!(cpu.reflected_call_timing_rem(), scaled % den);
    assert_ne!(
        scaled % den,
        0,
        "the fixture must exercise a non-zero carry"
    );
    // The bus half is charged NET of what the answer's own acks and replay writes
    // spent (this bus charges nothing for either, so the whole recorded total lands).
    assert_eq!(bus.committed_bus, raw_bus);
    assert_eq!(bus.bus_clock - bus_before, raw_bus);
    // The guest really did advance past the trip's instructions, so the ON arm's
    // `perf.instructions` matches the OFF arm's exactly rather than merely
    // reconciling with an elided counter.
    assert_eq!(cpu.perf.instructions - insns_before, 1_579);
    assert_eq!(cpu.reflected_call.as_ref().unwrap().insns_elided, 1_579);
    assert!(bus.answered_flag, "the answered trip must end the batch");
}

/// **Mutation bite**: remove the clamp (the `reflected_call_gate` call, or its `Err`
/// arm) -- a lump the remaining batch allowance cannot hold is applied anyway, running
/// the batch past the next device edge, and the guest's timer ISR loses ticks the real
/// PIT delivers. The assertion is on the FALL-THROUGH, not merely on a counter: every
/// register, the memory image, the clock and the carry must be untouched.
#[test]
fn a_lump_that_does_not_fit_the_remaining_allowance_falls_through() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    arm(&mut cpu);
    // 1,000 raw core scales to 83, plus 500 raw bus: 583 needed, 100 offered.
    bus.gate_allowance = Some(100);
    let key = key_for(&cpu, VECTOR, 0);
    let memo = distinguishable_memo(&cpu, 0x7200, cpu.control.cr3);
    install_memo(&mut cpu, key, memo);

    let regs_before = cpu.registers.clone();
    let mem_before = bus.mem.clone();
    let elapsed_before = cpu.elapsed_clocks;
    let rem_before = cpu.reflected_call_timing_rem();

    assert_eq!(
        on_int(&mut cpu, &mut bus, VECTOR).expect("no bus fault"),
        IntOutcome::NotAnswered
    );
    assert_eq!(cpu.registers, regs_before);
    assert_eq!(bus.mem, mem_before);
    assert_eq!(cpu.elapsed_clocks, elapsed_before);
    assert_eq!(cpu.reflected_call_timing_rem(), rem_before);
    assert!(
        bus.acks.is_empty(),
        "not one ack may be issued on a refusal"
    );
    assert!(!bus.answered_flag);
    let state = cpu.reflected_call.as_ref().unwrap();
    assert_eq!(state.answered, 0);
    assert_eq!(state.fell_through_device_edge, 1);
    assert_eq!(
        state.would_answer, 1,
        "screens 3 and 6 passed; the clamp is what refused"
    );
}

/// **Mutation bite**: move the observer test AFTER the apply (or delete it) -- a Class R
/// write inside a device window or the framebuffer aperture is SKIPPED, so the device
/// never sees the intermediate value the real trip showed it (review A.4, plan 5.7).
#[test]
fn a_class_r_range_a_device_can_observe_falls_through_at_answer_time() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    arm(&mut cpu);
    bus.gate_allowance = Some(1_000_000);
    bus.dma_visible = true;
    let key = key_for(&cpu, VECTOR, 0);
    let memo = distinguishable_memo(&cpu, 0x7200, cpu.control.cr3);
    install_memo(&mut cpu, key, memo);

    let regs_before = cpu.registers.clone();
    let mem_before = bus.mem.clone();
    assert_eq!(
        on_int(&mut cpu, &mut bus, VECTOR).expect("no bus fault"),
        IntOutcome::NotAnswered
    );
    assert_eq!(cpu.registers, regs_before);
    assert_eq!(bus.mem, mem_before);
    assert_eq!(
        cpu.reflected_call
            .as_ref()
            .unwrap()
            .fell_through_dma_visible,
        1
    );
}

/// The FALL-THROUGH INVARIANT, driven through the real hook with the gate ARMED and
/// every other screen passing, so the only thing standing between the memo and an
/// answer is one poisoned read cell.
///
/// **Mutation bite**: apply the replay before the read-set compare (partial
/// application) -- the memory assertion fails; charge the clocks before the compare --
/// the clock assertions fail; bump `answered` before the screens -- the counter
/// assertion fails.
#[test]
fn a_poisoned_read_cell_leaves_every_register_memory_clock_and_counter_untouched() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    arm(&mut cpu);
    bus.gate_allowance = Some(1_000_000);
    let addr = 0x7100u32;
    bus.write_raw(addr, BusWidth::Dword, 0x1111_1111);
    let key = key_for(&cpu, VECTOR, 0);
    let mut memo = distinguishable_memo(&cpu, 0x7200, cpu.control.cr3);
    memo.reads = Box::new([(addr, 0x1111_1111)]);
    install_memo(&mut cpu, key, memo);
    bus.write_raw(addr, BusWidth::Dword, 0x2222_2222); // poison, after caching

    let regs_before = cpu.registers.clone();
    let mem_before = bus.mem.clone();
    let elapsed_before = cpu.elapsed_clocks;
    let rem_before = cpu.reflected_call_timing_rem();
    let insns_before = cpu.perf.instructions;
    let cr3_before = cpu.control.cr3;

    assert_eq!(
        on_int(&mut cpu, &mut bus, VECTOR).expect("no bus fault"),
        IntOutcome::NotAnswered
    );
    assert_eq!(cpu.registers, regs_before, "no register may move");
    assert_eq!(bus.mem, mem_before, "no memory cell may move");
    assert_eq!(
        cpu.elapsed_clocks, elapsed_before,
        "no clock may be charged"
    );
    assert_eq!(
        cpu.reflected_call_timing_rem(),
        rem_before,
        "no carry may move"
    );
    assert_eq!(cpu.perf.instructions, insns_before);
    assert_eq!(
        cpu.control.cr3, cr3_before,
        "no control effect may be replayed"
    );
    assert!(bus.acks.is_empty());
    assert_eq!(bus.committed_bus, 0);
    assert!(!bus.answered_flag);
    let state = cpu.reflected_call.as_ref().unwrap();
    assert_eq!(state.answered, 0);
    assert_eq!(state.would_answer, 0);
    assert_eq!(state.read_set_mismatch, 1);
}

/// **Mutation bite**: give `CpuBus::reflected_call_gate` a permissive default
/// (`Ok(())`) instead of `Err(NotArmed)`. Every clamp fixture in this file, and every
/// test double in the whole CPU crate, would then silently ADMIT answers -- the
/// vacuity `callout_poll_skip`'s own default-to-decline exists to rule out.
#[test]
fn an_unarmed_test_double_can_never_fake_a_hit() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    arm(&mut cpu);
    assert!(bus.gate_allowance.is_none(), "the default, deliberately");
    let key = key_for(&cpu, VECTOR, 0);
    let memo = distinguishable_memo(&cpu, 0x7200, cpu.control.cr3);
    install_memo(&mut cpu, key, memo);

    let regs_before = cpu.registers.clone();
    assert_eq!(
        on_int(&mut cpu, &mut bus, VECTOR).expect("no bus fault"),
        IntOutcome::NotAnswered
    );
    assert_eq!(cpu.registers, regs_before);
    assert_eq!(
        cpu.reflected_call.as_ref().unwrap().fell_through_not_armed,
        1
    );
}

/// **Mutation bite**: record nested acks only for vectors inside the `0x10..=0x33`
/// opening window. `INT 2Fh` (multiplex) and `INT 0x60`-range TSR hooks then acknowledge
/// on the real trip and not on the answered one, so the bus trace diverges with nothing
/// in any screen able to see it.
#[test]
fn a_nested_int_outside_the_opening_window_is_still_recorded_as_an_ack() {
    let (mut cpu, mut bus) = synthetic_reflected_client();
    arm(&mut cpu);
    let key = key_for(&cpu, VECTOR, 0);
    let mut open = test_open_trip(&cpu);
    open.key = key;
    open.journaling = true;
    // Not the trip's own signature, and not at the entry SP: a genuine NESTED call.
    cpu.registers.set_esp(STACK_TOP - 64);
    cpu.reflected_call.as_mut().unwrap().open = Some(open);
    cpu.registers.set_eax(0x1234);

    let _ = on_int(&mut cpu, &mut bus, 0x60);
    let acks = &cpu
        .reflected_call
        .as_ref()
        .unwrap()
        .open
        .as_ref()
        .expect("the trip stays open across a nested INT")
        .nested_acks;
    assert_eq!(acks, &vec![(0x60u8, 0x1234u16)]);
}
