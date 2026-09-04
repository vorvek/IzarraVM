// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    Frame, achieved_interval, beyond_extent, conservation_mismatch, exe_directory,
    resolve_inline_chain, resolve_physical, sample_rate_line, sidecar_path,
    symbols_are_trustworthy, write_sidecar,
};
use std::collections::HashMap;
use std::time::Duration;

/// Parses [`write_sidecar`]'s format back into `(rip, phase, count)` triples.
/// Test-only: exists to give the sidecar its OWN round-trip test, independent
/// of any other guard in this module -- no production code reads a sidecar
/// back today (see the module doc's "Raw-sample sidecar" paragraph).
fn parse_sidecar(text: &str) -> Vec<(u64, u32, u64)> {
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| {
            let mut parts = line.split(',');
            let rip = u64::from_str_radix(parts.next()?.trim_start_matches("0x"), 16).ok()?;
            let phase = parts.next()?.parse().ok()?;
            let count = parts.next()?.parse().ok()?;
            Some((rip, phase, count))
        })
        .collect()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn narrow(w: &[u16]) -> String {
    String::from_utf16_lossy(w)
}

#[test]
fn directory_is_the_path_up_to_the_last_separator_nul_terminated() {
    let dir = exe_directory(&wide(r"D:\dev\IzarraVM\target\profiling\izarravm.exe"))
        .expect("a normal exe path has a directory");
    assert_eq!(*dir.last().expect("non-empty"), 0, "must be NUL-terminated");
    assert_eq!(
        narrow(&dir[..dir.len() - 1]),
        r"D:\dev\IzarraVM\target\profiling"
    );
}

#[test]
fn forward_slashes_are_accepted_too() {
    let dir = exe_directory(&wide("D:/dev/IzarraVM/target/release/izarravm.exe"))
        .expect("a forward-slash path still has a directory");
    assert_eq!(
        narrow(&dir[..dir.len() - 1]),
        "D:/dev/IzarraVM/target/release"
    );
}

#[test]
fn resolve_symbol_reports_a_nonzero_extent_for_a_live_function() {
    // The beyond-extent guard treats a zero recorded size as "no extent, proves
    // nothing", so if this toolchain's PDBs ever stop carrying function extents
    // the guard is silently disarmed. Resolve a function from this very test
    // binary and pin that the Size plumbing populates.
    //
    // In a CHILD PROCESS with one test thread: `SymInitializeW(fInvadeProcess
    // = 1)` walks every loaded module under the loader lock while the parent
    // harness's other threads load and unload DLLs (the audio-backend tests
    // do), and dbghelp is not thread-safe — CI took a STATUS_ACCESS_VIOLATION
    // with this test in flight beside the gui::session battery. The profiler
    // itself never has this problem: it initializes dbghelp at report time,
    // after the emulation thread has stopped.
    if std::env::var_os("IZARRAVM_RIPROFILE_EXTENT_CHILD").is_some() {
        resolve_extent_in_this_process();
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    let output = std::process::Command::new(exe)
        .args([
            "riprofile::tests::resolve_symbol_reports_a_nonzero_extent_for_a_live_function",
            "--exact",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("IZARRAVM_RIPROFILE_EXTENT_CHILD", "1")
        .output()
        .expect("spawn the extent child");
    assert!(
        output.status.success(),
        "extent child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// The function the child resolves. `set_phase` was the original probe and
/// release codegen destroyed it two ways at once: nothing in the test binary
/// reads `ACTIVE_PHASE`, so its store optimized down to a bare `ret`, and the
/// linker's identical-code folding then merged that `ret` with every other
/// empty function — `SymFromAddrW` answered with a ThinLTO-promoted
/// tracing_core symbol at displacement 0. The anchor mixes constants nothing
/// else in the binary uses, so no build can make it byte-identical to another
/// function, and `#[inline(never)]` pins a standalone body to resolve.
#[inline(never)]
fn extent_anchor(x: u32) -> u32 {
    x.rotate_left(9).wrapping_mul(0x9E37_79B1) ^ 0x495A_4152
}

fn resolve_extent_in_this_process() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SYMOPT_LOAD_LINES, SYMOPT_UNDNAME, SymCleanup, SymInitializeW, SymSetOptions,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // The search path must be the exe's own directory, same as
    // `stop_and_report`: dbghelp's NULL default (CWD + _NT_SYMBOL_PATH) does
    // not contain the test binary's PDB, and this test fails from a NULL path.
    let process = unsafe { GetCurrentProcess() };
    // The profiler's options, and the debug-info probe below needs the lines.
    unsafe { SymSetOptions(SYMOPT_LOAD_LINES | SYMOPT_UNDNAME) };
    let mut path = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(std::ptr::null_mut(), path.as_mut_ptr(), 1024) };
    let exe_dir = exe_directory(&path[..len as usize]).expect("test exe has a directory");
    assert_ne!(
        unsafe { SymInitializeW(process, exe_dir.as_ptr(), 1) },
        0,
        "SymInitializeW failed"
    );
    let rip = std::hint::black_box(extent_anchor as *const () as usize as u64);
    // A `--release` test binary carries no CodeView for this crate (`debug` is
    // unset in that profile): its PDB holds only linker publics, which have no
    // sizes and omit LTO-internalized functions entirely, so the extent
    // invariant has nothing to attach to. Detect that build per-ADDRESS —
    // module-level flags like `IMAGEHLP_MODULEW64.LineNumbers` read true even
    // then, because the CRT's /Z7 objects contribute debug info for their own
    // ranges.
    if super::resolve_line(process, rip).is_none() {
        unsafe { SymCleanup(process) };
        if cfg!(debug_assertions) {
            panic!(
                "a debug build must carry line info for its own code; without it \
                 the extent guard below would be silently skipped in CI"
            );
        }
        eprintln!(
            "riprofile extent test: publics-only PDB (optimized build without debug \
             info); function extents do not exist in this binary, skipping"
        );
        return;
    }
    let resolved = super::resolve_symbol(process, rip);
    unsafe { SymCleanup(process) };
    let (name, displacement, size) = resolved.expect("the test binary's own PDB must resolve");
    assert!(name.contains("extent_anchor"), "resolved {name:?}");
    assert!(
        size > 0,
        "the PDB carried no extent for a function symbol; the beyond-extent guard is disarmed"
    );
    assert!(
        displacement < u64::from(size),
        "a function entry must sit inside its own recorded extent"
    );
}

#[test]
fn a_zero_recorded_size_is_not_evidence_of_misattribution() {
    // The PDB carried no extent for the symbol; a displacement past nothing
    // proves nothing.
    assert!(!beyond_extent(0x40, 0));
}

#[test]
fn beyond_extent_is_displacement_at_or_past_the_recorded_size() {
    // A function of size S occupies [start, start+S): the last in-extent byte
    // is at displacement S-1 and displacement S is already the gap after it.
    assert!(!beyond_extent(0x3f, 0x40));
    assert!(beyond_extent(0x40, 0x40));
    assert!(beyond_extent(0x1000, 0x40));
}

#[test]
fn a_path_with_no_directory_falls_back_to_dbghelps_default() {
    // No separator at all, and a separator at index 0 (a root-relative path whose
    // directory would be the empty string). Both must yield None so the caller
    // passes NULL rather than an empty search path, which dbghelp would treat as
    // a real, and wrong, search path.
    assert!(exe_directory(&wide("izarravm.exe")).is_none());
    assert!(exe_directory(&wide(r"\izarravm.exe")).is_none());
    assert!(exe_directory(&[]).is_none());
}

// ===== Item 5: refuse a degraded symbol load =====

#[test]
fn a_loaded_pdb_with_line_numbers_is_trustworthy() {
    assert!(symbols_are_trustworthy(3, 1));
}

#[test]
fn anything_but_a_loaded_pdb_with_lines_is_refused() {
    // SymDeferred (5) never resolved to a PDB at all.
    assert!(!symbols_are_trustworthy(5, 0));
    // SymNone: no symbols loaded.
    assert!(!symbols_are_trustworthy(0, 1));
    // SymType claims Pdb but no line numbers -- a stripped or mismatched PDB.
    // This is the exact "plausible empty answer" the module doc warns about:
    // SymType=3 alone is not enough to trust inline resolution.
    assert!(!symbols_are_trustworthy(3, 0));
}

// ===== Item 2: honest sample-rate reporting =====

#[test]
fn achieved_interval_matches_the_nascar_anchor() {
    // nascar-586, 2026-08-25: 33,499 samples over a 34.952s steady-state
    // window is 1.0434 ms/sample, 2.09x the nominal 500us -- see the module
    // doc. `n` samples span `n-1` gaps, so the window divides by 33,498.
    let window = Duration::from_secs_f64(34.952);
    let interval = achieved_interval(33_499, window).expect("more than one sample");
    let ms = interval.as_secs_f64() * 1000.0;
    assert!(
        (ms - 1.0434).abs() < 0.001,
        "got {ms} ms/sample, want ~1.0434"
    );
}

#[test]
fn achieved_interval_is_none_for_fewer_than_two_samples() {
    assert!(achieved_interval(0, Duration::from_secs(1)).is_none());
    assert!(achieved_interval(1, Duration::from_secs(1)).is_none());
}

#[test]
fn sample_rate_line_states_both_achieved_and_requested() {
    let line = sample_rate_line(
        33_499,
        Some(Duration::from_secs_f64(34.952)),
        Some(Duration::from_secs_f64(0.0010434)),
    );
    assert!(line.contains("33499 samples"), "{line}");
    assert!(line.contains("34.952s"), "{line}");
    assert!(line.contains("1.043ms/sample achieved"), "{line}");
    assert!(line.contains("500\u{b5}s requested"), "{line}");
    // The old header only ever printed the nominal figure. Guard against
    // silently regressing back to that by pinning both numbers are present
    // and distinct in the same line.
    assert!(
        line.contains("1.043ms") && line.contains("500\u{b5}s") && !line.contains("1.043\u{b5}s"),
        "{line}"
    );
}

#[test]
fn sample_rate_line_handles_fewer_than_two_samples() {
    let line = sample_rate_line(1, None, None);
    assert!(line.contains("fewer than 2 samples"), "{line}");
    assert!(line.contains("requested"), "{line}");
}

// ===== Item 3: raw-sample sidecar =====

#[test]
fn sidecar_round_trips_rip_phase_count_triples() {
    let mut per_phase: HashMap<u32, HashMap<u64, u64>> = HashMap::new();
    per_phase.entry(0).or_default().insert(0x1400_1000, 3);
    per_phase.entry(0).or_default().insert(0x1400_2000, 7);
    per_phase.entry(1).or_default().insert(0x1400_1000, 5);

    let path = std::env::temp_dir().join(format!(
        "izarravm-riprofile-sidecar-{}-{}.samples",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    write_sidecar(&path, 0x1400_0000, &per_phase).expect("write sidecar");
    let text = std::fs::read_to_string(&path).expect("read sidecar back");
    let _ = std::fs::remove_file(&path);

    let mut got = parse_sidecar(&text);
    got.sort();
    let mut want = vec![
        (0x1400_1000u64, 0u32, 3u64),
        (0x1400_2000u64, 0u32, 7u64),
        (0x1400_1000u64, 1u32, 5u64),
    ];
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn sidecar_header_carries_the_module_base_as_a_comment() {
    let path = std::env::temp_dir().join(format!(
        "izarravm-riprofile-sidecar-header-{}-{}.samples",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    write_sidecar(&path, 0x1400_0000, &HashMap::new()).expect("write sidecar");
    let text = std::fs::read_to_string(&path).expect("read sidecar back");
    let _ = std::fs::remove_file(&path);
    assert!(text.contains("module_base=0x14000000"), "{text}");
    // Comment lines must not be mistaken for data rows.
    assert!(parse_sidecar(&text).is_empty());
}

#[test]
fn sidecar_path_is_the_report_path_with_a_samples_suffix() {
    let report = std::path::Path::new(r"D:\out\quake-rip.txt");
    assert_eq!(
        sidecar_path(report),
        std::path::Path::new(r"D:\out\quake-rip.txt.samples")
    );
}

// ===== Item 4: physical conservation =====

#[test]
fn conservation_mismatch_is_none_when_both_sides_agree() {
    let a = ("run_until_tick".to_string(), 0x10, 0x100);
    let b = ("run_until_tick".to_string(), 0x10, 0x100);
    assert_eq!(conservation_mismatch(Some(&a), Some(&b)), None);
}

#[test]
fn conservation_mismatch_is_none_when_both_sides_are_unresolved() {
    assert_eq!(conservation_mismatch(None, None), None);
}

#[test]
fn conservation_mismatch_catches_a_name_disagreement() {
    let old = ("run_until_tick".to_string(), 0x10, 0x100);
    let new = ("run_budgeted".to_string(), 0x10, 0x100);
    assert!(conservation_mismatch(Some(&old), Some(&new)).is_some());
}

#[test]
fn conservation_mismatch_catches_a_resolved_state_disagreement() {
    // One side found a symbol, the other did not -- e.g. a fabricated frame
    // on an unresolvable address, or a swallowed BOOL failure.
    let old = ("run_until_tick".to_string(), 0x10, 0x100);
    assert!(conservation_mismatch(Some(&old), None).is_some());
    assert!(conservation_mismatch(None, Some(&old)).is_some());
}

#[test]
fn conservation_mismatch_catches_a_beyond_extent_disagreement() {
    // Same name, but one side reports the address inside the symbol's
    // recorded extent and the other reports it past the end -- e.g. a wrong
    // context base shifting the displacement.
    let old = ("f".to_string(), 0x40, 0x40); // at the boundary: beyond
    let new = ("f".to_string(), 0x3f, 0x40); // one byte short: in-extent
    assert!(conservation_mismatch(Some(&old), Some(&new)).is_some());
}

/// Spawns a child process the same way [`resolve_symbol_reports_a_nonzero_extent_for_a_live_function`]
/// does, for the same reason: `SymInitializeW(fInvadeProcess = 1)` is not
/// thread-safe against the parent test harness's other threads.
fn spawn_riprofile_child(test_name: &str, env_var: &str) -> std::process::Output {
    let exe = std::env::current_exe().expect("test exe path");
    std::process::Command::new(exe)
        // `--include-ignored` is harmless for a live test and keeps the child
        // from selecting zero tests (exit 0, parent vacuously green) if this
        // helper is pointed at an ignored name again.
        .args([
            test_name,
            "--exact",
            "--include-ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(env_var, "1")
        .output()
        .expect("spawn riprofile child")
}

#[test]
fn physical_conservation_holds_across_real_and_unresolvable_addresses() {
    if std::env::var_os("IZARRAVM_RIPROFILE_CONSERVATION_CHILD").is_some() {
        conservation_check_in_this_process();
        return;
    }
    let output = spawn_riprofile_child(
        "riprofile::tests::physical_conservation_holds_across_real_and_unresolvable_addresses",
        "IZARRAVM_RIPROFILE_CONSERVATION_CHILD",
    );
    assert!(
        output.status.success(),
        "conservation child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn conservation_check_in_this_process() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SYMOPT_LOAD_LINES, SYMOPT_UNDNAME, SymCleanup, SymInitializeW, SymSetOptions,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let process = unsafe { GetCurrentProcess() };
    unsafe { SymSetOptions(SYMOPT_LOAD_LINES | SYMOPT_UNDNAME) };
    let mut path = [0u16; 1024];
    let len = unsafe { GetModuleFileNameW(std::ptr::null_mut(), path.as_mut_ptr(), 1024) };
    let exe_dir = exe_directory(&path[..len as usize]).expect("test exe has a directory");
    assert_ne!(
        unsafe { SymInitializeW(process, exe_dir.as_ptr(), 1) },
        0,
        "SymInitializeW failed"
    );

    // The adversarial population: addresses that resolve to no symbol at
    // all, standing in for the Direct JIT's code arena (real quake profiles
    // put ~25% of samples here). Plus one real, resolvable address.
    let real = std::hint::black_box(extent_anchor as *const () as usize as u64);
    let addrs: [u64; 5] = [real, 0x1, 0x2, 0x3, 0x1000];

    let mut mismatches = Vec::new();
    for &addr in &addrs {
        let old = super::resolve_symbol(process, addr);
        let new = resolve_physical(process, addr);
        if let Some(reason) = conservation_mismatch(old.as_ref(), new.as_ref()) {
            mismatches.push((addr, reason));
        }
    }
    unsafe { SymCleanup(process) };
    assert!(
        mismatches.is_empty(),
        "conservation mismatches: {mismatches:?}"
    );
}

// ===== Item 1: inline-aware resolution (the load-bearing test) =====

/// `SymTagEnum::SymTagInlineSite` in DbgHelp. windows-sys 0.59 does not export
/// the name; CallSite is 31, InlineSite is 32.
const SYM_TAG_INLINE_SITE: u32 = 32;

/// The inlined callee this fixture locates by name. `exact_cap_test` is
/// `#[inline]`, a few lines, and not jit-gated, so a `--no-default-features`
/// release artifact still carries it. Locating by this symbol (via
/// `SymEnumSymbolsExW` + `SYMENUM_OPTIONS_INLINE`) rather than by scanning
/// `run_until_tick`'s extent is what stops ThinLTO of an unrelated hot path
/// from making the test red.
const INLINE_CALLEE: &str = "exact_cap_test";

fn names_the_inline_callee(name: &str) -> bool {
    name == INLINE_CALLEE || name.rsplit("::").next() == Some(INLINE_CALLEE)
}

fn in_ci() -> bool {
    std::env::var_os("CI").is_some() || std::env::var_os("GITHUB_ACTIONS").is_some()
}

/// The shipped `izarravm.exe` this check loads as a separate module.
///
/// `CARGO_TARGET_DIR`, when set, wins even if the file is missing: falling
/// through to the main tree's `target/release` from a worktree would load a
/// foreign image/PDB pair. With the env var unset, walk up from the test
/// harness (`target/<profile>/deps/<harness>.exe`) so a config.toml
/// `build.target-dir` still works, then `CARGO_MANIFEST_DIR/../../target`.
fn release_exe_path() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return std::path::PathBuf::from(dir)
            .join("release")
            .join("izarravm.exe");
    }
    if let Ok(harness) = std::env::current_exe() {
        let profile_dir = harness.parent().and_then(|p| {
            if p.file_name().is_some_and(|n| n == "deps") {
                p.parent()
            } else {
                Some(p)
            }
        });
        if let Some(target_dir) = profile_dir.and_then(|p| p.parent()) {
            let candidate = target_dir.join("release").join("izarravm.exe");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/release/izarravm.exe")
}

#[test]
fn inline_resolution_names_an_inlined_callee_the_old_resolver_collapses() {
    if std::env::var_os("IZARRAVM_RIPROFILE_INLINE_CHILD").is_some() {
        inline_chain_check_in_this_process();
        return;
    }
    let output = spawn_riprofile_child(
        "riprofile::tests::inline_resolution_names_an_inlined_callee_the_old_resolver_collapses",
        "IZARRAVM_RIPROFILE_INLINE_CHILD",
    );
    assert!(
        output.status.success(),
        "inline chain child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn inline_chain_check_in_this_process() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SYMOPT_LOAD_LINES, SYMOPT_UNDNAME, SymCleanup, SymInitializeW, SymLoadModuleExW,
        SymSetOptions, SymUnloadModule64,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // Loads the shipped `izarravm.exe` as a separate module (`SymLoadModuleExW`,
    // `fInvadeProcess = 0`). The test harness binary is a different link and
    // LLVM inlines it differently; this check is against the artifact the
    // profiler actually symbolizes. Build profile of the harness does not
    // matter. Missing exe skips locally (worktrees without a release build)
    // and panics under CI, where `.github/workflows/ci.yml` builds release
    // before `cargo test --workspace`. A present exe with a bad/missing PDB
    // is always a hard fail: that is a broken artifact, not "I did not build
    // release."
    let exe_path = release_exe_path();
    if !exe_path.exists() {
        let msg = format!(
            "release exe not found at {exe_path:?}; build it with \
             `cargo build --profile release -p izarravm`"
        );
        if in_ci() {
            panic!("{msg}");
        }
        eprintln!("riprofile inline test: {msg} (skipped)");
        return;
    }
    let exe_path = exe_path.canonicalize().unwrap_or(exe_path);

    let process = unsafe { GetCurrentProcess() };
    unsafe { SymSetOptions(SYMOPT_LOAD_LINES | SYMOPT_UNDNAME) };
    let mut wide_path: Vec<u16> = exe_path.as_os_str().encode_wide().collect();
    wide_path.push(0);
    // Search path is the exe's directory so dbghelp finds the sibling PDB.
    // Keep `exe_dir` alive across the `SymInitializeW` call; the pointer is
    // into that Vec.
    let exe_dir =
        exe_directory(&wide_path[..wide_path.len() - 1]).expect("release exe has a directory");
    assert_ne!(
        unsafe { SymInitializeW(process, exe_dir.as_ptr(), 0) },
        0,
        "SymInitializeW failed, GetLastError={}",
        unsafe { GetLastError() }
    );

    let base = unsafe {
        SymLoadModuleExW(
            process,
            std::ptr::null_mut(),
            wide_path.as_ptr(),
            std::ptr::null(),
            0,
            0,
            std::ptr::null(),
            0,
        )
    };
    assert_ne!(
        base,
        0,
        "SymLoadModuleExW failed to load {exe_path:?}, GetLastError={}",
        unsafe { GetLastError() }
    );

    let mut info: windows_sys::Win32::System::Diagnostics::Debug::IMAGEHLP_MODULEW64 =
        unsafe { std::mem::zeroed() };
    info.SizeOfStruct = std::mem::size_of::<
        windows_sys::Win32::System::Diagnostics::Debug::IMAGEHLP_MODULEW64,
    >() as u32;
    assert_ne!(
        unsafe { super::SymGetModuleInfoW64(process, base, &mut info) },
        0,
        "SymGetModuleInfoW64 failed for the loaded release exe, GetLastError={}",
        unsafe { GetLastError() }
    );
    assert!(
        symbols_are_trustworthy(info.SymType, info.LineNumbers),
        "{exe_path:?} loaded degraded symbols (SymType={}, lines={}); rebuild it",
        info.SymType,
        info.LineNumbers
    );

    let sites = match enum_inline_sites(process, base, INLINE_CALLEE) {
        Ok(sites) => sites,
        Err(err) => {
            unsafe {
                SymUnloadModule64(process, base);
                SymCleanup(process);
            }
            panic!(
                "SymEnumSymbolsExW(SYMENUM_OPTIONS_INLINE, *{INLINE_CALLEE}*) failed, \
                 GetLastError={err}"
            );
        }
    };

    let mut empty_chain = 0usize;
    let mut chain_omits_callee = 0usize;
    let mut named_but_not_collapsed = 0usize;
    let mut example: Option<(u64, String, Vec<Frame>, Option<String>)> = None;
    for (addr, site_name) in &sites {
        let chain = resolve_inline_chain(process, *addr);
        if chain.is_empty() {
            empty_chain += 1;
            continue;
        }
        if !chain.iter().any(|f| names_the_inline_callee(&f.name)) {
            chain_omits_callee += 1;
            continue;
        }
        let old = super::resolve_symbol(process, *addr);
        let old_name = old.as_ref().map(|(name, ..)| name.clone());
        if old_name.as_deref().is_some_and(names_the_inline_callee) {
            named_but_not_collapsed += 1;
            continue;
        }
        example = Some((*addr, site_name.clone(), chain, old_name));
        break;
    }

    unsafe {
        SymUnloadModule64(process, base);
        SymCleanup(process);
    }

    if sites.is_empty() {
        panic!(
            "PDB has no inline-site records for {INLINE_CALLEE} (mask *{INLINE_CALLEE}*, \
             Tag=SymTagInlineSite). The function is not inlined in this image, or the \
             PDB name assumption broke. Not a resolver regression."
        );
    }

    let (addr, site_name, chain, old_name) = match example {
        Some(found) => found,
        None if chain_omits_callee > 0 => panic!(
            "RESOLVER REGRESSION: PDB recorded {} inline site(s) for {INLINE_CALLEE} \
             but resolve_inline_chain did not name it at any of them \
             ({chain_omits_callee} nonempty chain(s) omitted the name, {empty_chain} empty)",
            sites.len()
        ),
        None if empty_chain == sites.len() => panic!(
            "RESOLVER REGRESSION: PDB recorded {} inline site(s) for {INLINE_CALLEE} \
             but resolve_inline_chain returned empty at all of them",
            sites.len()
        ),
        None => panic!(
            "{INLINE_CALLEE} is inlined at {} site(s) and resolve_inline_chain names it, \
             but the old resolver also names it at every site ({named_but_not_collapsed} \
             hits). No collapsed-into-parent example.",
            sites.len()
        ),
    };

    assert!(
        names_the_inline_callee(&site_name),
        "enum site name must be {INLINE_CALLEE}, got {site_name:?}"
    );
    assert!(
        chain.iter().any(|f| names_the_inline_callee(&f.name)),
        "chain at {addr:#x} must name {INLINE_CALLEE}, got {:?}",
        chain.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
    assert!(
        old_name
            .as_deref()
            .is_none_or(|name| !names_the_inline_callee(name)),
        "old resolver must collapse {INLINE_CALLEE} into a different physical symbol at \
         {addr:#x}, got {old_name:?}"
    );
}

/// Enumerate inline-site records whose name matches `*{callee}*`.
/// `Err` is `GetLastError` after a FALSE return; `Ok(vec![])` is a successful
/// enum that found nothing.
fn enum_inline_sites(
    process: windows_sys::Win32::Foundation::HANDLE,
    base: u64,
    callee: &str,
) -> Result<Vec<(u64, String)>, u32> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SYMENUM_OPTIONS_INLINE, SymEnumSymbolsExW,
    };

    let mut mask = wide(&format!("*{callee}*"));
    mask.push(0);
    let mut sites: Vec<(u64, String)> = Vec::new();
    let ok = unsafe {
        SymEnumSymbolsExW(
            process,
            base,
            mask.as_ptr(),
            Some(collect_inline_sites),
            std::ptr::from_mut(&mut sites).cast(),
            SYMENUM_OPTIONS_INLINE,
        )
    };
    if ok == 0 {
        return Err(unsafe { GetLastError() });
    }
    sites.retain(|(_, name)| names_the_inline_callee(name));
    Ok(sites)
}

unsafe extern "system" fn collect_inline_sites(
    psyminfo: *const windows_sys::Win32::System::Diagnostics::Debug::SYMBOL_INFOW,
    _symbolsize: u32,
    usercontext: *const core::ffi::c_void,
) -> i32 {
    if psyminfo.is_null() || usercontext.is_null() {
        return 1;
    }
    let info = unsafe { &*psyminfo };
    if info.Tag != SYM_TAG_INLINE_SITE {
        return 1;
    }
    let len = (info.NameLen as usize).min(512);
    let raw = unsafe { std::slice::from_raw_parts(info.Name.as_ptr(), len) };
    // The enum callback's NameLen includes the trailing NUL; strip it so
    // path-segment matching does not see `exact_cap_test\0`.
    let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    let name = String::from_utf16_lossy(&raw[..end]);
    let out = unsafe { &mut *usercontext.cast::<Vec<(u64, String)>>().cast_mut() };
    out.push((info.Address, name));
    1
}
