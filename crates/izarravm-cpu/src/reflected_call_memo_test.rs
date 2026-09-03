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

    fn cumulative_raw_bus_clocks(&self) -> u64 {
        self.bus_clock
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
        hw_interrupt_seen: false,
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
        hw_interrupt_seen: false,
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
    let (mut cpu, bus) = synthetic_reflected_client();
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
    on_int(&mut cpu2, &bus, VECTOR);
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
        },
    );
    let result = compare_journal(
        &baseline,
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
    let (mut cpu, bus) = synthetic_reflected_client();
    arm(&mut cpu);
    let key = key_for(&cpu, VECTOR, 0);
    {
        let state = cpu.reflected_call.as_mut().unwrap();
        state.keys.entry(key).or_default().disarmed = true;
    }
    on_int(&mut cpu, &bus, VECTOR);
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
    let (mut cpu, bus) = synthetic_reflected_client();
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
    on_int(&mut cpu, &bus, VECTOR); // trips_seen -> 1,000,001: still far short of the window
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
    on_int(&mut cpu, &bus, VECTOR);
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
}
