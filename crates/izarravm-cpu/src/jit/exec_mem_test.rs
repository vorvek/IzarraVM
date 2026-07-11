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

    let code = [0xB8, 0x2A, 0x00, 0x00, 0x00, 0xC3];
    let entry = arena.install(&code).expect("one block must fit");
    let f: extern "C" fn() -> i32 = unsafe { std::mem::transmute(entry) };
    assert_eq!(f(), 42);
    assert_eq!(arena.used_slots(), 1);

    let oversized = vec![0xC3; arena.slot_len() + 1];
    assert!(arena.install(&oversized).is_none());
    assert_eq!(arena.used_slots(), 1);
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
