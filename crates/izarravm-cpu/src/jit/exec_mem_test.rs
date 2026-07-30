// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn executes_a_trivial_emitted_function() {
    // `mov eax, 42; ret` -- B8 2A 00 00 00 C3. Calling convention agnostic: it reads no
    // arguments and returns a 32-bit value in EAX/RAX on both win64 and sysv64.
    let code = [0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC3];
    let buf = ExecutableBuffer::new(&code).expect("allocation must succeed on a supported host");
    let f: extern "C" fn() -> i32 = unsafe { std::mem::transmute(buf.entry_ptr()) };
    assert_eq!(f(), 42);
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn arena_seals_one_page_per_block() {
    let mut arena = ExecutableArena::new().expect("allocation must succeed on a supported host");
    assert_eq!(EXECUTABLE_ARENA_LEN % arena.slot_len(), 0);
    assert_eq!(
        arena.slot_capacity(),
        EXECUTABLE_ARENA_LEN / arena.slot_len()
    );

    let code = [0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC3];
    let entry = arena.install(&code).expect("one block must fit");
    let f: extern "C" fn() -> i32 = unsafe { std::mem::transmute(entry) };
    assert_eq!(f(), 42);
    assert_eq!(arena.used_slots(), 1);

    let oversized = vec![0xC3; arena.slot_len() + 1];
    assert!(arena.install(&oversized).is_none());
    assert_eq!(arena.used_slots(), 1);
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn arena_bulk_copies_sealed_slots_then_executes_them() {
    let mut source = ExecutableArena::new().expect("allocation must succeed on a supported host");
    let returns_42 = [0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC3];
    let returns_7 = [0xB8, 0x07, 0x00, 0x00, 0x00, 0xC3];
    let old_42 = source.install(&returns_42).expect("source block");
    let old_7 = source.install(&returns_7).expect("source block");

    let mut fresh = ExecutableArena::new().expect("allocation must succeed on a supported host");
    let slot_42 = fresh
        .append_unsealed(
            source
                .sealed_slot_bytes(old_42, returns_42.len())
                .expect("validated source bytes"),
        )
        .expect("fresh slot");
    let slot_7 = fresh
        .append_unsealed(
            source
                .sealed_slot_bytes(old_7, returns_7.len())
                .expect("validated source bytes"),
        )
        .expect("fresh slot");

    assert!(fresh.sealed_slot_entry(slot_42).is_none());
    assert!(fresh.sealed_slot_entry(slot_7).is_none());
    assert!(fresh.install(&[0xC3]).is_none());
    assert!(fresh.seal_used_prefix());

    let new_42 = fresh.sealed_slot_entry(slot_42).expect("sealed entry");
    let new_7 = fresh.sealed_slot_entry(slot_7).expect("sealed entry");
    let f_42: extern "C" fn() -> i32 = unsafe { std::mem::transmute(new_42) };
    let f_7: extern "C" fn() -> i32 = unsafe { std::mem::transmute(new_7) };
    assert_eq!(f_42(), 42);
    assert_eq!(f_7(), 7);

    let next = fresh
        .install(&returns_42)
        .expect("normal install resumes after the bulk seal");
    let next_fn: extern "C" fn() -> i32 = unsafe { std::mem::transmute(next) };
    assert_eq!(next_fn(), 42);
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn arena_bulk_copy_rejects_unsealed_and_out_of_slot_ranges() {
    let mut source = ExecutableArena::new().expect("allocation must succeed on a supported host");
    let code = [0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC3];
    let entry = source.install(&code).expect("source block");
    assert!(source.append_unsealed(&code).is_none());

    assert!(source.contains_sealed_slot_range(entry, code.len()));
    assert_eq!(source.sealed_slot_bytes(entry, code.len()), Some(&code[..]));
    assert!(!source.contains_sealed_slot_range(entry, 0));
    assert!(!source.contains_sealed_slot_range(entry, source.slot_len() + 1));
    assert!(!source.contains_sealed_slot_range(entry.wrapping_add(1), code.len()));
    assert!(!source.contains_sealed_slot_range(entry.wrapping_add(source.slot_len()), code.len()));

    let mut fresh = ExecutableArena::new().expect("allocation must succeed on a supported host");
    assert!(!fresh.seal_used_prefix());
    assert!(fresh.append_unsealed(&[]).is_none());
    assert!(
        fresh
            .append_unsealed(&vec![0xC3; fresh.slot_len() + 1])
            .is_none()
    );
    assert_eq!(fresh.used_slots(), 0);

    let slot = fresh.append_unsealed(&code).expect("valid pending slot");
    assert!(fresh.sealed_slot_entry(slot).is_none());
    assert!(fresh.seal_used_prefix());
    assert!(!fresh.seal_used_prefix());
    assert!(fresh.append_unsealed(&code).is_none());
    assert_eq!(fresh.used_slots(), 1);
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn install_span_accepts_multi_page_code() {
    let mut arena = ExecutableArena::new().expect("allocation must succeed on a supported host");
    let page = arena.slot_len();
    let code = vec![0xC3u8; page + 17]; // > one page
    let entry = arena.install_span(&code).expect("multi-page span install");
    assert!(arena.contains_sealed_span_range(entry, code.len()));
    // The span seals immediately: its base is callable (0xC3 sled returns).
    let f: extern "C" fn() = unsafe { std::mem::transmute(entry) };
    f();
    // A normal one-page install continues after the span.
    let next = arena
        .install(&[0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC3])
        .expect("one-page install after a span");
    let g: extern "C" fn() -> i32 = unsafe { std::mem::transmute(next) };
    assert_eq!(g(), 42);
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn span_range_check_rejects_crossing_out_of_its_span() {
    let mut arena = ExecutableArena::new().expect("allocation must succeed on a supported host");
    let page = arena.slot_len();
    let a = arena.install_span(&vec![0xC3u8; page]).expect("span a");
    let _b = arena.install_span(&vec![0xC3u8; page]).expect("span b");
    assert!(arena.contains_sealed_span_range(a, page));
    assert!(!arena.contains_sealed_span_range(a, page + 1));
    assert!(!arena.contains_sealed_span_range(a, 0));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn one_page_install_still_works_unchanged() {
    let mut arena = ExecutableArena::new().expect("allocation must succeed on a supported host");
    let code = vec![0xC3u8; 64];
    assert!(arena.install(&code).is_some());
    let oversized = vec![0xC3u8; arena.slot_len() + 1];
    assert!(arena.install(&oversized).is_none());
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn mid_span_page_boundary_is_not_a_valid_entry() {
    // Only span BASES are valid entries: a page boundary INSIDE a multi-page
    // span must fail both range checks, exactly as mid-slot offsets fail the
    // one-page checks today.
    let mut arena = ExecutableArena::new().expect("allocation must succeed on a supported host");
    let page = arena.slot_len();
    let entry = arena
        .install_span(&vec![0xC3u8; 2 * page])
        .expect("two-page span");
    let mid = entry.wrapping_add(page);
    assert!(!arena.contains_sealed_span_range(mid, page));
    assert!(!arena.contains_sealed_span_range(mid, 1));
    assert!(!arena.contains_sealed_slot_range(mid, 1));
    assert!(arena.contains_sealed_span_range(entry, 2 * page));
}

#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn install_span_rejects_oversized_empty_and_unsealed_prefix() {
    // Span larger than the whole (small) arena.
    let mut arena =
        ExecutableArena::with_len_for_test(2 * host_page_len()).expect("small test arena");
    let page = arena.slot_len();
    assert!(arena.install_span(&vec![0xC3u8; 2 * page + 1]).is_none());
    assert!(arena.install_span(&[]).is_none());
    assert_eq!(arena.used_slots(), 0);
    // A span that exactly fills the arena still installs.
    assert!(arena.install_span(&vec![0xC3u8; 2 * page]).is_some());
    assert!(arena.is_full());
    assert!(arena.install_span(&[0xC3]).is_none());

    // A pending unsealed prefix (sealed != used) blocks install_span until sealed.
    let mut fresh = ExecutableArena::new().expect("allocation must succeed on a supported host");
    let slot = fresh.append_unsealed(&[0xC3]).expect("pending slot");
    assert!(fresh.install_span(&[0xC3]).is_none());
    assert!(fresh.seal_used_prefix());
    assert!(fresh.sealed_slot_entry(slot).is_some());
    assert!(fresh.install_span(&[0xC3]).is_some());
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
#[test]
fn installed_span_registers_a_runtime_function() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn RtlLookupFunctionEntry(
            control_pc: u64,
            image_base: *mut u64,
            history_table: *mut core::ffi::c_void,
        ) -> *const core::ffi::c_void;
    }
    let page = super::host_page_len();
    let mut arena = super::ExecutableArena::with_len_for_test(4 * page).unwrap();
    let entry = arena.install(&[0xC3]).unwrap(); // one RET; content is irrelevant to lookup
    let mut base = 0u64;
    // Probe an address INSIDE the span body, not just the entry.
    let rf = unsafe { RtlLookupFunctionEntry(entry as u64 + 4, &mut base, std::ptr::null_mut()) };
    assert!(
        !rf.is_null(),
        "no RUNTIME_FUNCTION covers the sealed span; growable-table registration missing"
    );
    // First install sits at arena offset 0, so the reported range base is the entry itself.
    assert_eq!(
        base, entry as u64,
        "table registered under the wrong RangeBase"
    );
}

#[cfg(not(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
)))]
#[test]
fn unsupported_host_never_allocates() {
    let code = [0xC3];
    assert!(ExecutableBuffer::new(&code).is_none());
    assert!(ExecutableArena::new().is_none());
}

/// Track C A2 (`dev_docs/plans/2026-07-19-clif-arena-reset-design.md` section 7.2): `reset`
/// reclaims a full arena back to the empty state `with_len_for_test` produced -- capacity,
/// used/sealed bookkeeping, and the span registry all reset -- so a fresh `install_span`
/// after it succeeds and runs correctly, exactly as it would against a brand-new arena.
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn reset_reclaims_a_full_arena_back_to_empty() {
    let mut arena =
        ExecutableArena::with_len_for_test(2 * host_page_len()).expect("small test arena");
    let page = arena.slot_len();
    let returns_42 = [0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC3];
    let returns_7 = [0xB8, 0x07, 0x00, 0x00, 0x00, 0xC3];
    let first = arena.install(&returns_42).expect("first install");
    let second = arena.install(&returns_7).expect("second install fills it");
    assert!(arena.is_full());
    assert!(
        arena.install(&[0xC3]).is_none(),
        "no room left before reset"
    );

    assert!(arena.reset(), "reset must succeed on a supported host");
    assert_eq!(arena.used_slots(), 0, "reset must reclaim every span");
    assert!(!arena.is_full());
    // The old entries no longer register as sealed spans (a stale pointer must not validate).
    assert!(!arena.contains_sealed_span_range(first, returns_42.len()));
    assert!(!arena.contains_sealed_span_range(second, returns_7.len()));

    // The reclaimed arena behaves exactly like a fresh one: same capacity, and code installed
    // into it actually runs (the pages are genuinely writable again, not just bookkeeping).
    let reinstalled = arena.install(&returns_42).expect("install after reset");
    let f: extern "C" fn() -> i32 = unsafe { std::mem::transmute(reinstalled) };
    assert_eq!(f(), 42);
    assert_eq!(arena.used_slots(), 1);
    assert!(
        arena.install(&vec![0xC3u8; page]).is_some(),
        "arena has its full capacity back"
    );
    assert!(arena.is_full());
}

/// `reset` on an arena that never sealed anything (nothing installed, or only pending
/// `append_unsealed` slots) is a no-op that still succeeds and leaves the arena writable and
/// empty -- the `sealed == 0` early return (design section 7.2).
#[cfg(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
#[test]
fn reset_on_a_never_sealed_arena_is_a_sound_no_op() {
    let mut fresh = ExecutableArena::new().expect("allocation must succeed on a supported host");
    assert!(fresh.reset());
    assert_eq!(fresh.used_slots(), 0);
    assert!(
        fresh
            .install(&[0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC3])
            .is_some()
    );

    let mut pending = ExecutableArena::new().expect("allocation must succeed on a supported host");
    let code = [0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC3];
    let slot = pending.append_unsealed(&code).expect("pending slot");
    assert!(
        pending.reset(),
        "an unsealed-only arena must still reset cleanly"
    );
    assert_eq!(pending.used_slots(), 0);
    assert!(pending.sealed_slot_entry(slot).is_none());
    assert!(pending.install(&code).is_some());
}
