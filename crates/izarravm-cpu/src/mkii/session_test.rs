// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_bus::{BusTrace, MkiiBusSession, MkiiBusSessionParts, MkiiCounterPath, TracingMode};

unsafe fn read(path: MkiiCounterPath, root: *const u8) -> u64 {
    let field = unsafe { root.add(path.root_offset as usize) };
    if path.pointee_offset == MkiiCounterPath::DIRECT {
        unsafe { field.cast::<u64>().read() }
    } else {
        let child = unsafe { field.cast::<*const u8>().read() };
        unsafe { child.add(path.pointee_offset as usize).cast::<u64>().read() }
    }
}

#[test]
fn mkii_session_rejects_another_owner_and_reads_embedded_counters() {
    struct Ledger {
        trace: u64,
        isa: u64,
        epoch: u64,
    }
    let mut owner = Ledger {
        trace: 7,
        isa: 11,
        epoch: 13,
    };
    let other = Ledger {
        trace: 17,
        isa: 19,
        epoch: 23,
    };
    let root = std::ptr::from_mut(&mut owner);
    let parts = MkiiBusSessionParts {
        trace_clocks: MkiiCounterPath::direct(std::mem::offset_of!(Ledger, trace) as u32),
        isa_clocks: MkiiCounterPath::direct(std::mem::offset_of!(Ledger, isa) as u32),
        mapping_epoch: MkiiCounterPath::direct(std::mem::offset_of!(Ledger, epoch) as u32),
        trace_origin: 0,
        cost_epoch: 1,
        bus_numerator: 33,
        bus_denominator: 105,
    };
    // SAFETY: all paths name initialized counters; only this test accesses the owner.
    let grant = unsafe { MkiiBusSession::certify(&owner, parts) }.unwrap();
    assert!(grant.into_parts(std::ptr::from_ref(&other)).is_none());
    let grant = unsafe { MkiiBusSession::certify(&owner, parts) }.unwrap();
    let live = grant.into_parts(root).unwrap();
    assert_eq!(unsafe { read(live.trace_clocks, root.cast()) }, 7);
    assert_eq!(unsafe { read(live.isa_clocks, root.cast()) }, 11);
    assert_eq!(unsafe { read(live.mapping_epoch, root.cast()) }, 13);
    unsafe {
        let helper = &mut *root;
        helper.trace += 29;
        helper.isa += 31;
        helper.epoch += 37;
    }
    assert_eq!(unsafe { read(live.trace_clocks, root.cast()) }, 36);
    assert_eq!(unsafe { read(live.isa_clocks, root.cast()) }, 42);
    assert_eq!(unsafe { read(live.mapping_epoch, root.cast()) }, 50);
}

#[test]
fn mkii_session_reloads_indirect_counters_after_a_helper() {
    struct Ledger<'a> {
        trace: &'a mut BusTrace,
        isa: &'a mut u64,
        epoch: &'a mut u64,
    }
    let mut first_trace = Box::new(BusTrace::with_capacity(0));
    let mut next_trace = Box::new(BusTrace::with_capacity(0));
    first_trace.set_tracing_mode(TracingMode::Off);
    next_trace.set_tracing_mode(TracingMode::Off);
    first_trace.add_elapsed_clocks(41);
    next_trace.add_elapsed_clocks(43);
    let (mut first_isa, mut next_isa) = (47, 53);
    let (mut first_epoch, mut next_epoch) = (59, 61);
    let mut owner = Ledger {
        trace: &mut first_trace,
        isa: &mut first_isa,
        epoch: &mut first_epoch,
    };
    let root = std::ptr::from_mut(&mut owner);
    let parts = MkiiBusSessionParts {
        trace_clocks: MkiiCounterPath::trace(std::mem::offset_of!(Ledger<'_>, trace) as u32),
        isa_clocks: MkiiCounterPath::indirect(std::mem::offset_of!(Ledger<'_>, isa) as u32, 0),
        mapping_epoch: MkiiCounterPath::indirect(std::mem::offset_of!(Ledger<'_>, epoch) as u32, 0),
        trace_origin: 0,
        cost_epoch: 1,
        bus_numerator: 33,
        bus_denominator: 105,
    };
    // SAFETY: the pointer fields and both sets of pointees remain live throughout the test.
    let grant = unsafe { MkiiBusSession::certify(&owner, parts) }.unwrap();
    let live = grant.into_parts(root).unwrap();
    assert_eq!(unsafe { read(live.trace_clocks, root.cast()) }, 41);
    assert_eq!(unsafe { read(live.isa_clocks, root.cast()) }, 47);
    assert_eq!(unsafe { read(live.mapping_epoch, root.cast()) }, 59);
    unsafe {
        let helper = &mut *root;
        helper.trace = &mut next_trace;
        helper.isa = &mut next_isa;
        helper.epoch = &mut next_epoch;
        helper.trace.add_elapsed_clocks(67);
        *helper.isa += 71;
        *helper.epoch += 73;
    }
    assert_eq!(unsafe { read(live.trace_clocks, root.cast()) }, 110);
    assert_eq!(unsafe { read(live.isa_clocks, root.cast()) }, 124);
    assert_eq!(unsafe { read(live.mapping_epoch, root.cast()) }, 134);
    assert_eq!(first_trace.elapsed_clocks(), 41);
    assert_eq!(first_isa, 47);
    assert_eq!(first_epoch, 59);
}
