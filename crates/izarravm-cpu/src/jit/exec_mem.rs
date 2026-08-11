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

/// Default virtual memory reserved for direct blocks. Each block owns one host page so completed
/// blocks can be sealed Read+Execute while unused slots remain Read+Write. 32,768 slots at 4 KiB.
///
/// RAISED 32 -> 128 MiB, MEASURED, not chosen. At 32 MiB (8,192 slots) duke3d-486 filled the arena
/// and then compacted 1,205 times in one BENCH2 demo, because `arena_compaction_can_reclaim`
/// admits a whole-arena rebuild for a single dead slot and mean occupancy at that moment was
/// 95.9%: each rebuild copied 14.7 MB of live code to reclaim ~4% of the slots, and the next ~335
/// installs refilled it. Compaction cost scales with LIVE bytes copied, not with arena size, so
/// capacity is the cheap dial. Two independent A/B/B/A rounds against `IZARRAVM_JIT_ARENA_MIB`,
/// one binary, `dev_docs/duke3d-open-area-profile-results.md` §9:
///
/// | | compactions | `arena_compaction_ns` | installs | min wall |
/// |---|---|---|---|---|
/// | duke3d-486 32 MiB | 1,205 | 4.10 s | 588,852 | 138.103 s |
/// | duke3d-486 128 MiB | 19 | 0.17 s | 419,997 | 133.651 s (-3.22%) |
/// | duke3d-586 32 -> 128 MiB | 3,021 -> 83 | 10.34 -> 0.98 s | 1.98 M -> 1.48 M | -4.16% |
///
/// The arms did not overlap across the eight duke3d-486 legs and every deterministic counter was
/// byte-identical within each arm. doom-586 takes 2 compactions in a whole run, predicted no
/// change and measured none (installs, resets and frame hash identical; only the two arena
/// counters differ, 2 compactions/3.48 ms at 32 MiB vs 0/0 at 128 MiB, as the size change forces).
///
/// What it COSTS, because a default is a posture: 128 MiB of commit charge instead of 32, and two
/// arenas (256 MiB) live for the duration of a rebuild. Commit, not working set -- untouched slots
/// have no physical backing -- but a long duke run does eventually touch most of it. Allocation
/// failure still degrades rather than breaks: `ExecutableArena::new` returning `None` disables the
/// backend, and `compact_arena` returning false falls back to `reset_storage`.
///
/// 64 MiB was measured too and is NOT past the knee (317 compactions, 2.31 s, 135.276 s min wall);
/// the ladder is monotonic through 128. The remaining lever is sub-page packing: 54% of every slot
/// is still padding at a 1,870-byte mean block, which no size change recovers.
const DEFAULT_ARENA_MIB: usize = 128;
const MIN_ARENA_MIB: usize = 8;
/// Upper clamp on `IZARRAVM_JIT_ARENA_MIB`, set by two hard structural ceilings rather than taste:
///
/// - `BlockId` carries a **u16** metadata-slot index, and a live block owns exactly one arena
///   slot, so `blocks.len()` can reach the slot capacity. At the 4 KiB minimum page size a
///   256 MiB arena is exactly 65,536 slots (indices 0..=65,535 all fit u16; the first
///   `u16::try_from` failure is at index 65,536, i.e. strictly beyond 256 MiB) — so the clamp
///   below is conservative in the safe direction. Crossing the real cliff would permanently set
///   `BlockCache::disabled`, silently dropping the run to the interpreter.
/// - `DEFAULT_ENTRY_CAP` (131,072 metadata entries) is required to stay strictly above the slot
///   capacity, so that arena pressure resets compiled code before the metadata map fills
///   (`default_metadata_is_bounded_above_the_executable_arena`).
///
/// 192 MiB is 49,152 slots at 4 KiB, comfortably under both, and 6x the default. Raising this
/// further is a design slice (widen `BlockId`'s index, scale `entry_cap`), not a constant edit.
const MAX_ARENA_MIB: usize = 192;

const _: () = assert!(MAX_ARENA_MIB * 256 < 65_536);
const _: () = assert!(MIN_ARENA_MIB <= DEFAULT_ARENA_MIB && DEFAULT_ARENA_MIB <= MAX_ARENA_MIB);

/// Live sweep knob: `IZARRAVM_JIT_ARENA_MIB=<n>` sizes the executable arena at construction,
/// clamped to `MIN_ARENA_MIB..=MAX_ARENA_MIB`. Read once, cached, exactly like
/// `IZARRAVM_DECODE_CACHE_LINES`, so one release binary can run both arms of an A/B.
///
/// Guest-INERT, unlike the decode-cache knob: nothing about arena size is charged to guest bus
/// clocks or master ticks, and no block is compiled differently because of it. Only the wall
/// spent rebuilding the arena moves. The fixture frame hashes and DUKEMARK invariants must be
/// unchanged across values; if one moves, this claim is wrong and it is a bug, not a re-pin.
pub(crate) fn executable_arena_len() -> usize {
    static LEN: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LEN.get_or_init(|| arena_len_from_env(std::env::var("IZARRAVM_JIT_ARENA_MIB").ok().as_deref()))
}

/// The pure half of `executable_arena_len`, so the clamp is testable without touching a
/// process-global `OnceLock` that another test may already have initialised.
fn arena_len_from_env(value: Option<&str>) -> usize {
    let mib = value
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map(|mib| mib.clamp(MIN_ARENA_MIB, MAX_ARENA_MIB))
        .unwrap_or(DEFAULT_ARENA_MIB);
    mib * 1024 * 1024
}

/// A bounded collection of one-page executable-code spans, installed through `install`. Only
/// span BASES are valid entries; the registry records every span as `(offset, page_len)` in
/// offset order.
pub(crate) struct ExecutableArena {
    ptr: *mut u8,
    len: usize,
    page_len: usize,
    used: usize,
    sealed: usize,
    /// Sealed spans, `(offset, rounded_len)`, sorted by offset (installation order).
    spans: Vec<(usize, usize)>,
    /// Spans appended by `append_unsealed` and not yet sealed; they register into `spans`
    /// when `seal_used_prefix` succeeds.
    pending_spans: Vec<(usize, usize)>,
    /// Windows x64 unwind-info registration for every sealed span (see `jit::unwind`).
    /// `None` when unwind registration is disabled (`IZARRAVM_JIT_UNWIND=0`) or failed.
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    unwind: Option<super::unwind::ArenaUnwind>,
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
        Self::with_len(executable_arena_len())
    }

    fn with_len(total_len: usize) -> Option<Self> {
        let page_len = page_size();
        let len = total_len / page_len * page_len;
        if len == 0 {
            return None;
        }
        let unwind_enabled = cfg!(all(target_os = "windows", target_arch = "x86_64"))
            && std::env::var("IZARRAVM_JIT_UNWIND")
                .map(|v| v != "0")
                .unwrap_or(true);
        let alloc_len = if unwind_enabled { len + page_len } else { len };
        let ptr = alloc_rw(alloc_len)?;
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        let unwind = if unwind_enabled {
            // SAFETY: the page at ptr+len was just allocated RW above.
            super::unwind::ArenaUnwind::new(
                ptr,
                len,
                unsafe { ptr.add(len) },
                page_len,
                (len / page_len) as u32,
            )
        } else {
            None
        };
        Some(Self {
            ptr,
            len,
            page_len,
            used: 0,
            sealed: 0,
            spans: Vec::new(),
            pending_spans: Vec::new(),
            #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
            unwind,
        })
    }

    /// Test seam: a small arena so fill-then-compact paths run without 32MB of installs.
    #[cfg(test)]
    pub(crate) fn with_len_for_test(total_len: usize) -> Option<Self> {
        Self::with_len(total_len)
    }

    /// Copy one block into the next page and seal that page Read+Execute. The Direct backend's
    /// callers pre-reject oversized compilations, so every install is exactly one page; the span
    /// registers under its base offset, and only the base is a valid entry.
    pub(crate) fn install(&mut self, code: &[u8]) -> Option<*const u8> {
        if code.is_empty() || code.len() > self.page_len || self.sealed != self.used {
            return None;
        }
        let rounded = self.page_len;
        let offset = self.used;
        if rounded > self.len - offset {
            return None;
        }
        // SAFETY: `offset + rounded <= len`, all values are page-aligned, and this page has
        // not previously been exposed or sealed.
        let slot = unsafe { self.ptr.add(offset) };
        // SAFETY: the page has `rounded` writable bytes and `code` fits in it.
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), slot, code.len()) };
        if !flush_instruction_cache(slot, rounded) || !make_rx(slot, rounded) {
            return None;
        }
        self.used += rounded;
        self.sealed = self.used;
        self.spans.push((offset, rounded));
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        if let Some(u) = self.unwind.as_mut() {
            u.cover(offset, rounded);
        }
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
        self.pending_spans.push((offset, self.page_len));
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
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        if let Some(u) = self.unwind.as_mut() {
            for &(offset, len) in &self.pending_spans {
                u.cover(offset, len);
            }
        }
        // `sealed == 0` above means `spans` is still empty, so the pending appends (made in
        // offset order) keep the registry sorted.
        self.spans.append(&mut self.pending_spans);
        true
    }

    /// Return the entry for a slot only after its whole span has been sealed executable.
    pub(crate) fn sealed_slot_entry(&self, slot: ArenaSlot) -> Option<*const u8> {
        self.sealed_span_len_at(slot.0)
            .is_some()
            // SAFETY: a registered sealed span proves the offset lies within this allocation.
            .then(|| unsafe { self.ptr.add(slot.0) as *const u8 })
    }

    /// Whether `entry..entry+code_len` starts at a sealed span BASE and stays within that span.
    pub(crate) fn contains_sealed_span_range(&self, entry: *const u8, code_len: usize) -> bool {
        if code_len == 0 {
            return false;
        }
        let Some(offset) = (entry as usize).checked_sub(self.ptr as usize) else {
            return false;
        };
        self.sealed_span_len_at(offset)
            .is_some_and(|span_len| code_len <= span_len)
    }

    /// Whether `entry..entry+code_len` starts at, and stays within, one sealed arena slot. The
    /// one-page-cap contract for the Direct backend's callers; implemented over the span
    /// registry (every legacy one-page install is a one-page span).
    pub(crate) fn contains_sealed_slot_range(&self, entry: *const u8, code_len: usize) -> bool {
        if code_len > self.page_len {
            return false;
        }
        self.contains_sealed_span_range(entry, code_len)
    }

    /// Borrow the exact requested bytes from a validated sealed slot.
    pub(crate) fn sealed_slot_bytes(&self, entry: *const u8, code_len: usize) -> Option<&[u8]> {
        if !self.contains_sealed_slot_range(entry, code_len) {
            return None;
        }
        // SAFETY: range validation proves these bytes lie in one sealed page owned by `self`.
        Some(unsafe { std::slice::from_raw_parts(entry, code_len) })
    }

    /// The registered span length at `offset` when `offset` is a span BASE whose whole span is
    /// sealed, else `None`. Mid-span offsets (including interior page boundaries) have no
    /// registry entry and always miss.
    fn sealed_span_len_at(&self, offset: usize) -> Option<usize> {
        let index = self
            .spans
            .binary_search_by_key(&offset, |(base, _)| *base)
            .ok()?;
        let (base, span_len) = self.spans[index];
        (base + span_len <= self.sealed).then_some(span_len)
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
        // Unregister before the memory disappears; ArenaUnwind::drop deletes the table.
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        {
            self.unwind = None;
        }
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
