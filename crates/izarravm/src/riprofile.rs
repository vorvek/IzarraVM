// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! TEMPORARY diagnostic tooling — statistical RIP sampler for headless runs.
//!
//! Gated by `IZARRAVM_RIP_PROFILE=<report-path>`. A sampler thread suspends the
//! emulation (main) thread at ~2 kHz, records the instruction pointer, and at
//! the end of the run resolves every unique address to a function name and a
//! file:line via dbghelp (requires an unstripped build with debug info — use
//! `--profile profiling`). Addresses that resolve to no symbol are almost
//! entirely the Direct JIT's code arena, so the "<no symbol>" bucket doubles as
//! the native-code share of wall time.
//!
//! `IZARRAVM_RIP_PROFILE_DELAY_SECS=<n>` (default 0) delays sampling to skip
//! BIOS/DOS boot and demo load.
//!
//! Suspend/resume sampling at 2 kHz costs a few percent of wall and skews
//! nothing this is used for (relative shares, not absolute wall). Never build
//! an A/B ladder against this profile.

use std::collections::HashMap;
use std::io::Write;
use std::mem;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    GetThreadContext, SymFromAddrW, SymGetLineFromAddrW64, SymInitializeW, SymSetOptions, CONTEXT,
    IMAGEHLP_LINEW64, SYMBOL_INFOW, SYMOPT_DEFERRED_LOADS, SYMOPT_LOAD_LINES, SYMOPT_UNDNAME,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    SymGetModuleInfoW64, SymLoadModuleExW, IMAGEHLP_MODULEW64,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, ResumeThread, SuspendThread,
};

const CONTEXT_CONTROL_AMD64: u32 = 0x0010_0001;
const SAMPLE_INTERVAL: Duration = Duration::from_micros(500);
const MAX_SAMPLES: usize = 4 << 20;

pub struct Sampler {
    stop: Arc<AtomicBool>,
    join: JoinHandle<Vec<u64>>,
    target: usize,
}

impl Sampler {
    /// Duplicate the calling thread's handle and start sampling it. Call from
    /// the emulation thread itself, immediately before the run loop.
    pub fn start() -> Option<Self> {
        let delay = std::env::var("IZARRAVM_RIP_PROFILE_DELAY_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let process = unsafe { GetCurrentProcess() };
        let mut target: HANDLE = std::ptr::null_mut();
        let ok = unsafe {
            DuplicateHandle(
                process,
                GetCurrentThread(),
                process,
                &mut target,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            eprintln!("riprofile: DuplicateHandle failed; sampler disabled");
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stop_seen = Arc::clone(&stop);
        let target_addr = target as usize;
        let join = std::thread::Builder::new()
            .name("rip-sampler".into())
            .spawn(move || {
                // The Win32 ABI requires CONTEXT to be 16-byte aligned;
                // windows-sys's struct declaration alone does not guarantee
                // that on the stack, and a misaligned buffer makes
                // GetThreadContext fail on every call.
                #[repr(C, align(16))]
                struct AlignedContext(CONTEXT);
                let target = target_addr as HANDLE;
                let mut samples = Vec::with_capacity(1 << 20);
                let mut suspend_failures = 0u64;
                let mut context_failures = 0u64;
                let mut first_error = None;
                std::thread::sleep(Duration::from_secs(delay));
                while !stop_seen.load(Ordering::Relaxed) && samples.len() < MAX_SAMPLES {
                    std::thread::sleep(SAMPLE_INTERVAL);
                    // SAFETY: the target thread is suspended only across the
                    // GetThreadContext call; no allocation or locking happens
                    // while it is suspended, so it cannot deadlock against us.
                    let rip = unsafe {
                        if SuspendThread(target) == u32::MAX {
                            suspend_failures += 1;
                            first_error.get_or_insert_with(|| {
                                ("SuspendThread", windows_sys::Win32::Foundation::GetLastError())
                            });
                            None
                        } else {
                            let mut ctx: AlignedContext = mem::zeroed();
                            ctx.0.ContextFlags = CONTEXT_CONTROL_AMD64;
                            let ok = GetThreadContext(target, &mut ctx.0);
                            ResumeThread(target);
                            if ok == 0 {
                                context_failures += 1;
                                first_error.get_or_insert_with(|| {
                                    ("GetThreadContext", windows_sys::Win32::Foundation::GetLastError())
                                });
                            }
                            (ok != 0).then_some(ctx.0.Rip)
                        }
                    };
                    if let Some(rip) = rip {
                        samples.push(rip);
                    }
                }
                if suspend_failures + context_failures > 0 {
                    let (call, code) = first_error.unwrap_or(("?", 0));
                    eprintln!(
                        "riprofile: {suspend_failures} suspend / {context_failures} context \
                         failures (first: {call} GetLastError={code})"
                    );
                }
                samples
            })
            .ok()?;
        eprintln!("riprofile: sampling armed (delay {delay}s, interval {SAMPLE_INTERVAL:?})");
        Some(Self { stop, join, target: target_addr })
    }

    pub fn stop_and_report(self, out_path: &Path) {
        self.stop.store(true, Ordering::Relaxed);
        let samples = self.join.join().unwrap_or_default();
        unsafe { CloseHandle(self.target as HANDLE) };
        if samples.is_empty() {
            eprintln!("riprofile: no samples collected");
            return;
        }
        let mut counts: HashMap<u64, u64> = HashMap::new();
        for s in &samples {
            *counts.entry(*s).or_default() += 1;
        }

        let process = unsafe { GetCurrentProcess() };
        unsafe {
            SymSetOptions(SYMOPT_LOAD_LINES | SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS);
            if SymInitializeW(process, std::ptr::null(), 1) == 0 {
                eprintln!("riprofile: SymInitializeW failed; dumping raw addresses");
            }
            // Invade-process enumeration can leave the main exe resolved by
            // exports only (a Rust exe has none). Force-load its PDB.
            let base = GetModuleHandleW(std::ptr::null());
            let mut path = [0u16; 1024];
            let len = GetModuleFileNameW(std::ptr::null_mut(), path.as_mut_ptr(), 1024);
            if len > 0 {
                let loaded = SymLoadModuleExW(
                    process,
                    std::ptr::null_mut(),
                    path.as_ptr(),
                    std::ptr::null(),
                    base as u64,
                    0,
                    std::ptr::null(),
                    0,
                );
                if loaded == 0 {
                    let e = windows_sys::Win32::Foundation::GetLastError();
                    if e != 0 {
                        eprintln!("riprofile: SymLoadModuleExW failed (GetLastError={e})");
                    }
                }
                let mut info: IMAGEHLP_MODULEW64 = mem::zeroed();
                info.SizeOfStruct = mem::size_of::<IMAGEHLP_MODULEW64>() as u32;
                if SymGetModuleInfoW64(process, base as u64, &mut info) != 0 {
                    eprintln!(
                        "riprofile: exe module base {:#x}, SymType={}, lines={}",
                        base as u64, info.SymType, info.LineNumbers
                    );
                } else {
                    eprintln!(
                        "riprofile: SymGetModuleInfoW64 failed (GetLastError={})",
                        windows_sys::Win32::Foundation::GetLastError()
                    );
                }
            }
        }

        let mut by_func: HashMap<String, u64> = HashMap::new();
        let mut by_site: HashMap<String, u64> = HashMap::new();
        let mut by_file: HashMap<String, u64> = HashMap::new();
        let mut first_resolve_error = true;
        for (&rip, &n) in &counts {
            if first_resolve_error && resolve_symbol(process, rip).is_none() {
                first_resolve_error = false;
                eprintln!(
                    "riprofile: first failed SymFromAddrW rip={rip:#x} GetLastError={}",
                    unsafe { windows_sys::Win32::Foundation::GetLastError() }
                );
            }
            let func = resolve_symbol(process, rip)
                .unwrap_or_else(|| "<no symbol — JIT arena or foreign code>".into());
            let site = resolve_line(process, rip)
                .unwrap_or_else(|| format!("{func} (no line info)"));
            let file = site.rsplit_once(':').map_or(site.clone(), |(f, _)| f.into());
            *by_func.entry(func).or_default() += n;
            *by_site.entry(site).or_default() += n;
            *by_file.entry(file).or_default() += n;
        }

        let total = samples.len() as u64;
        let mut report = String::new();
        report.push_str(&format!(
            "riprofile report — {total} samples at {SAMPLE_INTERVAL:?} \
             ({} unique addresses)\n\n",
            counts.len()
        ));
        for (title, map, top) in [
            ("BY FUNCTION", &by_func, 60),
            ("BY FILE", &by_file, 40),
            ("BY FILE:LINE", &by_site, 120),
        ] {
            let mut rows: Vec<_> = map.iter().collect();
            rows.sort_by(|a, b| b.1.cmp(a.1));
            report.push_str(&format!("== {title} ==\n"));
            for (name, n) in rows.into_iter().take(top) {
                report.push_str(&format!("{:>7.3}%  {:>9}  {}\n", *n as f64 * 100.0 / total as f64, n, name));
            }
            report.push('\n');
        }
        match std::fs::File::create(out_path).and_then(|mut f| f.write_all(report.as_bytes())) {
            Ok(()) => eprintln!("riprofile: report written to {}", out_path.display()),
            Err(e) => {
                eprintln!("riprofile: could not write {}: {e}; report follows\n{report}", out_path.display());
            }
        }
    }
}

fn resolve_symbol(process: HANDLE, rip: u64) -> Option<String> {
    const MAX_NAME: usize = 512;
    #[repr(C)]
    struct SymbolBuf {
        info: SYMBOL_INFOW,
        _name_tail: [u16; MAX_NAME],
    }
    let mut buf: SymbolBuf = unsafe { mem::zeroed() };
    buf.info.SizeOfStruct = mem::size_of::<SYMBOL_INFOW>() as u32;
    buf.info.MaxNameLen = MAX_NAME as u32;
    let mut displacement = 0u64;
    let ok = unsafe { SymFromAddrW(process, rip, &mut displacement, &mut buf.info) };
    (ok != 0).then(|| {
        let len = (buf.info.NameLen as usize).min(MAX_NAME);
        let name = unsafe { std::slice::from_raw_parts(buf.info.Name.as_ptr(), len) };
        String::from_utf16_lossy(name)
    })
}

fn resolve_line(process: HANDLE, rip: u64) -> Option<String> {
    let mut line: IMAGEHLP_LINEW64 = unsafe { mem::zeroed() };
    line.SizeOfStruct = mem::size_of::<IMAGEHLP_LINEW64>() as u32;
    let mut displacement = 0u32;
    let ok = unsafe { SymGetLineFromAddrW64(process, rip, &mut displacement, &mut line) };
    (ok != 0).then(|| {
        let mut len = 0usize;
        while unsafe { *line.FileName.add(len) } != 0 {
            len += 1;
        }
        let file = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(line.FileName, len) });
        // Keep paths readable: everything from the last `crates` component on,
        // or the bare filename for std/vendored sources.
        let trimmed = file
            .rfind("crates\\")
            .or_else(|| file.rfind("crates/"))
            .map(|i| &file[i..])
            .unwrap_or_else(|| file.rsplit(['\\', '/']).next().unwrap_or(&file));
        format!("{trimmed}:{}", line.LineNumber)
    })
}
