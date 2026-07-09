//! A small write-then-execute (W^X) memory buffer for emitted machine code. Windows and Linux
//! x86-64 get a real allocator; every other target compiles to a buffer that can never be
//! created, so the JIT compiles nothing and the interpreter runs unchanged.

/// Owns a page of memory that starts Read+Write (so the encoder can write bytes into it) and is
/// flipped to Read+Execute before any code in it runs, and never both writable and executable at
/// the same time.
pub(crate) struct ExecutableBuffer {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: the buffer holds only plain code bytes; nothing here is interior-mutable shared state.
// `CpuGsw` (which owns these via `JitBlockCache`) is not itself required to be Send/Sync today,
// but marking this Send/Sync keeps it from accidentally blocking a future requirement; the
// pointer is never aliased outside this type.
unsafe impl Send for ExecutableBuffer {}
unsafe impl Sync for ExecutableBuffer {}

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
        if !make_rx(ptr, len) {
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

impl Drop for ExecutableBuffer {
    fn drop(&mut self) {
        // SAFETY: `self.ptr`/`self.len` were produced together by `alloc_rw` in `new` and never
        // mutated afterward.
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
    }

    // Layout matches the real Win32 SYSTEM_INFO on x86-64 (48 bytes): a 4-byte
    // wProcessorArchitecture/wReserved union, dwPageSize, two 8-byte address pointers, an
    // 8-byte affinity mask, then 16 bytes of processor/allocation-granularity fields. We only
    // read dwPageSize; the rest is dead weight kept solely so GetSystemInfo (which writes the
    // full struct unconditionally) never writes past the end of `info`.
    #[repr(C)]
    struct SystemInfo {
        _processor_architecture: [u8; 4],
        page_size: u32,
        _address_and_processor_fields: [u8; 40],
    }

    const _: () = assert!(std::mem::size_of::<SystemInfo>() == 48);

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
    pub(super) unsafe fn free(_ptr: *mut u8, _len: usize) {}
}

use os::{alloc_rw, free, make_rx, page_size};

#[cfg(test)]
mod tests {
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
        let buf =
            ExecutableBuffer::new(&code).expect("allocation must succeed on a supported host");
        let f: extern "C" fn() -> i32 = unsafe { std::mem::transmute(buf.entry_ptr()) };
        assert_eq!(f(), 42);
    }

    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )))]
    #[test]
    fn unsupported_host_never_allocates() {
        let code = [0xC3];
        assert!(ExecutableBuffer::new(&code).is_none());
    }
}
