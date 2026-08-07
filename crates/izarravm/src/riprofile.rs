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
//! That last sentence is only true when symbols actually loaded, and reading it
//! when they did not inverts the answer: an unsymbolized run puts ~100% in
//! "<no symbol>", which reads as "all native code" when it means "resolved
//! nothing". Both the startup line (`SymType=3` is a loaded PDB) and the report
//! header (unresolved address count and sample share) exist to make that
//! distinction impossible to miss. A healthy idle-phase run resolves ~96%.
//!
//! Resolution is nearest-PRECEDING-symbol: an address past a function's true
//! end still inherits that function's name, so hot symbol-poor bytes inflate
//! whatever small function happens to sit before them. The report header
//! always carries the beyond-extent sample share (so a healthy report is
//! distinguishable from one produced before this check existed), and the
//! BEYOND-EXTENT table shows the top flagged addresses with raw RVAs, so that
//! failure announces itself instead of minting a plausible-looking hot row.
//!
//! `IZARRAVM_RIP_PROFILE_DELAY_SECS=<n>` (default 0) delays sampling to skip
//! BIOS/DOS boot and demo load.
//!
//! Samples are tagged with the boot profiler's active phase (see
//! [`set_phase`]), so `--headless-boot-profile` gets a separate table per phase
//! instead of one total smeared across POST, boot, idle and disk load. Runs that
//! never call `set_phase` stay in phase 0 and report exactly as before.
//!
//! Suspend/resume sampling at 2 kHz costs a few percent of wall and skews
//! nothing this is used for (relative shares, not absolute wall). Never build
//! an A/B ladder against this profile.

use std::collections::HashMap;
use std::io::Write;
use std::mem;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    CONTEXT, GetThreadContext, IMAGEHLP_LINEW64, IMAGEHLP_MODULEW64, SYMBOL_INFOW,
    SYMOPT_LOAD_LINES, SYMOPT_UNDNAME, SymFromAddrW, SymGetLineFromAddrW64, SymGetModuleInfoW64,
    SymInitializeW, SymSetOptions,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, ResumeThread, SuspendThread,
};

const CONTEXT_CONTROL_AMD64: u32 = 0x0010_0001;
const SAMPLE_INTERVAL: Duration = Duration::from_micros(500);
const MAX_SAMPLES: usize = 4 << 20;

/// The phase every subsequent sample is attributed to. Written by the emulation
/// thread at a phase boundary, read by the sampler thread with each sample: a
/// relaxed `u32` either side, because a sample landing on the wrong side of a
/// boundary costs one sample out of thousands and is not worth synchronizing.
static ACTIVE_PHASE: AtomicU32 = AtomicU32::new(0);

/// Attribute subsequent samples to `phase`. `--headless-boot-profile` calls this
/// at every boundary; everything else leaves it at 0 and gets one table.
pub fn set_phase(phase: u32) {
    ACTIVE_PHASE.store(phase, Ordering::Relaxed);
}

pub struct Sampler {
    stop: Arc<AtomicBool>,
    join: JoinHandle<Vec<(u64, u32)>>,
    target: usize,
    /// Phase id -> display name, for the per-phase report headings. Empty when
    /// the caller never named any, which collapses the report to one table.
    phase_names: Vec<(u32, String)>,
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
                                (
                                    "SuspendThread",
                                    windows_sys::Win32::Foundation::GetLastError(),
                                )
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
                                    (
                                        "GetThreadContext",
                                        windows_sys::Win32::Foundation::GetLastError(),
                                    )
                                });
                            }
                            (ok != 0).then_some(ctx.0.Rip)
                        }
                    };
                    if let Some(rip) = rip {
                        samples.push((rip, ACTIVE_PHASE.load(Ordering::Relaxed)));
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
        ACTIVE_PHASE.store(0, Ordering::Relaxed);
        Some(Self {
            stop,
            join,
            target: target_addr,
            phase_names: Vec::new(),
        })
    }

    /// Name the phases this run will pass through, so the report can head each
    /// table with something readable. Ids not named here still report, under
    /// their number.
    pub fn name_phases(&mut self, names: &[(u32, &str)]) {
        self.phase_names = names
            .iter()
            .map(|&(id, name)| (id, name.to_string()))
            .collect();
    }

    pub fn stop_and_report(self, out_path: &Path) {
        self.stop.store(true, Ordering::Relaxed);
        let samples = self.join.join().unwrap_or_default();
        unsafe { CloseHandle(self.target as HANDLE) };
        if samples.is_empty() {
            eprintln!("riprofile: no samples collected");
            return;
        }
        // Resolve symbols once over the whole run, then attribute per phase: an
        // address costs a dbghelp round trip, and the same address recurs in
        // every phase.
        let mut counts: HashMap<u64, u64> = HashMap::new();
        let mut per_phase: HashMap<u32, HashMap<u64, u64>> = HashMap::new();
        for &(rip, phase) in &samples {
            *counts.entry(rip).or_default() += 1;
            *per_phase.entry(phase).or_default().entry(rip).or_default() += 1;
        }

        let process = unsafe { GetCurrentProcess() };
        let module_base = unsafe { GetModuleHandleW(std::ptr::null()) } as u64;
        unsafe {
            // NO `SYMOPT_DEFERRED_LOADS`: deferred modules report `SymType = 5`
            // (SymDeferred) and only try to find a PDB on the first query, so a
            // missing or mismatched PDB surfaced as a per-address failure rather
            // than as one diagnosable message at init. Loading eagerly costs one
            // PDB parse at report time, off the measured path entirely.
            SymSetOptions(SYMOPT_LOAD_LINES | SYMOPT_UNDNAME);

            // The exe's own directory as the search path. dbghelp's default
            // (NULL) is the CWD plus `_NT_SYMBOL_PATH`, and neither has to
            // contain `target/profiling`, so a run started from anywhere but the
            // build directory could only find the PDB through the absolute path
            // baked into the image's debug directory.
            let mut path = [0u16; 1024];
            let len = GetModuleFileNameW(std::ptr::null_mut(), path.as_mut_ptr(), 1024);
            let exe_dir = exe_directory(&path[..len as usize]);
            let search = exe_dir.map_or(std::ptr::null(), |dir| dir.as_ptr());
            if SymInitializeW(process, search, 1) == 0 {
                eprintln!("riprofile: SymInitializeW failed; dumping raw addresses");
            }

            // `fInvadeProcess = 1` above ALREADY registered every loaded module,
            // this exe included. Do NOT also call `SymLoadModuleExW` for it: a
            // second registration at the same base re-registers the module with
            // no symbol source, flipping SymType from SymPdb to 0 (SymNone), and
            // every later `SymFromAddrW` inside the image then fails with 487
            // (ERROR_INVALID_ADDRESS). That is what made the whole report land in
            // the "<no symbol>" bucket, which reads as "all JIT arena" and is the
            // exact opposite of the truth.
            let mut info: IMAGEHLP_MODULEW64 = mem::zeroed();
            info.SizeOfStruct = mem::size_of::<IMAGEHLP_MODULEW64>() as u32;
            if SymGetModuleInfoW64(process, module_base, &mut info) != 0 {
                // SymType 3 is SymPdb. Anything else means the report's
                // "<no symbol>" bucket is unresolved addresses, not native code,
                // so say so rather than letting it be misread.
                eprintln!(
                    "riprofile: exe module base {module_base:#x}, SymType={} (3=Pdb), lines={}",
                    info.SymType, info.LineNumbers
                );
                if info.SymType != 3 {
                    eprintln!(
                        "riprofile: WARNING no PDB loaded -- '<no symbol>' below means \
                         UNRESOLVED, not JIT arena. Build with `--profile profiling`."
                    );
                }
            } else {
                eprintln!(
                    "riprofile: SymGetModuleInfoW64 failed (GetLastError={})",
                    windows_sys::Win32::Foundation::GetLastError()
                );
            }
        }

        let mut resolved: HashMap<u64, (String, String, String)> = HashMap::new();
        // Counted, not sampled: `counts` is a HashMap, so "the first address that
        // failed" was whichever one iteration happened to reach first, and a
        // single JIT-arena address failing is normal. Only the ratio says whether
        // symbolization is healthy.
        let mut unresolved_addrs = 0u64;
        let mut unresolved_samples = 0u64;
        let mut beyond_samples = 0u64;
        // (samples, rip, name, displacement, recorded size) for every address
        // past its symbol's extent. These stay in the main tables under the
        // preceding symbol's name — the 2026-08-06 doom-586 report is only
        // comparable if attribution semantics hold still — and are ALSO listed
        // raw so a hot symbol-poor gap can be seen instead of inferred.
        let mut beyond_rows: Vec<(u64, u64, String, u64, u32)> = Vec::new();
        for (&rip, &n) in &counts {
            let symbol = resolve_symbol(process, rip);
            if symbol.is_none() {
                unresolved_addrs += 1;
                unresolved_samples += n;
            }
            let (func, displacement, size) =
                symbol.unwrap_or_else(|| ("<no symbol — JIT arena or foreign code>".into(), 0, 0));
            if beyond_extent(displacement, size) {
                beyond_samples += n;
                beyond_rows.push((n, rip, func.clone(), displacement, size));
            }
            let site =
                resolve_line(process, rip).unwrap_or_else(|| format!("{func} (no line info)"));
            let file = site
                .rsplit_once(':')
                .map_or(site.clone(), |(f, _)| f.into());
            resolved.insert(rip, (func, site, file));
        }

        let total = samples.len() as u64;
        eprintln!(
            "riprofile: {unresolved_addrs}/{} addresses unresolved, {:.2}% of samples",
            counts.len(),
            unresolved_samples as f64 * 100.0 / total.max(1) as f64,
        );
        eprintln!(
            "riprofile: {:.2}% of samples beyond the attributed symbol's extent",
            beyond_samples as f64 * 100.0 / total.max(1) as f64,
        );
        let mut report = String::new();
        report.push_str(&format!(
            "riprofile report — {total} samples at {SAMPLE_INTERVAL:?} \
             ({} unique addresses, {unresolved_addrs} unresolved carrying \
             {:.2}% of samples, {:.2}% of samples beyond their symbol's \
             recorded extent)\n\n",
            counts.len(),
            unresolved_samples as f64 * 100.0 / total.max(1) as f64,
            beyond_samples as f64 * 100.0 / total.max(1) as f64,
        ));
        report.push_str(&render_tables(&counts, &resolved));

        if !beyond_rows.is_empty() {
            beyond_rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            report.push_str(&format!(
                "== BEYOND-EXTENT — {:.2}% of samples sit past the recorded end of the \
                 symbol the tables above attribute them to; those rows are guesses ==\n\
                 (top {} of {} flagged addresses; rva = address - exe base \
                 {module_base:#x}, for llvm-pdbutil)\n",
                beyond_samples as f64 * 100.0 / total.max(1) as f64,
                beyond_rows.len().min(40),
                beyond_rows.len(),
            ));
            for (n, rip, func, displacement, size) in beyond_rows.iter().take(40) {
                report.push_str(&format!(
                    "{:>7.3}%  {:>9}  {rip:#014x}  rva {:#011x}  \
                     +{displacement:#x} past {func} (size {size:#x})\n",
                    *n as f64 * 100.0 / total.max(1) as f64,
                    n,
                    rip.wrapping_sub(module_base),
                ));
            }
            report.push('\n');
        }

        // Per-phase tables, in phase order. Only emitted when the run actually
        // crossed a boundary: a single-phase run would just repeat the total.
        let mut phases: Vec<u32> = per_phase.keys().copied().collect();
        phases.sort_unstable();
        if phases.len() > 1 {
            for phase in phases {
                let Some(phase_counts) = per_phase.get(&phase) else {
                    continue;
                };
                let phase_total: u64 = phase_counts.values().sum();
                let name = self
                    .phase_names
                    .iter()
                    .find(|(id, _)| *id == phase)
                    .map(|(_, name)| name.as_str())
                    .unwrap_or("unnamed");
                report.push_str(&format!(
                    "\n################ PHASE {phase} ({name}) — {phase_total} samples, \
                     {:.2}% of the run ################\n\n",
                    phase_total as f64 * 100.0 / total as f64,
                ));
                report.push_str(&render_tables(phase_counts, &resolved));
            }
        }
        match std::fs::File::create(out_path).and_then(|mut f| f.write_all(report.as_bytes())) {
            Ok(()) => eprintln!("riprofile: report written to {}", out_path.display()),
            Err(e) => {
                eprintln!(
                    "riprofile: could not write {}: {e}; report follows\n{report}",
                    out_path.display()
                );
            }
        }
    }
}

/// Render the three ranked tables for one sample set. `resolved` maps an address
/// to its `(function, file:line, file)` triple, resolved once for the whole run.
fn render_tables(
    counts: &HashMap<u64, u64>,
    resolved: &HashMap<u64, (String, String, String)>,
) -> String {
    let mut by_func: HashMap<&str, u64> = HashMap::new();
    let mut by_site: HashMap<&str, u64> = HashMap::new();
    let mut by_file: HashMap<&str, u64> = HashMap::new();
    let mut total = 0u64;
    for (&rip, &n) in counts {
        total += n;
        let Some((func, site, file)) = resolved.get(&rip) else {
            continue;
        };
        *by_func.entry(func.as_str()).or_default() += n;
        *by_site.entry(site.as_str()).or_default() += n;
        *by_file.entry(file.as_str()).or_default() += n;
    }
    let denominator = total.max(1) as f64;
    let mut out = String::new();
    for (title, map, top) in [
        ("BY FUNCTION", &by_func, 60),
        ("BY FILE", &by_file, 40),
        ("BY FILE:LINE", &by_site, 120),
    ] {
        let mut rows: Vec<_> = map.iter().collect();
        // Ties by name so two equal-weight rows cannot swap between runs.
        rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        out.push_str(&format!("== {title} ==\n"));
        for (name, n) in rows.into_iter().take(top) {
            out.push_str(&format!(
                "{:>7.3}%  {:>9}  {}\n",
                *n as f64 * 100.0 / denominator,
                n,
                name
            ));
        }
        out.push('\n');
    }
    out
}

/// The directory part of a UTF-16 module path, NUL-terminated for Win32. `None`
/// when the path is empty or has no separator, which leaves the search path at
/// dbghelp's default rather than pointing it somewhere wrong.
fn exe_directory(path: &[u16]) -> Option<Vec<u16>> {
    const SEP: u16 = b'\\' as u16;
    const ALT_SEP: u16 = b'/' as u16;
    let cut = path.iter().rposition(|&c| c == SEP || c == ALT_SEP)?;
    if cut == 0 {
        return None;
    }
    let mut dir = path[..cut].to_vec();
    dir.push(0);
    Some(dir)
}

#[cfg(test)]
#[path = "riprofile_test.rs"]
mod tests;

/// Resolve `rip` to `(name, displacement, recorded_size)`. dbghelp answers with
/// the nearest PRECEDING symbol however far past its end the address lies; the
/// caller decides whether the displacement clears the recorded extent.
fn resolve_symbol(process: HANDLE, rip: u64) -> Option<(String, u64, u32)> {
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
        (String::from_utf16_lossy(name), displacement, buf.info.Size)
    })
}

/// Whether a resolved RIP fell past the recorded extent of the symbol dbghelp
/// attributed it to, which means the attribution is a guess about the gap after
/// that symbol rather than the symbol itself. A zero recorded size means the
/// PDB carried no extent and proves nothing either way; only a nonzero extent
/// the displacement clears is evidence of misattribution.
fn beyond_extent(displacement: u64, size: u32) -> bool {
    size > 0 && displacement >= u64::from(size)
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
        let file =
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(line.FileName, len) });
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
