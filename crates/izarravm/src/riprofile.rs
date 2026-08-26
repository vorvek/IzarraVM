// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! TEMPORARY diagnostic tooling — statistical RIP sampler for headless runs.
//!
//! Gated by `IZARRAVM_RIP_PROFILE=<report-path>`. A sampler thread suspends the
//! emulation (main) thread at ~2 kHz, records the instruction pointer, and at
//! the end of the run resolves every unique address to a function name and a
//! file:line via dbghelp (requires an unstripped build with debug info —
//! `--profile release` already carries line-tables, and `--profile profiling`
//! carries full debug info; the two are NOT byte-identical, see the root
//! `Cargo.toml`, and shares of wall must not be compared across them).
//! Addresses that resolve to no symbol are almost entirely the Direct JIT's
//! code arena, so the "<no symbol>" bucket doubles as the native-code share of
//! wall time.
//!
//! That last sentence is only true when symbols actually loaded, and reading it
//! when they did not inverts the answer: an unsymbolized run puts ~100% in
//! "<no symbol>", which reads as "all native code" when it means "resolved
//! nothing". The startup line (`SymType=3` is a loaded PDB) says so, and
//! **a degraded load (`SymType != 3` or no line numbers) makes the report
//! refuse to write at all** rather than emit a plausible, wrong, empty answer:
//! zero inline frames from a load that never found its PDB reads as "nothing
//! is inlined" when it means "nothing was loaded". A healthy idle-phase run
//! resolves ~96%.
//!
//! Resolution is nearest-PRECEDING-symbol: an address past a function's true
//! end still inherits that function's name, so hot symbol-poor bytes inflate
//! whatever small function happens to sit before them. The report header
//! always carries the beyond-extent sample share (so a healthy report is
//! distinguishable from one produced before this check existed), and the
//! BEYOND-EXTENT table shows the top flagged addresses with raw RVAs, so that
//! failure announces itself instead of minting a plausible-looking hot row.
//!
//! **Inline-frame resolution.** Neither `SymFromAddrW` nor
//! `SymGetLineFromAddrW64` expand inline frames: an inlined callee's samples
//! land on the enclosing (physical) function and its call-site line. Every
//! address is therefore also resolved through `SymAddrIncludeInlineTrace` /
//! `SymQueryInlineTrace` / `SymFromInlineContextW` /
//! `SymGetLineFromInlineContextW`, which walk the inline chain the physical
//! function's call actually took. The physical `== BY FUNCTION ==`, `== BY
//! FILE ==` and `== BY FILE:LINE ==` tables are UNCHANGED (same
//! `SymFromAddrW`/`SymGetLineFromAddrW64` calls as before this existed, so old
//! and new reports stay comparable), and two new tables report the inline
//! chain under their own headings:
//!
//! - `== INLINE EXCLUSIVE BY FUNCTION ==` charges each sample to the
//!   INNERMOST frame only (the deepest inline frame, or the physical function
//!   when nothing was inlined at that address) and sums to 100%.
//! - `== INLINE INCLUSIVE BY FUNCTION ==` charges each sample to EVERY frame
//!   in its chain and sums to more than 100%: it answers what a frame's
//!   subtree costs, not who owns the time.
//! - `== CHAIN DEPTH HISTOGRAM ==` reports how many inline frames each sample
//!   actually returned (0 = nothing inlined at that address), so a reader can
//!   see how deep the traces went instead of assuming it.
//!
//! No splitting variant is computed; splitting would invent a distribution the
//! samples do not carry.
//!
//! **The report header states the ACHIEVED mean sample interval, not just the
//! nominal one.** `SAMPLE_INTERVAL` below is a nominal 500us, but
//! `std::thread::sleep` cannot deliver that on Windows: the achieved rate is
//! close to 1.04ms (measured: 33,499 nascar-586 samples over a 34.952s window
//! is 1.0434ms/sample, 2.09x nominal). Every report before this measured the
//! window from first sample to last and printed the true mean interval beside
//! the requested one, so a reader is never left assuming the nominal figure
//! held.
//!
//! **Raw-sample sidecar.** `stop_and_report` also writes `<report>.samples`
//! (after the run ends, never during, so it cannot perturb a measurement): one
//! `rip_hex,phase,count` row per unique address/phase pair actually sampled,
//! aggregated from the same in-process counts the report itself is built
//! from. This lets a profile be re-cut (new tables, a different question)
//! without re-running a 20-to-60-second fixture. It stores raw process
//! addresses under this run's own module base (recorded in the header
//! comment), not RVAs against a named PDB; resolving a sidecar against a
//! DIFFERENT binary is future work and is not implemented or guarded here.
//!
//! **Physical conservation.** Every address is resolved BOTH by the old
//! `SymFromAddrW` call and, independently, by the new
//! `SymFromInlineContextW(..., INLINE_FRAME_CONTEXT_IGNORE, ...)` call that
//! also anchors the inline-chain walk. If the two disagree on a name or on
//! beyond-extent status for any address, that is the resolver's fault, not the
//! guest's, and the report says so loudly in its own section rather than
//! silently trusting the new path.
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
//!
//! Everything this file adds beyond the original resolver runs in
//! `stop_and_report`, strictly after `self.join.join()`, i.e. after the
//! emulation thread has stopped. Nothing here executes while the guest runs.

use std::collections::HashMap;
use std::io::Write;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    CONTEXT, GetThreadContext, IMAGEHLP_LINEW64, IMAGEHLP_MODULEW64, INLINE_FRAME_CONTEXT_IGNORE,
    INLINE_FRAME_CONTEXT_INIT, SYMBOL_INFOW, SYMOPT_LOAD_LINES, SYMOPT_UNDNAME,
    SymAddrIncludeInlineTrace, SymFromAddrW, SymFromInlineContextW, SymGetLineFromAddrW64,
    SymGetLineFromInlineContextW, SymGetModuleInfoW64, SymInitializeW, SymQueryInlineTrace,
    SymSetOptions,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, ResumeThread, SuspendThread,
};

const CONTEXT_CONTROL_AMD64: u32 = 0x0010_0001;
const SAMPLE_INTERVAL: Duration = Duration::from_micros(500);
const MAX_SAMPLES: usize = 4 << 20;

/// `IMAGEHLP_MODULEW64.SymType` value for a fully loaded PDB. Anything else
/// means the "<no symbol>" bucket is unresolved addresses, not native code,
/// and (see [`symbols_are_trustworthy`]) means inline-frame resolution must
/// refuse rather than silently report zero inline frames everywhere.
const SYM_PDB: i32 = 3;

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

/// The sampler thread's raw output: the collected `(rip, phase)` pairs plus
/// the wall-clock span they actually cover. The span is what lets
/// `stop_and_report` print the ACHIEVED sample interval instead of the
/// nominal `SAMPLE_INTERVAL`, which `std::thread::sleep` cannot deliver on
/// Windows (see the module doc).
#[derive(Default)]
struct RawSamples {
    samples: Vec<(u64, u32)>,
    first_at: Option<Instant>,
    last_at: Option<Instant>,
}

pub struct Sampler {
    stop: Arc<AtomicBool>,
    join: JoinHandle<RawSamples>,
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
                let mut raw = RawSamples::default();
                raw.samples.reserve(1 << 20);
                let mut suspend_failures = 0u64;
                let mut context_failures = 0u64;
                let mut first_error = None;
                std::thread::sleep(Duration::from_secs(delay));
                while !stop_seen.load(Ordering::Relaxed) && raw.samples.len() < MAX_SAMPLES {
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
                        let now = Instant::now();
                        raw.first_at.get_or_insert(now);
                        raw.last_at = Some(now);
                        raw.samples
                            .push((rip, ACTIVE_PHASE.load(Ordering::Relaxed)));
                    }
                }
                if suspend_failures + context_failures > 0 {
                    let (call, code) = first_error.unwrap_or(("?", 0));
                    eprintln!(
                        "riprofile: {suspend_failures} suspend / {context_failures} context \
                         failures (first: {call} GetLastError={code})"
                    );
                }
                raw
            })
            .ok()?;
        eprintln!(
            "riprofile: sampling armed (delay {delay}s, interval {SAMPLE_INTERVAL:?} requested)"
        );
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
        let raw = self.join.join().unwrap_or_default();
        unsafe { CloseHandle(self.target as HANDLE) };
        let samples = raw.samples;
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
            // contain the build directory, so a run started from anywhere but
            // it could only find the PDB through the absolute path baked into
            // the image's debug directory.
            let mut path = [0u16; 1024];
            let len = GetModuleFileNameW(std::ptr::null_mut(), path.as_mut_ptr(), 1024);
            let exe_dir = exe_directory(&path[..len as usize]);
            let search = exe_dir.map_or(std::ptr::null(), |dir| dir.as_ptr());
            if SymInitializeW(process, search, 1) == 0 {
                eprintln!("riprofile: SymInitializeW failed; dumping raw addresses");
            }
        }

        // `fInvadeProcess = 1` above ALREADY registered every loaded module,
        // this exe included. Do NOT also call `SymLoadModuleExW` for it: a
        // second registration at the same base re-registers the module with
        // no symbol source, flipping SymType from SymPdb to 0 (SymNone), and
        // every later `SymFromAddrW` inside the image then fails with 487
        // (ERROR_INVALID_ADDRESS). That is what made the whole report land in
        // the "<no symbol>" bucket, which reads as "all JIT arena" and is the
        // exact opposite of the truth.
        let mut info: IMAGEHLP_MODULEW64 = unsafe { mem::zeroed() };
        info.SizeOfStruct = mem::size_of::<IMAGEHLP_MODULEW64>() as u32;
        let module_ok = unsafe { SymGetModuleInfoW64(process, module_base, &mut info) } != 0;
        if module_ok {
            // SymType 3 is SymPdb. Anything else means the report's
            // "<no symbol>" bucket is unresolved addresses, not native code,
            // so say so rather than letting it be misread.
            eprintln!(
                "riprofile: exe module base {module_base:#x}, SymType={} (3=Pdb), lines={}",
                info.SymType, info.LineNumbers
            );
        } else {
            eprintln!(
                "riprofile: SymGetModuleInfoW64 failed (GetLastError={})",
                unsafe { windows_sys::Win32::Foundation::GetLastError() }
            );
        }
        if !module_ok || !symbols_are_trustworthy(info.SymType, info.LineNumbers) {
            eprintln!(
                "riprofile: REFUSING to write a report -- a degraded symbol load \
                 (SymType={}, lines={}) resolves zero inline frames, which reads as \
                 \"nothing is inlined\" when it means \"nothing was loaded\". Build \
                 `--profile release` or `--profile profiling` and run the exe from its \
                 own directory (or set `_NT_SYMBOL_PATH`) so the PDB is found; no report \
                 was written.",
                info.SymType, info.LineNumbers
            );
            return;
        }

        let mut resolved: HashMap<u64, (String, String, String)> = HashMap::new();
        let mut inline_resolved: HashMap<u64, InlineResolution> = HashMap::new();
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
        // Physical conservation: every address is resolved by the OLD
        // `SymFromAddrW` path (above, `symbol`) and INDEPENDENTLY by the NEW
        // `SymFromInlineContextW(..., IGNORE, ...)` path that also anchors the
        // inline-chain walk. A disagreement here is the resolver's fault.
        let mut conservation_mismatches: Vec<(u64, String)> = Vec::new();
        for (&rip, &n) in &counts {
            let symbol = resolve_symbol(process, rip);
            if symbol.is_none() {
                unresolved_addrs += 1;
                unresolved_samples += n;
            }
            let (func, displacement, size) = symbol
                .clone()
                .unwrap_or_else(|| ("<no symbol — JIT arena or foreign code>".into(), 0, 0));
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

            let physical_new = resolve_physical(process, rip);
            if let Some(reason) = conservation_mismatch(symbol.as_ref(), physical_new.as_ref()) {
                conservation_mismatches.push((rip, reason));
            }
            let chain = resolve_inline_chain(process, rip);
            inline_resolved.insert(
                rip,
                InlineResolution {
                    physical_new,
                    chain,
                },
            );
        }

        if conservation_mismatches.is_empty() {
            eprintln!(
                "riprofile: physical conservation check passed ({} addresses cross-checked \
                 between SymFromAddrW and SymFromInlineContextW(IGNORE))",
                counts.len()
            );
        } else {
            eprintln!(
                "riprofile: PHYSICAL CONSERVATION CHECK FAILED -- {} of {} addresses disagree \
                 between the old and the re-derived resolver; first: {:x?}",
                conservation_mismatches.len(),
                counts.len(),
                conservation_mismatches.first(),
            );
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
        let window = match (raw.first_at, raw.last_at) {
            (Some(f), Some(l)) => Some(l.duration_since(f)),
            _ => None,
        };
        let achieved = achieved_interval(samples.len(), window.unwrap_or_default());
        let mut report = String::new();
        report.push_str(&format!(
            "riprofile report — {} ({} unique addresses, {unresolved_addrs} unresolved \
             carrying {:.2}% of samples, {:.2}% of samples beyond their symbol's recorded \
             extent)\n\n",
            sample_rate_line(total, window, achieved),
            counts.len(),
            unresolved_samples as f64 * 100.0 / total.max(1) as f64,
            beyond_samples as f64 * 100.0 / total.max(1) as f64,
        ));
        if !conservation_mismatches.is_empty() {
            report.push_str(&format!(
                "== CONSERVATION CHECK: FAILED -- {} of {} addresses disagree between the old \
                 SymFromAddrW resolver and the re-derived SymFromInlineContextW(IGNORE) one; \
                 the resolver is wrong, not the guest ==\n",
                conservation_mismatches.len(),
                counts.len(),
            ));
            for (rip, reason) in conservation_mismatches.iter().take(20) {
                report.push_str(&format!("  {rip:#014x}  {reason}\n"));
            }
            report.push('\n');
        }
        report.push_str(&render_tables(&counts, &resolved, &inline_resolved));

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
                report.push_str(&render_tables(phase_counts, &resolved, &inline_resolved));
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

        // The raw-sample sidecar: written last, after the report, and never
        // read back by anything in this run -- see the module doc.
        let samples_path = sidecar_path(out_path);
        match write_sidecar(&samples_path, module_base, &per_phase) {
            Ok(()) => eprintln!(
                "riprofile: raw samples written to {}",
                samples_path.display()
            ),
            Err(e) => eprintln!("riprofile: could not write {}: {e}", samples_path.display()),
        }
    }
}

/// One inline frame: its name and `file:line` (or a placeholder when the PDB
/// carried no line for it at that context).
#[derive(Clone, Debug)]
struct Frame {
    name: String,
    site: String,
}

/// The inline-aware resolution of one sampled address.
///
/// `physical_new` is the physical (never-inlined) symbol, re-derived through
/// `SymFromInlineContextW(..., INLINE_FRAME_CONTEXT_IGNORE, ...)` rather than
/// carried over from `resolve_symbol`'s `SymFromAddrW` call -- that
/// independence is what gives the conservation check (see
/// [`conservation_mismatch`]) any power at all. It is not rendered in the
/// report; the report's physical tables come from the untouched
/// `resolve_symbol`/`resolve_line` pair so old and new reports stay
/// comparable.
///
/// `chain` is the inline frames nested at this address, INNERMOST first
/// (`chain[0]` is the deepest frame, the one actually executing;
/// `chain.last()` is the shallowest inline site, nearest the physical
/// function). This is dbghelp's own context-walk order, not an assumption:
/// verified empirically against a real 8-level chain in this binary
/// (`div_ceil` at `chain[0]` through `next_timer_wake` at `chain.last()`,
/// with `div_ceil` being the function `next_timer_wake` transitively calls,
/// not the other way around) after the design review flagged this direction
/// as unsettled. Empty when
/// `SymAddrIncludeInlineTrace` reports no inline records here, which is the
/// ordinary case for code nothing was inlined into.
struct InlineResolution {
    physical_new: Option<(String, u64, u32)>,
    chain: Vec<Frame>,
}

/// Render the three legacy ranked tables for one sample set, unchanged from
/// before inline resolution existed, followed by the inline-aware EXCLUSIVE,
/// INCLUSIVE and chain-depth-histogram tables. `resolved` and `inline_resolved`
/// each map an address to its resolution, computed once for the whole run.
fn render_tables(
    counts: &HashMap<u64, u64>,
    resolved: &HashMap<u64, (String, String, String)>,
    inline_resolved: &HashMap<u64, InlineResolution>,
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
    out.push_str(&render_inline_views(counts, inline_resolved));
    out
}

/// The inline-aware EXCLUSIVE, INCLUSIVE and chain-depth-histogram tables. See
/// the module doc for what each answers.
fn render_inline_views(
    counts: &HashMap<u64, u64>,
    inline_resolved: &HashMap<u64, InlineResolution>,
) -> String {
    // Keyed by (name, site), not name alone: a generic function inlined at two
    // different call sites (or a recursive-shaped chain) is genuinely two
    // different lines of interior code, and collapsing them to one name would
    // hide that. This is also what gives `Frame::site` a production reader,
    // not just a test one.
    let mut exclusive: HashMap<(&str, &str), u64> = HashMap::new();
    let mut inclusive: HashMap<(&str, &str), u64> = HashMap::new();
    let mut depth_hist: HashMap<usize, u64> = HashMap::new();
    let mut total = 0u64;
    for (&rip, &n) in counts {
        total += n;
        let Some(res) = inline_resolved.get(&rip) else {
            continue;
        };
        *depth_hist.entry(res.chain.len()).or_default() += n;
        // chain[0] is the INNERMOST frame -- see InlineResolution's doc.
        if let Some(innermost) = res.chain.first() {
            *exclusive
                .entry((innermost.name.as_str(), innermost.site.as_str()))
                .or_default() += n;
        } else {
            let name = res
                .physical_new
                .as_ref()
                .map(|(name, ..)| name.as_str())
                .unwrap_or("<no symbol — JIT arena or foreign code>");
            *exclusive.entry((name, "(physical)")).or_default() += n;
        }
        for frame in &res.chain {
            *inclusive
                .entry((frame.name.as_str(), frame.site.as_str()))
                .or_default() += n;
        }
    }
    let denominator = total.max(1) as f64;
    let mut out = String::new();
    for (title, map) in [
        (
            "INLINE EXCLUSIVE BY FUNCTION — innermost frame owns the sample; sums to 100%",
            &exclusive,
        ),
        (
            "INLINE INCLUSIVE BY FUNCTION — every frame in the chain is charged; sums to more than 100%",
            &inclusive,
        ),
    ] {
        let mut rows: Vec<_> = map.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        out.push_str(&format!("== {title} ==\n"));
        for ((name, site), n) in rows.into_iter().take(60) {
            out.push_str(&format!(
                "{:>7.3}%  {:>9}  {name}  ({site})\n",
                *n as f64 * 100.0 / denominator,
                n,
            ));
        }
        out.push('\n');
    }
    out.push_str(
        "== CHAIN DEPTH HISTOGRAM — inline frames returned per sampled address \
         (0 = nothing inlined there) ==\n",
    );
    let mut depths: Vec<usize> = depth_hist.keys().copied().collect();
    depths.sort_unstable();
    for depth in depths {
        let n = depth_hist[&depth];
        out.push_str(&format!(
            "{:>7.3}%  {:>9}  depth {depth}\n",
            n as f64 * 100.0 / denominator,
            n
        ));
    }
    out.push('\n');
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

const MAX_SYMBOL_NAME: usize = 512;

#[repr(C)]
struct SymbolBuf {
    info: SYMBOL_INFOW,
    _name_tail: [u16; MAX_SYMBOL_NAME],
}

impl SymbolBuf {
    fn new() -> Self {
        let mut buf: SymbolBuf = unsafe { mem::zeroed() };
        buf.info.SizeOfStruct = mem::size_of::<SYMBOL_INFOW>() as u32;
        buf.info.MaxNameLen = MAX_SYMBOL_NAME as u32;
        buf
    }

    fn name(&self) -> String {
        let len = (self.info.NameLen as usize).min(MAX_SYMBOL_NAME);
        let name = unsafe { std::slice::from_raw_parts(self.info.Name.as_ptr(), len) };
        String::from_utf16_lossy(name)
    }
}

/// Resolve `rip` to `(name, displacement, recorded_size)`. dbghelp answers with
/// the nearest PRECEDING symbol however far past its end the address lies; the
/// caller decides whether the displacement clears the recorded extent.
///
/// UNCHANGED since before inline resolution existed: this is the "old" half of
/// the physical conservation check (see [`conservation_mismatch`]) and it also
/// still feeds the report's legacy `BY FUNCTION`/`BY FILE`/`BY FILE:LINE`
/// tables, so old and new reports stay comparable.
fn resolve_symbol(process: HANDLE, rip: u64) -> Option<(String, u64, u32)> {
    let mut buf = SymbolBuf::new();
    let mut displacement = 0u64;
    let ok = unsafe { SymFromAddrW(process, rip, &mut displacement, &mut buf.info) };
    (ok != 0).then(|| (buf.name(), displacement, buf.info.Size))
}

/// Resolve `rip` at one inline context. `ctx = INLINE_FRAME_CONTEXT_IGNORE`
/// answers the physical (never-inlined) symbol, independently re-derived from
/// [`resolve_symbol`]'s `SymFromAddrW` call. Any other `ctx` (from
/// [`resolve_inline_chain`]'s walk) answers that inline frame.
fn resolve_frame_at_context(process: HANDLE, rip: u64, ctx: u32) -> Option<(String, u64, u32)> {
    let mut buf = SymbolBuf::new();
    let mut displacement = 0u64;
    let ok = unsafe { SymFromInlineContextW(process, rip, ctx, &mut displacement, &mut buf.info) };
    (ok != 0).then(|| (buf.name(), displacement, buf.info.Size))
}

/// The physical (never-inlined) symbol at `rip`, re-derived through
/// `SymFromInlineContextW(..., INLINE_FRAME_CONTEXT_IGNORE, ...)`. See
/// [`InlineResolution::physical_new`] for why this must not be the same call
/// as [`resolve_symbol`].
fn resolve_physical(process: HANDLE, rip: u64) -> Option<(String, u64, u32)> {
    resolve_frame_at_context(process, rip, INLINE_FRAME_CONTEXT_IGNORE)
}

/// Whether a resolved RIP fell past the recorded extent of the symbol dbghelp
/// attributed it to, which means the attribution is a guess about the gap after
/// that symbol rather than the symbol itself. A zero recorded size means the
/// PDB carried no extent and proves nothing either way; only a nonzero extent
/// the displacement clears is evidence of misattribution.
fn beyond_extent(displacement: u64, size: u32) -> bool {
    size > 0 && displacement >= u64::from(size)
}

/// Compares the OLD (`SymFromAddrW`) physical resolution against the NEW
/// (`SymFromInlineContextW(..., IGNORE, ...)`) one for the same address.
/// `None` when they agree on name and on beyond-extent status; `Some(reason)`
/// describing the disagreement otherwise. A resolver that kept the old value
/// forward for the "new" column would make this tautological -- see the
/// module doc's "physical conservation" paragraph.
fn conservation_mismatch(
    old: Option<&(String, u64, u32)>,
    new: Option<&(String, u64, u32)>,
) -> Option<String> {
    match (old, new) {
        (None, None) => None,
        (Some((oname, odisp, osize)), Some((nname, ndisp, nsize))) => {
            if oname == nname && beyond_extent(*odisp, *osize) == beyond_extent(*ndisp, *nsize) {
                None
            } else {
                Some(format!(
                    "old={oname:?}@{odisp:#x}/{osize:#x} new={nname:?}@{ndisp:#x}/{nsize:#x}"
                ))
            }
        }
        (old, new) => Some(format!("resolved-state differs: old={old:?} new={new:?}")),
    }
}

fn format_line(line: &IMAGEHLP_LINEW64) -> String {
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
}

/// UNCHANGED since before inline resolution existed: the "old" half of the
/// legacy `BY FILE:LINE` table.
fn resolve_line(process: HANDLE, rip: u64) -> Option<String> {
    let mut line: IMAGEHLP_LINEW64 = unsafe { mem::zeroed() };
    line.SizeOfStruct = mem::size_of::<IMAGEHLP_LINEW64>() as u32;
    let mut displacement = 0u32;
    let ok = unsafe { SymGetLineFromAddrW64(process, rip, &mut displacement, &mut line) };
    (ok != 0).then(|| format_line(&line))
}

/// The file:line for `rip` at one inline context; see [`resolve_frame_at_context`].
fn resolve_line_at_context(process: HANDLE, rip: u64, ctx: u32) -> Option<String> {
    let mut line: IMAGEHLP_LINEW64 = unsafe { mem::zeroed() };
    line.SizeOfStruct = mem::size_of::<IMAGEHLP_LINEW64>() as u32;
    let mut displacement = 0u32;
    let ok =
        unsafe { SymGetLineFromInlineContextW(process, rip, ctx, 0, &mut displacement, &mut line) };
    (ok != 0).then(|| format_line(&line))
}

/// The inline frames nested at `rip`, INNERMOST first: `chain[0]` is the
/// deepest frame, the one actually executing at that address; `chain.last()`
/// is the shallowest inline site, nearest the physical function. This is
/// `SymQueryInlineTrace`'s own context-walk order (`ctx, ctx+1, ...`), not an
/// assumption -- the design this instrument implements explicitly flagged the
/// direction as unsettled by its precondition spike, and it was settled here
/// empirically against a real multi-level chain (see [`InlineResolution`]).
/// Empty when `SymAddrIncludeInlineTrace` reports no inline records for this
/// address, which is the ordinary case for code nothing was inlined into --
/// NOT an error, and not evidence either way about whether inlining happened
/// elsewhere in the binary.
fn resolve_inline_chain(process: HANDLE, rip: u64) -> Vec<Frame> {
    let n = unsafe { SymAddrIncludeInlineTrace(process, rip) };
    if n == 0 {
        return Vec::new();
    }
    let mut ctx: u32 = 0;
    let mut frame_index: u32 = 0;
    let ok = unsafe {
        SymQueryInlineTrace(
            process,
            rip,
            INLINE_FRAME_CONTEXT_INIT,
            rip,
            rip,
            &mut ctx,
            &mut frame_index,
        )
    };
    if ok == 0 {
        return Vec::new();
    }
    let mut chain = Vec::with_capacity(n as usize);
    for i in 0..n {
        let this_ctx = ctx.wrapping_add(i);
        let Some((name, ..)) = resolve_frame_at_context(process, rip, this_ctx) else {
            continue;
        };
        let site = resolve_line_at_context(process, rip, this_ctx)
            .unwrap_or_else(|| format!("{name} (no line info)"));
        chain.push(Frame { name, site });
    }
    chain
}

/// Whether the module's symbol load is trustworthy enough for inline-aware
/// resolution: a loaded PDB (`SymType == SymPdb`, 3) carrying line numbers. A
/// stripped export-only fallback, a deferred load that never found its PDB, or
/// a mismatched PDB all resolve zero inline frames, which reads as "nothing is
/// inlined" when it means "nothing was loaded" -- see the
/// `SYMOPT_DEFERRED_LOADS` comment in `stop_and_report`.
fn symbols_are_trustworthy(sym_type: i32, line_numbers: i32) -> bool {
    sym_type == SYM_PDB && line_numbers != 0
}

/// The measured mean spacing between the first and last sample: `None` when
/// fewer than two samples were captured (a single point has no window, and a
/// zero-length window from a clock quirk must not divide by nothing).
fn achieved_interval(sample_count: usize, window: Duration) -> Option<Duration> {
    (sample_count > 1).then(|| window / (sample_count as u32 - 1))
}

/// The report header's sample-count line, stating the ACHIEVED sampling rate
/// beside the nominal `SAMPLE_INTERVAL` requested -- see the module doc for
/// why they differ by about 2x on this platform. Every report before this
/// change printed only the nominal figure, which was never the true rate.
fn sample_rate_line(total: u64, window: Option<Duration>, interval: Option<Duration>) -> String {
    match (window, interval) {
        (Some(w), Some(i)) => format!(
            "{total} samples over {:.3}s = {:.3}ms/sample achieved ({SAMPLE_INTERVAL:?} requested)",
            w.as_secs_f64(),
            i.as_secs_f64() * 1000.0,
        ),
        _ => format!(
            "{total} samples, achieved rate unknown — fewer than 2 samples \
             ({SAMPLE_INTERVAL:?} requested)"
        ),
    }
}

/// `<report>.samples`, beside the report itself.
fn sidecar_path(report_path: &Path) -> PathBuf {
    let mut name = report_path.as_os_str().to_os_string();
    name.push(".samples");
    PathBuf::from(name)
}

/// Writes the raw-sample sidecar: one header comment naming this run's module
/// base, then one `rip_hex,phase,count` row per unique (address, phase) pair
/// actually sampled, aggregated from the same `per_phase` map the report is
/// built from. Called once, after the report is written, never during a run
/// (see the module doc). Addresses are raw process RIPs against THIS run's
/// module base, not RVAs against a named PDB: resolving a sidecar from a
/// different binary is future work and has no guard here.
fn write_sidecar(
    path: &Path,
    module_base: u64,
    per_phase: &HashMap<u32, HashMap<u64, u64>>,
) -> std::io::Result<()> {
    let mut out = String::new();
    out.push_str("# riprofile raw samples v1\n");
    out.push_str(&format!("# module_base={module_base:#x}\n"));
    out.push_str("# rip_hex,phase,count\n");
    let mut phases: Vec<u32> = per_phase.keys().copied().collect();
    phases.sort_unstable();
    for phase in phases {
        let Some(map) = per_phase.get(&phase) else {
            continue;
        };
        let mut rips: Vec<u64> = map.keys().copied().collect();
        rips.sort_unstable();
        for rip in rips {
            out.push_str(&format!("{rip:#x},{phase},{}\n", map[&rip]));
        }
    }
    std::fs::write(path, out)
}
