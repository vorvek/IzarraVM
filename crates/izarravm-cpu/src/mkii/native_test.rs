// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::jit::exec_mem::ExecutableBuffer;

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn caller(target: usize) -> (ExecutableBuffer, usize) {
    let mut e = Encoder::new();
    let info = prologue(&mut e);
    e.mov_r64_imm64(Reg::RAX, target as u64);
    e.call_r64(Reg::RAX);
    let return_offset = e.position();
    e.mov_r32_r32(Reg::RAX, Reg::RAX);
    epilogue(&mut e);
    let code = e.finish();
    let buffer = ExecutableBuffer::new_with_unwind(&code, &info).unwrap();
    (buffer, return_offset)
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn mkii_frame_calls_a_host_helper() {
    extern "C" fn answer() -> u32 {
        42
    }
    let (buffer, _) = caller(answer as *const () as usize);
    // SAFETY: caller emits a no-argument C function returning its helper's u32.
    let function: unsafe extern "C" fn() -> u32 =
        unsafe { std::mem::transmute(buffer.entry_ptr()) };
    assert_eq!(unsafe { function() }, 42);
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
#[test]
fn mkii_frame_windows_walk_reaches_both_generated_callers() {
    use std::cell::{Cell, RefCell};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        CONTEXT, RtlCaptureContext, RtlLookupFunctionEntry, RtlVirtualUnwind,
    };
    thread_local! {
        static CAPTURE: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
        static EXPECTED: Cell<[usize; 2]> = const { Cell::new([0; 2]) };
        static REGISTERED: Cell<[bool; 2]> = const { Cell::new([false; 2]) };
    }
    #[repr(C, align(16))]
    struct AlignedContext(CONTEXT);
    extern "C" fn capture() -> u32 {
        // SAFETY: the captured context belongs to this live stack. Registered frames use
        // the OS unwinder; a leaf frame contains its return address at RSP.
        unsafe {
            let mut aligned: AlignedContext = std::mem::zeroed();
            let context = &mut aligned.0;
            RtlCaptureContext(context);
            let mut frames = Vec::new();
            let expected = EXPECTED.get();
            let mut registered = [false; 2];
            for _ in 0..32 {
                if context.Rip == 0 {
                    break;
                }
                frames.push(context.Rip as usize);
                let mut base = 0;
                let entry = RtlLookupFunctionEntry(context.Rip, &mut base, std::ptr::null_mut());
                let generated = expected.iter().position(|pc| *pc == context.Rip as usize);
                if let Some(index) = generated {
                    registered[index] = !entry.is_null();
                    if entry.is_null() {
                        break;
                    }
                }
                if entry.is_null() {
                    context.Rip = *(context.Rsp as *const u64);
                    context.Rsp += 8;
                } else {
                    let mut data = std::ptr::null_mut();
                    let mut establisher = 0;
                    RtlVirtualUnwind(
                        0,
                        base,
                        context.Rip,
                        entry,
                        context,
                        &mut data,
                        &mut establisher,
                        std::ptr::null_mut(),
                    );
                }
                if generated == Some(1) {
                    break;
                }
            }
            REGISTERED.set(registered);
            CAPTURE.with_borrow_mut(|stored| *stored = frames);
        }
        17
    }

    let (inner, inner_return) = caller(capture as *const () as usize);
    let (outer, outer_return) = caller(inner.entry_ptr() as usize);
    EXPECTED.set([
        inner.entry_ptr() as usize + inner_return,
        outer.entry_ptr() as usize + outer_return,
    ]);
    // SAFETY: both generated frames implement the same no-argument C signature.
    let function: unsafe extern "C" fn() -> u32 = unsafe { std::mem::transmute(outer.entry_ptr()) };
    assert_eq!(unsafe { function() }, 17);
    assert_eq!(REGISTERED.get(), [true, true]);
    CAPTURE.with_borrow(|frames| {
        assert!(
            frames.contains(&(inner.entry_ptr() as usize + inner_return)),
            "{frames:x?}"
        );
        assert!(
            frames.contains(&(outer.entry_ptr() as usize + outer_return)),
            "{frames:x?}"
        );
    });
}
