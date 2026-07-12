// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Write-then-execute (W^X) storage for emitted machine code. Windows and Linux x86-64 get a real
//! allocator; every other target compiles nothing and runs the interpreter unchanged.

/// Owns a page of memory that starts Read+Write (so the encoder can write bytes into it) and is
/// flipped to Read+Execute before any code in it runs, and never both writable and executable at
/// the same time.
pub(crate) struct ExecutableBuffer {
    ptr: *mut u8,
    len: usize,
}

/// Maximum virtual memory reserved for direct blocks. Each block owns one host page so completed
/// blocks can be sealed Read+Execute while unused slots remain Read+Write.
pub(crate) const EXECUTABLE_ARENA_LEN: usize = 32 * 1024 * 1024;

/// A bounded collection of one-page executable-code slots.
pub(crate) struct ExecutableArena {
    ptr: *mut u8,
    len: usize,
    page_len: usize,
    used: usize,
    sealed: usize,
}

/// Identifies one arena slot without exposing its address before that slot is executable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ArenaSlot(usize);

// SAFETY: the buffer holds only plain code bytes; nothing here is interior-mutable shared state.
// `CpuGsw` (which owns these via `JitBlockCache`) is not itself required to be Send/Sync today,
// but marking this Send/Sync keeps it from accidentally blocking a future requirement; the
// pointer is never aliased outside this type.
unsafe impl Send for ExecutableBuffer {}
unsafe impl Sync for ExecutableBuffer {}

// SAFETY: installing code requires an exclusive borrow, and installed pages are never made
// writable again. Exposed entry pointers refer only to sealed pages.
unsafe impl Send for ExecutableArena {}
unsafe impl Sync for ExecutableArena {}

impl ExecutableBuffer {
    /// Allocate `len` bytes (rounded up to a page), write `code` into it, then flip the page to
    /// Read+Execute. `len` must be >= `code.len()`. Returns `None` on an unsupported host or an
    /// OS allocation failure -- both are "compile nothing, fall back to the interpreter".
    pub(crate) fn new(code: &[u8]) -> Option<Self> {
        let page = page_size();
        let len = code.len().div_ceil(page) * page;
        let ptr = alloc_rw(len)?;
        // SAFETY: `ptr` was just allocated with `len` writable bytes and is not aliased yet.
        unsafe {
            std::ptr::copy_nonoverlapping(code.as_ptr(), ptr, code.len());
        }
        if !flush_instruction_cache(ptr, len) || !make_rx(ptr, len) {
            // SAFETY: `ptr`/`len` came from `alloc_rw` above.
            unsafe { free(ptr, len) };
            return None;
        }
        Some(Self { ptr, len })
    }

    /// The buffer's base address, valid to call through as a function pointer of the caller's
    /// chosen `extern "C"` signature once the buffer has been flipped to Read+Execute (always
    /// true by the time `new` returns `Some`).
    pub(crate) fn entry_ptr(&self) -> *const u8 {
        self.ptr
    }
}

impl ExecutableArena {
    /// Reserve the fixed-size arena as Read+Write memory. Individual pages become Read+Execute as
    /// blocks are installed. Unsupported hosts and allocation failure both return `None`.
    pub(crate) fn new() -> Option<Self> {
        let page_len = page_size();
        let len = EXECUTABLE_ARENA_LEN / page_len * page_len;
        if len == 0 {
            return None;
        }
        let ptr = alloc_rw(len)?;
        Some(Self {
            ptr,
            len,
            page_len,
            used: 0,
            sealed: 0,
        })
    }

    /// Copy one block into the next page and seal that page Read+Execute.
    pub(crate) fn install(&mut self, code: &[u8]) -> Option<*const u8> {
        if code.is_empty()
            || code.len() > self.page_len
            || self.is_full()
            || self.sealed != self.used
        {
            return None;
        }
        // SAFETY: `used < len`, both values are page-aligned, and this page has not previously
        // been exposed or sealed.
        let slot = unsafe { self.ptr.add(self.used) };
        // SAFETY: the slot has `page_len` writable bytes and `code` fits in it.
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), slot, code.len()) };
        if !flush_instruction_cache(slot, self.page_len) || !make_rx(slot, self.page_len) {
            return None;
        }
        self.used += self.page_len;
        self.sealed = self.used;
        Some(slot)
    }

    /// Copy one block into the next slot without making it executable. This is allowed only in a
    /// fresh arena whose used prefix is still wholly writable. The returned token cannot expose an
    /// entry pointer until `seal_used_prefix` succeeds.
    pub(crate) fn append_unsealed(&mut self, code: &[u8]) -> Option<ArenaSlot> {
        if code.is_empty() || code.len() > self.page_len || self.is_full() || self.sealed != 0 {
            return None;
        }
        let offset = self.used;
        // SAFETY: `used < len`, both values are page-aligned, and this fresh slot is writable.
        let slot = unsafe { self.ptr.add(offset) };
        // SAFETY: the slot has `page_len` writable bytes and `code` fits in it.
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), slot, code.len()) };
        self.used += self.page_len;
        Some(ArenaSlot(offset))
    }

    /// Flush all unsealed code and make the used prefix Read+Execute with one protection change.
    /// Failure leaves the arena unpublished and prevents normal installs until a retry succeeds.
    pub(crate) fn seal_used_prefix(&mut self) -> bool {
        if self.used == 0 || self.sealed != 0 {
            return false;
        }
        if !flush_instruction_cache(self.ptr, self.used) || !make_rx(self.ptr, self.used) {
            return false;
        }
        self.sealed = self.used;
        true
    }

    /// Return the entry for a slot only after its whole page has been sealed executable.
    pub(crate) fn sealed_slot_entry(&self, slot: ArenaSlot) -> Option<*const u8> {
        self.slot_is_sealed(slot.0)
            // SAFETY: `slot_is_sealed` proves the offset lies within this allocation.
            .then(|| unsafe { self.ptr.add(slot.0) as *const u8 })
    }

    /// Whether `entry..entry+code_len` starts at, and stays within, one sealed arena slot.
    pub(crate) fn contains_sealed_slot_range(&self, entry: *const u8, code_len: usize) -> bool {
        if code_len == 0 || code_len > self.page_len {
            return false;
        }
        let Some(offset) = (entry as usize).checked_sub(self.ptr as usize) else {
            return false;
        };
        self.slot_is_sealed(offset)
    }

    /// Borrow the exact requested bytes from a validated sealed slot.
    pub(crate) fn sealed_slot_bytes(&self, entry: *const u8, code_len: usize) -> Option<&[u8]> {
        if !self.contains_sealed_slot_range(entry, code_len) {
            return None;
        }
        // SAFETY: range validation proves these bytes lie in one sealed page owned by `self`.
        Some(unsafe { std::slice::from_raw_parts(entry, code_len) })
    }

    fn slot_is_sealed(&self, offset: usize) -> bool {
        offset % self.page_len == 0
            && offset
                .checked_add(self.page_len)
                .is_some_and(|end| end <= self.sealed)
    }

    pub(crate) fn is_full(&self) -> bool {
        self.used == self.len
    }

    pub(crate) fn slot_len(&self) -> usize {
        self.page_len
    }

    pub(crate) fn slot_capacity(&self) -> usize {
        self.len / self.page_len
    }

    #[cfg(test)]
    #[cfg_attr(
        not(all(
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        )),
        allow(dead_code)
    )]
    pub(crate) fn used_slots(&self) -> usize {
        self.used / self.page_len
    }
}

impl Drop for ExecutableBuffer {
    fn drop(&mut self) {
        // SAFETY: `self.ptr`/`self.len` were produced together by `alloc_rw` in `new` and never
        // mutated afterward.
        unsafe { free(self.ptr, self.len) };
    }
}

impl Drop for ExecutableArena {
    fn drop(&mut self) {
        // SAFETY: this is the original base and length returned by `alloc_rw`. Some pages have
        // since become Read+Execute, which does not change how the allocation is released.
        unsafe { free(self.ptr, self.len) };
    }
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
mod os {
    use std::ffi::c_void;

    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const MEM_RELEASE: u32 = 0x8000;
    const PAGE_READWRITE: u32 = 0x04;
    const PAGE_EXECUTE_READ: u32 = 0x20;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn VirtualAlloc(
            lpAddress: *mut c_void,
            dwSize: usize,
            flAllocationType: u32,
            flProtect: u32,
        ) -> *mut c_void;
        fn VirtualProtect(
            lpAddress: *mut c_void,
            dwSize: usize,
            flNewProtect: u32,
            lpflOldProtect: *mut u32,
        ) -> i32;
        fn VirtualFree(lpAddress: *mut c_void, dwSize: usize, dwFreeType: u32) -> i32;
        fn GetSystemInfo(lpSystemInfo: *mut SystemInfo);
        fn GetCurrentProcess() -> *mut c_void;
        fn FlushInstructionCache(
            hProcess: *mut c_void,
            lpBaseAddress: *const c_void,
            dwSize: usize,
        ) -> i32;
    }

    // Layout matches the real Win32 SYSTEM_INFO on x86-64 (48 bytes): a 4-byte
    // wProcessorArchitecture/wReserved union, dwPageSize, two 8-byte address pointers, an
    // 8-byte affinity mask, then 16 bytes of processor/allocation-granularity fields. We only
    // read dwPageSize; the rest is dead weight kept solely so GetSystemInfo (which writes the
    // full struct unconditionally) never writes past the end of `info`.
    #[repr(C, align(8))]
    struct SystemInfo {
        _processor_architecture: [u8; 4],
        page_size: u32,
        _address_and_processor_fields: [u8; 40],
    }

    const _: () = assert!(std::mem::size_of::<SystemInfo>() == 48);
    const _: () = assert!(std::mem::align_of::<SystemInfo>() == 8);

    pub(super) fn page_size() -> usize {
        let mut info: SystemInfo = unsafe { std::mem::zeroed() };
        unsafe { GetSystemInfo(&mut info) };
        info.page_size.max(4096) as usize
    }

    pub(super) fn alloc_rw(len: usize) -> Option<*mut u8> {
        let p = unsafe {
            VirtualAlloc(
                std::ptr::null_mut(),
                len,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        if p.is_null() {
            None
        } else {
            Some(p as *mut u8)
        }
    }

    pub(super) fn make_rx(ptr: *mut u8, len: usize) -> bool {
        let mut old = 0u32;
        unsafe { VirtualProtect(ptr as *mut c_void, len, PAGE_EXECUTE_READ, &mut old) != 0 }
    }

    pub(super) fn flush_instruction_cache(ptr: *mut u8, len: usize) -> bool {
        unsafe { FlushInstructionCache(GetCurrentProcess(), ptr as *const c_void, len) != 0 }
    }

    pub(super) unsafe fn free(ptr: *mut u8, _len: usize) {
        unsafe {
            VirtualFree(ptr as *mut c_void, 0, MEM_RELEASE);
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod os {
    use std::ffi::c_void;

    const PROT_READ: i32 = 0x1;
    const PROT_WRITE: i32 = 0x2;
    const PROT_EXEC: i32 = 0x4;
    const MAP_PRIVATE: i32 = 0x02;
    const MAP_ANONYMOUS: i32 = 0x20;
    const MAP_FAILED: isize = -1;

    unsafe extern "C" {
        fn mmap(
            addr: *mut c_void,
            len: usize,
            prot: i32,
            flags: i32,
            fd: i32,
            offset: i64,
        ) -> *mut c_void;
        fn munmap(addr: *mut c_void, len: usize) -> i32;
        fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
        fn sysconf(name: i32) -> i64;
    }

    const _SC_PAGESIZE: i32 = 30;

    pub(super) fn page_size() -> usize {
        let n = unsafe { sysconf(_SC_PAGESIZE) };
        if n > 0 { n as usize } else { 4096 }
    }

    pub(super) fn alloc_rw(len: usize) -> Option<*mut u8> {
        let p = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if p as isize == MAP_FAILED {
            None
        } else {
            Some(p as *mut u8)
        }
    }

    pub(super) fn make_rx(ptr: *mut u8, len: usize) -> bool {
        unsafe { mprotect(ptr as *mut c_void, len, PROT_READ | PROT_EXEC) == 0 }
    }
    pub(super) fn flush_instruction_cache(_ptr: *mut u8, _len: usize) -> bool {
        true
    }

    pub(super) unsafe fn free(ptr: *mut u8, len: usize) {
        unsafe {
            munmap(ptr as *mut c_void, len);
        }
    }
}

#[cfg(not(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
)))]
mod os {
    pub(super) fn page_size() -> usize {
        4096
    }
    pub(super) fn alloc_rw(_len: usize) -> Option<*mut u8> {
        None
    }
    pub(super) fn make_rx(_ptr: *mut u8, _len: usize) -> bool {
        false
    }
    pub(super) fn flush_instruction_cache(_ptr: *mut u8, _len: usize) -> bool {
        false
    }
    pub(super) unsafe fn free(_ptr: *mut u8, _len: usize) {}
}

use os::{alloc_rw, flush_instruction_cache, free, make_rx, page_size};

pub(crate) fn host_page_len() -> usize {
    page_size()
}

#[cfg(test)]
#[path = "exec_mem_test.rs"]
mod tests;
