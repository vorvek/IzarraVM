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
fn one_page_install_still_works_unchanged() {
    let mut arena = ExecutableArena::new().expect("allocation must succeed on a supported host");
    let code = vec![0xC3u8; 64];
    assert!(arena.install(&code).is_some());
    let oversized = vec![0xC3u8; arena.slot_len() + 1];
    assert!(arena.install(&oversized).is_none());
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
