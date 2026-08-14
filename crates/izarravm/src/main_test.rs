// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn munt_test_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "izarravm-munt-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

const PERF_COUNTER_KEYS: &[&str] = &[
    "brk_cap",
    "brk_cont_decode_miss",
    "brk_cont_not_continuable",
    "brk_cont_page_cross",
    "brk_decode_or_branch",
    "brk_halt",
    "brk_interrupt",
    "brk_step",
    "cache_tier_lookups",
    "code_invalidations",
    "code_watch_block_page_edges",
    "code_watch_block_page_releases",
    "code_watch_sticky_page_edges",
    "code_watch_sweep_cleared",
    "data_direct_reads",
    "data_direct_writes",
    "data_slow_reads",
    "data_slow_writes",
    "decode_inval_cs_load",
    "decode_inval_other",
    "decode_inval_smc",
    "decode_misses",
    "decode_probes",
    "device_write_bytes",
    "device_write_coarse_resets",
    "device_write_code_hits",
    "device_write_ranges",
    "direct_data_pointer_reads",
    "direct_data_pointer_writes",
    "direct_map_invalidations",
    "direct_page_hits",
    "direct_page_misses",
    "fast_map_wipe_aperture_pages_cleared",
    "fast_map_wipe_pages_cleared",
    "fast_map_wipe_vga_pages_cleared",
    "fast_map_wipes_a20",
    "fast_map_wipes_admission",
    "fast_map_wipes_direct_data_map",
    "fast_map_wipes_direct_map",
    "fast_map_wipes_tlb_flush",
    "fetch_page_hits",
    "fetch_page_misses",
    "flag_materializations",
    "instructions",
    "interp_fast_map_hits",
    "interp_fast_map_misses",
    "jit_direct_arena_compaction_bytes",
    "jit_direct_arena_compaction_failures",
    "jit_direct_arena_compaction_live_blocks",
    "jit_direct_arena_compaction_ns",
    "jit_direct_arena_compactions",
    "jit_direct_blocks_installed",
    "jit_direct_blocks_installed_sixteen_bit",
    "jit_direct_cache_resets",
    "jit_direct_chain_quota_cache_misses",
    "jit_direct_chain_quota_entries",
    "jit_direct_compile_attempts",
    "jit_direct_compile_ns",
    "jit_direct_decode_dependencies_scanned",
    "jit_direct_deferred_short",
    "jit_direct_dispatch_declines",
    "jit_direct_entries",
    "jit_direct_entries_sixteen_bit",
    "jit_direct_exit_code_watch",
    "jit_direct_exit_cross_page_or_alignment",
    "jit_direct_exit_other",
    "jit_direct_exit_permission",
    "jit_direct_exit_unavailable_or_kind",
    "jit_direct_hash_hits",
    "jit_direct_hot_hits",
    "jit_direct_insns",
    "jit_direct_insns_sixteen_bit",
    "jit_direct_linked_transfers",
    "jit_direct_links_cleared",
    "jit_direct_links_created",
    "jit_direct_lookup_misses",
    "jit_direct_portals_hidden",
    "jit_direct_reject_aggregate_accounting",
    "jit_direct_reject_alignment",
    "jit_direct_reject_cpl",
    "jit_direct_reject_cs_layout",
    "jit_direct_reject_data_segment",
    "jit_direct_reject_fetch_limit",
    "jit_direct_reject_interrupt_shadow",
    "jit_direct_reject_mode_key",
    "jit_direct_reject_observer",
    "jit_direct_reject_x87_top",
    "jit_direct_reject_zero_budget",
    "jit_direct_side_exits",
    "jit_direct_unresolved_dynamic_hidden",
    "jit_direct_unresolved_dynamic_miss_or_unbound",
    "jit_direct_unresolved_exits",
    "jit_direct_unresolved_static_hidden",
    "jit_direct_unresolved_static_unbound",
    "jit_direct_word_address_slots",
    "jit_direct_word_control_admitted",
    "jit_direct_word_control_refused",
    "jit_direct_x87_pad_bails",
    "jit_native_load_hits",
    "jit_native_store_hits",
    "jit_paged_tlb_successes",
    "monitor_resident_core_clocks",
    "monitor_trips_vec13",
    "poll_head_prefilter_rejects",
    "poll_neg_cache_hits",
    "poll_neg_cache_stores",
    "poll_neg_cache_volatile",
    "poll_skip_iterations",
    "poll_skip_memory_iterations",
    "poll_skip_memory_spans",
    "poll_skip_spans",
    "rep_string_fast_iterations",
    "rep_string_iterations",
    "rmw_census_enabled",
    "rmw_census_reads",
    "rmw_census_rmw_pairs",
    "rmw_census_writes",
    "slot_admit_misaligned",
    "slot_reject_absent",
    "slot_reject_enabled",
    "slot_reject_epoch",
    "slot_reject_misaligned",
    "slot_reject_page_cross",
    "slot_reject_permission",
    "slow_prefetch_refills",
    "smc_heat_chunks_hot",
    "smc_heat_demotions",
    "smc_lane_accepts",
    "smc_lane_registrations",
    "smc_lane_reject_address",
    "smc_lane_reject_width",
    "smc_narrow_kills",
    "smc_scan_calls",
    "smc_scan_keys",
    "straight_line_runs",
];

#[test]
fn cli_parses_munt_roms_and_stable_external_port_identity() {
    let cli = Cli::try_parse_from([
        "izarravm",
        "--midi-backend",
        "munt",
        "--midi-port",
        "USB MIDI",
        "--midi-port-ordinal",
        "2",
        "--mt32-control-rom",
        "control.rom",
        "--mt32-pcm-rom",
        "pcm.rom",
    ])
    .unwrap();

    assert_eq!(cli.midi_backend, Some(MidiBackend::Munt));
    assert_eq!(cli.midi_port.as_deref(), Some("USB MIDI"));
    assert_eq!(cli.midi_port_ordinal, Some(2));
    assert_eq!(
        cli.mt32_control_rom.as_deref(),
        Some(std::path::Path::new("control.rom"))
    );
    assert_eq!(
        cli.mt32_pcm_rom.as_deref(),
        Some(std::path::Path::new("pcm.rom"))
    );
}

/// The exact shape every fixture run and every campaign script uses. It has no business
/// failing to parse, and when it did, the message named `--dosroot` -- an argument that is
/// nowhere on this command line -- because `--dosroot` also reads `IZARRAVM_DOSROOT` and clap's
/// stock `PathBuf` parser rejects an empty value from the environment exactly as it would from
/// the command line. This test parses the invocation; the one below covers the empty value.
#[test]
fn cli_parses_the_standard_fixture_invocation() {
    let cli = Cli::try_parse_from([
        "izarravm",
        "--cpu",
        "486",
        "--memory-mib",
        "64",
        "--video",
        "vega",
        "--hdd-folder",
        "fixture",
        "--cycles",
        "1000",
    ])
    .expect("the standard fixture invocation must parse");
    assert_eq!(cli.cpu, Some(GswMode::Gsw486));
    assert_eq!(cli.memory_mib, Some(64));
    assert_eq!(cli.hdd_folder.as_deref(), Some(Path::new("fixture")));
    assert_eq!(cli.cycles, Some(1000));
}

/// An empty `--dosroot` (the command-line spelling of an `IZARRAVM_DOSROOT=` left set to
/// nothing) must PARSE and then read as absent, rather than aborting the run. Passing it
/// explicitly is how this can be tested without mutating the process environment, which is
/// unsafe and would race the other tests in this binary; clap resolves an explicit value and
/// an environment value through the same parser, so this covers both.
#[test]
fn empty_dosroot_parses_and_is_not_a_c_drive() {
    let cli = Cli::try_parse_from(["izarravm", "--dosroot", ""])
        .expect("an empty dosroot must not fail the parse");
    assert_eq!(cli.dosroot.as_deref(), Some(Path::new("")));
    assert!(
        crate::startup::c_drive_override(&cli).is_none(),
        "an empty dosroot must not become the C: root"
    );
}

#[test]
fn cli_accepts_explicit_interpreter_backend() {
    let cli = Cli::try_parse_from(["izarravm", "--interpreter"]).unwrap();
    assert!(cli.interpreter);
}

#[test]
fn execution_backend_requires_avx2_only_for_native_builds() {
    assert_eq!(
        requested_execution_backend(false, true, true),
        Ok(ExecutionBackend::Automatic)
    );
    assert!(
        requested_execution_backend(false, true, false)
            .unwrap_err()
            .contains("AVX2")
    );
    assert_eq!(
        requested_execution_backend(true, true, false),
        Ok(ExecutionBackend::Interpreter)
    );
    assert_eq!(
        requested_execution_backend(false, false, false),
        Ok(ExecutionBackend::Interpreter)
    );
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
#[test]
#[allow(clippy::assertions_on_constants)]
fn windows_build_keeps_avx2_runtime_detectable() {
    assert!(
        !cfg!(target_feature = "avx2"),
        "the portable host binary must not enable AVX2 at compile time"
    );
}

#[test]
fn cli_accepts_hdd_folder_profile_json_output() {
    let cli = Cli::try_parse_from([
        "izarravm",
        "--hdd-folder",
        "game",
        "--profile-json",
        "results/run.json",
        "--expect-test-exit",
    ])
    .unwrap();

    assert_eq!(cli.hdd_folder.as_deref(), Some(Path::new("game")));
    assert_eq!(
        cli.profile_json.as_deref(),
        Some(Path::new("results/run.json"))
    );
    assert!(cli.expect_test_exit);
}

#[test]
fn machine_profile_environment_gate_is_explicit() {
    for value in [None, Some(""), Some("0")] {
        assert!(!machine_profile_requested(value));
    }
    for value in [Some("1"), Some("true"), Some("enabled")] {
        assert!(machine_profile_requested(value));
    }
}

#[test]
fn cli_rejects_multiple_run_modes() {
    for arguments in [
        vec![
            "izarravm",
            "--hdd-folder",
            "game",
            "--headless-profile-exe",
            "probe.exe",
        ],
        vec!["izarravm", "--hdd-folder", "game", "--headless-bench"],
    ] {
        assert!(Cli::try_parse_from(arguments).is_err());
    }
}

#[test]
fn hdd_profile_json_reports_fixed_time_and_native_metrics() {
    let dir = munt_test_dir("hdd-profile");
    let path = dir.join("run.json");
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw486;
    let mut machine = Machine::new_raw_program(profile, &[0xb8, 0x00, 0x4c, 0xcd, 0x21])
        .expect("build raw machine");
    machine.enable_machine_profiling();
    let stop = machine
        .run_until_halt_or_cycles(100_000)
        .expect("run raw program");

    write_hdd_profile_json(
        &path,
        Path::new("fixture"),
        GswMode::Gsw486,
        100_000,
        std::time::Duration::from_secs(1),
        &stop,
        Some((35, 70)),
        &machine,
    )
    .expect("write profile JSON");

    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(report["schema"], "izarravm-hdd-profile-v1");
    assert_eq!(report["mode"], "486");
    assert_eq!(report["stop"]["kind"], "dos_exit");
    assert_eq!(report["stop"]["code"], 0);
    assert_eq!(report["timedemo"]["gametics"], 35);
    assert_eq!(report["machine_phase_timing_enabled"], true);
    assert!(report["guest_seconds"].as_f64().unwrap() > 0.0);
    assert!(report["direct_native_coverage"].as_f64().is_some());
    assert!(
        report["direct_slow_exits_per_100_instructions"]
            .as_f64()
            .is_some()
    );
    assert!(report["perf"]["instructions"].as_u64().unwrap() > 0);
    assert!(report["raw_bus_clocks"].as_u64().unwrap() > 0);
    assert_eq!(
        report["scaled_bus_clocks"].as_u64(),
        Some(machine.scaled_bus_clocks())
    );
    assert!(machine.scaled_bus_clocks() > 0);
    assert!(report["perf"]["jit_direct_entries"].as_u64().is_some());
    assert!(report["perf"]["jit_direct_insns"].as_u64().is_some());
    assert!(report["perf"]["jit_direct_side_exits"].as_u64().is_some());
    assert!(report["direct_barrier_census"].is_null());
    #[cfg(feature = "direct-link-refusal-census")]
    assert_eq!(
        report.get("direct_link_refusal_census"),
        Some(&serde_json::Value::Null)
    );
    #[cfg(not(feature = "direct-link-refusal-census"))]
    assert!(report.get("direct_link_refusal_census").is_none());
    #[cfg(feature = "direct-callout-attribution")]
    assert_eq!(
        report.get("direct_callout_attribution"),
        Some(&serde_json::Value::Null)
    );
    #[cfg(not(feature = "direct-callout-attribution"))]
    assert!(report.get("direct_callout_attribution").is_none());
    for field in [
        "jit_direct_exit_cross_page_or_alignment",
        "jit_direct_exit_unavailable_or_kind",
        "jit_direct_exit_permission",
        "jit_direct_exit_code_watch",
        "jit_direct_exit_other",
        "jit_direct_unresolved_static_unbound",
        "jit_direct_unresolved_static_hidden",
        "jit_direct_unresolved_dynamic_miss_or_unbound",
        "jit_direct_unresolved_dynamic_hidden",
        "jit_direct_decode_dependencies_scanned",
        "jit_direct_portals_hidden",
        "jit_direct_arena_compactions",
        "jit_direct_arena_compaction_live_blocks",
        "jit_direct_arena_compaction_bytes",
        "jit_direct_arena_compaction_failures",
        "jit_direct_arena_compaction_ns",
        "jit_direct_reject_observer",
        "jit_direct_reject_interrupt_shadow",
        "jit_direct_reject_aggregate_accounting",
        "jit_direct_reject_mode_key",
        "jit_direct_reject_x87_top",
        "jit_direct_reject_cs_layout",
        "jit_direct_reject_cpl",
        "jit_direct_reject_data_segment",
        "jit_direct_reject_alignment",
        "jit_direct_reject_fetch_limit",
        "jit_direct_reject_zero_budget",
    ] {
        assert!(report["perf"][field].as_u64().is_some());
    }
    assert!(report["classified_wall_ns"].as_u64().is_some());
    assert!(report["unattributed_wall_ns"].as_u64().is_some());
    assert!(
        report["classified_wall_ns"].as_u64().unwrap()
            <= std::time::Duration::from_secs(1).as_nanos() as u64
    );
    assert!(
        report["machine_phases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|phase| phase["name"] == "cpu_batch" && phase["count"].as_u64().unwrap() > 0)
    );
    for name in ["video_conversion", "audio_render"] {
        assert!(
            report["machine_phases"]
                .as_array()
                .unwrap()
                .iter()
                .any(|phase| phase["name"] == name)
        );
    }

    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(feature = "direct-admission-census")]
#[test]
fn direct_barrier_census_json_exposes_ordered_zero_admission_declines() {
    let mut cpu = izarravm_cpu::CpuGsw::default();
    assert!(direct_barrier_census_json(cpu.direct_barrier_census_snapshot()).is_null());
    cpu.enable_direct_barrier_census(true);

    let report = direct_barrier_census_json(cpu.direct_barrier_census_snapshot());
    assert_eq!(
        report["admission_declines"],
        serde_json::json!([
            { "kind": "heat_refusal", "count": 0 },
            { "kind": "key_failure", "count": 0 },
            { "kind": "dormant_probe", "count": 0 },
            { "kind": "rejected_probe", "count": 0 },
        ])
    );
}

#[cfg(not(feature = "direct-admission-census"))]
#[test]
fn direct_barrier_census_json_omits_admission_declines_without_feature() {
    let mut cpu = izarravm_cpu::CpuGsw::default();
    cpu.enable_direct_barrier_census(true);

    let report = direct_barrier_census_json(cpu.direct_barrier_census_snapshot());
    assert!(report.get("admission_declines").is_none());
}

/// The closure block is a property of the census's own output, so its key set is pinned here.
///
/// All keys live inside `direct_barrier_census`; none is a `perf` key, which is why
/// `perf_counter_json_exposes_the_complete_counter_surface` is untouched by this feature.
#[cfg(feature = "barrier-census-closure")]
#[test]
fn direct_barrier_census_json_exposes_the_closure_block() {
    let mut cpu = izarravm_cpu::CpuGsw::default();
    assert!(direct_barrier_census_json(cpu.direct_barrier_census_snapshot()).is_null());
    cpu.enable_direct_barrier_census(true);

    let report = direct_barrier_census_json(cpu.direct_barrier_census_snapshot());
    assert_eq!(
        report["closure"],
        serde_json::json!({
            "classified_static": 0,
            "static_unbound_exits": 0,
            "unattributed_static": 0,
            "classified_dynamic": 0,
            "dynamic_miss_exits": 0,
            "unattributed_dynamic": 0,
            "rejected_unattributed": 0,
            "dynamic_rejected_unattributed": 0,
            "rejected_barrier_overwrites": 0,
        })
    );
}

/// B.3's histogram block is a SIBLING of `closure`, and its key set is pinned the same way and for
/// the same reason: this JSON is what the campaign diffs, so a schema change must fail a test
/// rather than quietly appear in a report.
///
/// The per-site rows are exercised by the cpu-crate fixtures; what this pins is the block's own
/// shape, including the two `unattributed_*` keys, which are the C3 identity made readable
/// in-place instead of reconstructed by hand from two other blocks.
#[cfg(feature = "barrier-census-closure")]
#[test]
fn direct_barrier_census_json_exposes_the_dormant_heat_block() {
    let mut cpu = izarravm_cpu::CpuGsw::default();
    cpu.enable_direct_barrier_census(true);

    let report = direct_barrier_census_json(cpu.direct_barrier_census_snapshot());
    assert_eq!(
        report["dormant_heat"],
        serde_json::json!({
            "class_static": 0,
            "class_dynamic": 0,
            "head_static": 0,
            "head_dynamic": 0,
            "truncated_static": 0,
            "truncated_dynamic": 0,
            "unattributed_static": 0,
            "unattributed_dynamic": 0,
            "distinct_sites": 0,
            "walked_entries_run_wide": 0,
            "sites": [],
        })
    );
}

/// A site with exits actually reaches the JSON, with the lane-match columns carried.
///
/// The zero-snapshot pin above would pass on a build whose `sites` array was hard-coded empty, and
/// an empty array is exactly what a diffuse-versus-concentrated reading would misread as
/// "concentrated on nothing".
#[cfg(feature = "barrier-census-closure")]
#[test]
fn direct_barrier_census_json_carries_dormant_heat_sites_and_their_lane_match() {
    let mut cpu = izarravm_cpu::CpuGsw::default();
    cpu.enable_direct_barrier_census(true);
    cpu.note_dormant_heat_exit_for_test(0x1234, false);
    cpu.note_dormant_heat_exit_for_test(0x1234, false);
    cpu.note_dormant_heat_exit_for_test(0x1234, true);

    let report = direct_barrier_census_json(cpu.direct_barrier_census_snapshot());
    assert_eq!(report["dormant_heat"]["class_static"], 2);
    assert_eq!(report["dormant_heat"]["class_dynamic"], 1);
    assert_eq!(report["dormant_heat"]["head_static"], 2);
    assert_eq!(report["dormant_heat"]["unattributed_static"], 0);
    assert_eq!(report["dormant_heat"]["unattributed_dynamic"], 0);
    assert_eq!(report["dormant_heat"]["distinct_sites"], 1);
    assert_eq!(
        report["dormant_heat"]["sites"],
        serde_json::json!([{
            // Hex, per the vga_wipe_census_json precedent: a guest linear is cross-referenced
            // against a map file or a disassembly, and both are hex.
            "linear": "0x00001234",
            "static_exits": 2,
            "dynamic_exits": 1,
            "compile_walked": false,
            "imm_lane_matched": false,
            "disp_lane_matched": false,
        }])
    );
}

/// The default build emits no `closure` key at all, rather than a block of nulls: a placeholder
/// would change every profile JSON the campaign diffs against without announcing it.
#[cfg(not(feature = "barrier-census-closure"))]
#[test]
fn direct_barrier_census_json_omits_the_closure_block_without_feature() {
    let mut cpu = izarravm_cpu::CpuGsw::default();
    cpu.enable_direct_barrier_census(true);

    let report = direct_barrier_census_json(cpu.direct_barrier_census_snapshot());
    assert!(report.get("closure").is_none());
    assert!(report.get("dormant_heat").is_none());
}

#[cfg(feature = "direct-link-refusal-census")]
#[test]
fn direct_link_refusal_census_json_exposes_ordered_zero_snapshot() {
    let mut cpu = izarravm_cpu::CpuGsw::default();
    assert!(direct_link_refusal_census_json(cpu.direct_link_refusal_census_snapshot()).is_null());
    cpu.enable_direct_link_refusal_census(true);
    assert_eq!(
        direct_link_refusal_census_json(cpu.direct_link_refusal_census_snapshot()),
        serde_json::json!({
            "seen": 0,
            "missing_id": 0,
            "invalid_id": 0,
            "rows": [],
        })
    );
}

#[cfg(feature = "direct-link-refusal-census")]
#[test]
fn direct_link_refusal_census_json_preserves_nonzero_rows_and_closures() {
    let snapshot = izarravm_cpu::DirectLinkRefusalCensusSnapshot {
        seen: 3,
        missing_id: 1,
        invalid_id: 1,
        rows: vec![izarravm_cpu::DirectLinkRefusalCensusRow {
            id: 7,
            source_linear: 0x1000,
            source_physical: 0x2000,
            source_mode_key: 3,
            source_generation: 11,
            slot: 1,
            target_linear: 0x1100,
            target_mode_key: 3,
            last_target_generation: Some(12),
            state: "refused_segment_layout",
            unbound_exits: 1,
            buckets: vec![
                ("suppressed", 0),
                ("not_attempted", 0),
                ("refused_segment_layout", 1),
            ],
        }],
    };
    assert_eq!(
        snapshot.seen,
        snapshot.missing_id
            + snapshot.invalid_id
            + snapshot
                .rows
                .iter()
                .map(|row| row.unbound_exits)
                .sum::<u64>()
    );
    assert_eq!(
        snapshot.rows[0].unbound_exits,
        snapshot.rows[0]
            .buckets
            .iter()
            .map(|(_, count)| count)
            .sum::<u64>()
    );

    let report = direct_link_refusal_census_json(Some(snapshot));
    assert_eq!(report["seen"], 3);
    assert_eq!(report["rows"][0]["id"], 7);
    assert_eq!(report["rows"][0]["source"]["generation"], 11);
    assert_eq!(report["rows"][0]["target"]["last_attempted_generation"], 12);
    assert_eq!(report["rows"][0]["buckets"][2]["count"], 1);
}

#[cfg(feature = "direct-callout-attribution")]
#[test]
fn direct_callout_attribution_json_has_the_exact_ordered_schema() {
    use izarravm_cpu::{
        DirectCallOutAttributionHelperRow as HelperRow, DirectCallOutAttributionPortRow as PortRow,
        DirectCallOutAttributionSnapshot as Snapshot, DirectCallOutOutcomeCounts as Counts,
    };

    let snapshot = Snapshot {
        helpers: vec![
            HelperRow {
                helper: "in_al_dx",
                counts: Counts {
                    attempts: 3,
                    continued: 1,
                    step_break: 1,
                    abnormal: 1,
                },
            },
            HelperRow {
                helper: "pushad",
                counts: Counts {
                    attempts: 2,
                    continued: 2,
                    step_break: 0,
                    abnormal: 0,
                },
            },
            HelperRow {
                helper: "popad",
                counts: Counts {
                    attempts: 1,
                    continued: 0,
                    step_break: 0,
                    abnormal: 1,
                },
            },
        ],
        ports: vec![
            PortRow {
                port: 0x0201,
                counts: Counts {
                    attempts: 1,
                    continued: 1,
                    step_break: 0,
                    abnormal: 0,
                },
            },
            PortRow {
                port: 0x03da,
                counts: Counts {
                    attempts: 2,
                    continued: 0,
                    step_break: 1,
                    abnormal: 1,
                },
            },
        ],
        totals: Counts {
            attempts: 6,
            continued: 3,
            step_break: 1,
            abnormal: 2,
        },
    };

    assert_eq!(
        direct_callout_attribution_json(Some(snapshot)),
        serde_json::json!({
            "schema": "izarravm-direct-callout-attribution-v1",
            "helpers": [
                { "helper": "in_al_dx", "attempts": 3, "continued": 1, "step_break": 1, "abnormal": 1 },
                { "helper": "pushad", "attempts": 2, "continued": 2, "step_break": 0, "abnormal": 0 },
                { "helper": "popad", "attempts": 1, "continued": 0, "step_break": 0, "abnormal": 1 },
            ],
            "ports": [
                { "port": 0x0201, "attempts": 1, "continued": 1, "step_break": 0, "abnormal": 0 },
                { "port": 0x03da, "attempts": 2, "continued": 0, "step_break": 1, "abnormal": 1 },
            ],
            "totals": { "attempts": 6, "continued": 3, "step_break": 1, "abnormal": 2 },
        })
    );
}

#[cfg(feature = "direct-callout-attribution")]
#[test]
#[should_panic]
fn direct_callout_attribution_json_rejects_an_open_row() {
    use izarravm_cpu::{
        DirectCallOutAttributionHelperRow as HelperRow,
        DirectCallOutAttributionSnapshot as Snapshot, DirectCallOutOutcomeCounts as Counts,
    };
    let open = Counts {
        attempts: 1,
        ..Counts::default()
    };
    let _ = direct_callout_attribution_json(Some(Snapshot {
        helpers: ["in_al_dx", "pushad", "popad"]
            .into_iter()
            .map(|helper| HelperRow {
                helper,
                counts: open,
            })
            .collect(),
        ports: Vec::new(),
        totals: Counts {
            attempts: 3,
            ..Counts::default()
        },
    }));
}

#[test]
fn perf_counter_json_exposes_the_complete_counter_surface() {
    let perf = PerfCounters {
        brk_cont_decode_miss: 101,
        brk_cont_not_continuable: 102,
        brk_cont_page_cross: 103,
        decode_inval_cs_load: 104,
        decode_inval_smc: 105,
        decode_inval_other: 106,
        smc_heat_demotions: 107,
        smc_heat_chunks_hot: 108,
        monitor_trips_vec13: 110,
        monitor_resident_core_clocks: 111,
        ..PerfCounters::default()
    };
    let fast_map_probe = izarravm_cpu::FastMapProbeCounters {
        hits: 113,
        misses: 114,
    };
    // Built field by field, never from `default()`: `census_enabled` reads the environment, so a
    // `default()` here would make the assertions depend on whether the census happens to be
    // armed in the shell that runs the test.
    let fast_map_audit = izarravm_cpu::FastMapAuditCounters {
        wipes_direct_map: 115,
        wipes_direct_data_map: 116,
        wipes_a20: 117,
        wipes_tlb_flush: 118,
        wipes_admission: 119,
        wipe_pages_cleared: 123,
        wipe_vga_pages_cleared: 124,
        wipe_aperture_pages_cleared: 125,
        census_reads: 120,
        census_writes: 121,
        census_rmw_pairs: 122,
        census_enabled: true,
        slot_reject_misaligned: 130,
        slot_reject_page_cross: 131,
        slot_reject_absent: 132,
        slot_reject_epoch: 133,
        slot_reject_permission: 134,
        slot_admit_misaligned: 135,
        slot_reject_enabled: true,
        last_read_insn: 0,
        last_read_page: u32::MAX,
    };

    let code_watch_edges = izarravm_cpu::CodeWatchEdgeCounters {
        sticky_page_edges: 126,
        block_page_edges: 127,
        block_page_releases: 128,
        sweep_cleared_entries: 129,
    };

    let report = bench::perf_counters_json(
        &perf,
        izarravm_cpu::PollSkipMemoryCounters::default(),
        fast_map_probe,
        fast_map_audit,
        code_watch_edges,
        None,
    );
    let object = report.as_object().unwrap();
    let keys: Vec<_> = object.keys().map(String::as_str).collect();
    assert_eq!(keys, PERF_COUNTER_KEYS);
    for (key, expected) in [
        ("brk_cont_decode_miss", 101),
        ("brk_cont_not_continuable", 102),
        ("brk_cont_page_cross", 103),
        ("decode_inval_cs_load", 104),
        ("decode_inval_smc", 105),
        ("decode_inval_other", 106),
        ("smc_heat_demotions", 107),
        ("smc_heat_chunks_hot", 108),
        ("monitor_trips_vec13", 110),
        ("monitor_resident_core_clocks", 111),
        ("interp_fast_map_hits", 113),
        ("interp_fast_map_misses", 114),
        ("fast_map_wipes_direct_map", 115),
        ("fast_map_wipes_direct_data_map", 116),
        ("fast_map_wipes_a20", 117),
        ("fast_map_wipes_tlb_flush", 118),
        ("fast_map_wipes_admission", 119),
        ("fast_map_wipe_pages_cleared", 123),
        ("fast_map_wipe_vga_pages_cleared", 124),
        ("fast_map_wipe_aperture_pages_cleared", 125),
        ("rmw_census_reads", 120),
        ("rmw_census_writes", 121),
        ("rmw_census_rmw_pairs", 122),
        ("rmw_census_enabled", 1),
        ("slot_reject_misaligned", 130),
        ("slot_reject_page_cross", 131),
        ("slot_reject_absent", 132),
        ("slot_reject_epoch", 133),
        ("slot_reject_permission", 134),
        ("slot_admit_misaligned", 135),
        ("slot_reject_enabled", 1),
        ("code_watch_sticky_page_edges", 126),
        ("code_watch_block_page_edges", 127),
        ("code_watch_block_page_releases", 128),
        ("code_watch_sweep_cleared", 129),
    ] {
        assert_eq!(
            object[key].as_u64(),
            Some(expected),
            "wrong value for {key}"
        );
    }

    let zeros = bench::perf_counters_json(
        &PerfCounters::default(),
        izarravm_cpu::PollSkipMemoryCounters::default(),
        izarravm_cpu::FastMapProbeCounters::default(),
        izarravm_cpu::FastMapAuditCounters {
            census_enabled: false,
            last_read_page: 0,
            ..Default::default()
        },
        izarravm_cpu::CodeWatchEdgeCounters::default(),
        None,
    );
    let zero_object = zeros.as_object().unwrap();
    assert_eq!(zero_object.len(), PERF_COUNTER_KEYS.len());
    assert!(zero_object.values().all(|value| value.as_u64() == Some(0)));
}

#[test]
fn perf_counter_inventory_guard_covers_every_struct_field() {
    let PerfCounters {
        instructions: _,
        decode_misses: _,
        decode_probes: _,
        jit_direct_dispatch_declines: _,
        straight_line_runs: _,
        brk_decode_or_branch: _,
        brk_cont_decode_miss: _,
        brk_cont_not_continuable: _,
        brk_cont_page_cross: _,
        brk_step: _,
        brk_interrupt: _,
        brk_cap: _,
        brk_halt: _,
        decode_inval_cs_load: _,
        decode_inval_smc: _,
        decode_inval_other: _,
        code_invalidations: _,
        smc_narrow_kills: _,
        smc_lane_registrations: _,
        smc_lane_accepts: _,
        smc_scan_calls: _,
        smc_scan_keys: _,
        smc_lane_reject_width: _,
        smc_lane_reject_address: _,
        smc_heat_demotions: _,
        smc_heat_chunks_hot: _,
        device_write_ranges: _,
        device_write_bytes: _,
        device_write_code_hits: _,
        device_write_coarse_resets: _,
        jit_direct_entries: _,
        jit_direct_insns: _,
        jit_direct_side_exits: _,
        jit_direct_exit_cross_page_or_alignment: _,
        jit_direct_exit_unavailable_or_kind: _,
        jit_direct_exit_permission: _,
        jit_direct_exit_code_watch: _,
        jit_direct_exit_other: _,
        jit_direct_compile_attempts: _,
        jit_direct_blocks_installed: _,
        jit_direct_compile_ns: _,
        jit_direct_hot_hits: _,
        jit_direct_hash_hits: _,
        jit_direct_lookup_misses: _,
        jit_direct_linked_transfers: _,
        jit_direct_unresolved_exits: _,
        jit_direct_unresolved_static_unbound: _,
        jit_direct_unresolved_static_hidden: _,
        jit_direct_unresolved_dynamic_miss_or_unbound: _,
        jit_direct_unresolved_dynamic_hidden: _,
        jit_direct_deferred_short: _,
        jit_direct_word_address_slots: _,
        jit_direct_blocks_installed_sixteen_bit: _,
        jit_direct_entries_sixteen_bit: _,
        jit_direct_insns_sixteen_bit: _,
        jit_direct_word_control_admitted: _,
        jit_direct_word_control_refused: _,
        jit_direct_reject_observer: _,
        jit_direct_reject_interrupt_shadow: _,
        jit_direct_reject_aggregate_accounting: _,
        poll_skip_spans: _,
        poll_skip_iterations: _,
        poll_neg_cache_hits: _,
        poll_neg_cache_stores: _,
        poll_neg_cache_volatile: _,
        poll_head_prefilter_rejects: _,
        jit_direct_reject_mode_key: _,
        jit_direct_reject_x87_top: _,
        jit_direct_reject_cs_layout: _,
        jit_direct_reject_cpl: _,
        jit_direct_reject_data_segment: _,
        jit_direct_reject_alignment: _,
        jit_direct_reject_fetch_limit: _,
        jit_direct_reject_zero_budget: _,
        jit_direct_chain_quota_entries: _,
        jit_direct_chain_quota_cache_misses: _,
        jit_direct_x87_pad_bails: _,
        jit_direct_cache_resets: _,
        jit_direct_arena_compactions: _,
        jit_direct_arena_compaction_live_blocks: _,
        jit_direct_arena_compaction_bytes: _,
        jit_direct_arena_compaction_failures: _,
        jit_direct_arena_compaction_ns: _,
        jit_direct_links_created: _,
        jit_direct_links_cleared: _,
        jit_direct_decode_dependencies_scanned: _,
        jit_direct_portals_hidden: _,
        jit_native_load_hits: _,
        jit_native_store_hits: _,
        jit_paged_tlb_successes: _,
        data_direct_reads: _,
        data_slow_reads: _,
        data_direct_writes: _,
        data_slow_writes: _,
        direct_page_hits: _,
        direct_page_misses: _,
        direct_data_pointer_reads: _,
        direct_data_pointer_writes: _,
        fetch_page_hits: _,
        fetch_page_misses: _,
        slow_prefetch_refills: _,
        direct_map_invalidations: _,
        rep_string_iterations: _,
        rep_string_fast_iterations: _,
        flag_materializations: _,
        cache_tier_lookups: _,
        monitor_trips_vec13: _,
        monitor_resident_core_clocks: _,
    } = PerfCounters::default();
    let izarravm_cpu::PollSkipMemoryCounters {
        spans: _,
        iterations: _,
    } = izarravm_cpu::PollSkipMemoryCounters::default();
    let izarravm_cpu::FastMapProbeCounters { hits: _, misses: _ } =
        izarravm_cpu::FastMapProbeCounters::default();
    let izarravm_cpu::FastMapAuditCounters {
        wipes_direct_map: _,
        wipes_direct_data_map: _,
        wipes_a20: _,
        wipes_tlb_flush: _,
        wipes_admission: _,
        wipe_pages_cleared: _,
        wipe_vga_pages_cleared: _,
        wipe_aperture_pages_cleared: _,
        census_reads: _,
        census_writes: _,
        census_rmw_pairs: _,
        census_enabled: _,
        slot_reject_misaligned: _,
        slot_reject_page_cross: _,
        slot_reject_absent: _,
        slot_reject_epoch: _,
        slot_reject_permission: _,
        slot_admit_misaligned: _,
        slot_reject_enabled: _,
        last_read_insn: _,
        last_read_page: _,
    } = izarravm_cpu::FastMapAuditCounters::default();
    let izarravm_cpu::CodeWatchEdgeCounters {
        sticky_page_edges: _,
        block_page_edges: _,
        block_page_releases: _,
        sweep_cleared_entries: _,
    } = izarravm_cpu::CodeWatchEdgeCounters::default();
}

#[test]
fn hdd_profile_json_does_not_enable_machine_phase_timing() {
    let dir = munt_test_dir("hdd-profile-uninstrumented");
    let path = dir.join("run.json");
    let mut machine = Machine::new_raw_program(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        &[0xb8, 0x00, 0x4c, 0xcd, 0x21],
    )
    .expect("build raw machine");
    let stop = machine
        .run_until_halt_or_cycles(100_000)
        .expect("run raw program");

    write_hdd_profile_json(
        &path,
        Path::new("fixture"),
        GswMode::Gsw386,
        100_000,
        std::time::Duration::from_secs(1),
        &stop,
        None,
        &machine,
    )
    .expect("write profile JSON");

    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(report["machine_phase_timing_enabled"], false);
    assert_eq!(report["classified_wall_ns"], 0);
    assert!(
        report["machine_phases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|phase| phase["wall_ns"] == 0 && phase["count"] == 0)
    );

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn stop_reason_json_preserves_each_outcome() {
    let cases = [
        (StopReason::Halted, json!({ "kind": "halted" })),
        (
            StopReason::CycleLimit { requested: 123 },
            json!({ "kind": "cycle_limit", "requested": 123 }),
        ),
        (
            StopReason::CpuError("bad opcode".into()),
            json!({ "kind": "cpu_error", "message": "bad opcode" }),
        ),
        (
            StopReason::DosExit { code: 7 },
            json!({ "kind": "dos_exit", "code": 7 }),
        ),
        (
            StopReason::TestExit { code: 9 },
            json!({ "kind": "test_exit", "code": 9 }),
        ),
    ];

    for (stop, expected) in cases {
        assert_eq!(stop_reason_json(&stop), expected);
    }
}

#[test]
fn ascii_to_set1_maps_a_letter_to_make_and_break() {
    assert_eq!(ascii_to_set1('h'), vec![0x23, 0xa3]);
    // Uppercase wraps the key in left-Shift make/break.
    assert_eq!(ascii_to_set1('H'), vec![0x2a, 0x23, 0xa3, 0xaa]);
    // Enter is the unshifted return key.
    assert_eq!(ascii_to_set1('\r'), vec![0x1c, 0x9c]);
    // A shifted number-row glyph holds Shift over the digit key.
    assert_eq!(ascii_to_set1('!'), vec![0x2a, 0x02, 0x82, 0xaa]);
    // Characters with no US-layout key produce nothing.
    assert!(ascii_to_set1('\u{00f1}').is_empty());
}

#[test]
fn write_framebuffer_ppm_uses_the_active_distira_scanout() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.write_physical_u32(
        izarravm_machine::DISTIRA_MMIO_BASE + izarravm_video::DISTIRA_REG_CLEAR_COLOR as u32,
        0x0010_2030,
    );
    machine.write_physical_u32(
        izarravm_machine::DISTIRA_MMIO_BASE + izarravm_video::DISTIRA_REG_COMMAND as u32,
        izarravm_video::DISTIRA_CMD_CLEAR,
    );
    machine.write_physical_u32(
        izarravm_machine::DISTIRA_MMIO_BASE + izarravm_video::DISTIRA_REG_COMMAND as u32,
        izarravm_video::DISTIRA_CMD_SWAP,
    );
    assert_eq!(machine.active_display(), ActiveDisplay::Distira);

    let dir = std::env::temp_dir().join(format!(
        "izarravm_ppm_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("frame.ppm");

    write_framebuffer_ppm(&mut machine, &path).expect("write ppm");
    let bytes = std::fs::read(&path).expect("read ppm back");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(bytes.starts_with(b"P6\n"));
    let header = String::from_utf8_lossy(&bytes[..32.min(bytes.len())]);
    let mut parts = header.split_whitespace();
    assert_eq!(parts.next(), Some("P6"));
    let width: usize = parts.next().unwrap().parse().unwrap();
    let height: usize = parts.next().unwrap().parse().unwrap();
    assert_eq!((width, height), (640, 480));
}

#[test]
fn langid_maps_to_guest_layout_index() {
    assert_eq!(layout_index_from_langid(0x0409), 0); // en-US
    assert_eq!(layout_index_from_langid(0x0809), 1); // en-GB
    assert_eq!(layout_index_from_langid(0x1009), 0); // en-CA -> US
    assert_eq!(layout_index_from_langid(0x0c0a), 2); // es-ES
    assert_eq!(layout_index_from_langid(0x080a), 16); // es-MX -> Latin America
    assert_eq!(layout_index_from_langid(0x040c), 3); // fr-FR
    assert_eq!(layout_index_from_langid(0x0407), 4); // de-DE
    assert_eq!(layout_index_from_langid(0x0410), 5); // it-IT
    assert_eq!(layout_index_from_langid(0x0411), 0); // ja-JP -> US fallback
}

#[test]
fn langid_maps_new_layouts() {
    assert_eq!(layout_index_from_langid(0x080c), 6); // fr-BE -> BE
    assert_eq!(layout_index_from_langid(0x0c0c), 7); // fr-CA -> CF
    assert_eq!(layout_index_from_langid(0x0406), 8); // da-DK -> DK
    assert_eq!(layout_index_from_langid(0x0413), 9); // nl-NL -> NL
    assert_eq!(layout_index_from_langid(0x0414), 10); // nb-NO -> NO
    assert_eq!(layout_index_from_langid(0x0816), 11); // pt-PT -> PO
    assert_eq!(layout_index_from_langid(0x100c), 12); // fr-CH -> SF
    assert_eq!(layout_index_from_langid(0x0807), 13); // de-CH -> SG
    assert_eq!(layout_index_from_langid(0x040b), 14); // fi-FI -> SU
    assert_eq!(layout_index_from_langid(0x041d), 15); // sv-SE -> SV
}

#[test]
fn codepage_index_for_each_layout() {
    let want = [0u8, 0, 1, 1, 1, 1, 1, 3, 4, 1, 4, 2, 1, 1, 1, 1, 1];
    for (i, w) in want.iter().enumerate() {
        assert_eq!(codepage_index_for_layout(i as u8), *w);
    }
}

#[test]
fn katea_run_prog_name_picks_a_clean_8_3_name() {
    use std::path::Path;
    assert_eq!(katea_run_prog_name(Path::new("/x/FOO.EXE")), "PROG.EXE");
    assert_eq!(katea_run_prog_name(Path::new("bar.com")), "PROG.COM");
    assert_eq!(katea_run_prog_name(Path::new("noext")), "PROG.COM");
    assert_eq!(katea_run_prog_name(Path::new("a.longext")), "PROG.LON");
}

#[test]
fn boot_suite_failure_summary_lists_every_failed_record() {
    let mut results = SuiteResults {
        version: 1,
        declared_record_count: 3,
        payload_len: 0,
        checksum: 0,
        records: vec![
            izarravm_firmware::SuiteRecord {
                status: SuiteRecordStatus::Begin,
                name: "suite.boot".to_string(),
                value: None,
            },
            izarravm_firmware::SuiteRecord {
                status: SuiteRecordStatus::Fail,
                name: "sound.opl3".to_string(),
                value: None,
            },
            izarravm_firmware::SuiteRecord {
                status: SuiteRecordStatus::Fail,
                name: "timer.irq0".to_string(),
                value: None,
            },
        ],
    };

    assert_eq!(
        boot_suite_failure_summary(&results).as_deref(),
        Some("boot suite reported FAIL: sound.opl3, timer.irq0")
    );
    results
        .records
        .retain(|record| record.status != SuiteRecordStatus::Fail);
    assert_eq!(boot_suite_failure_summary(&results), None);
}

fn passing_izarra_results() -> SuiteResults {
    let records = IZARRA_BIOS_REQUIRED_RECORDS
        .iter()
        .map(|&(name, status)| izarravm_firmware::SuiteRecord {
            status,
            name: name.to_owned(),
            value: (status == SuiteRecordStatus::Measure).then(|| "value".to_owned()),
        })
        .collect::<Vec<_>>();
    SuiteResults {
        version: 1,
        declared_record_count: records.len() as u16,
        payload_len: 0,
        checksum: 0,
        records,
    }
}

#[test]
fn izarra_bios_gate_accepts_one_complete_passing_record_set() {
    assert_eq!(izarra_bios_failure_summary(&passing_izarra_results()), None);
}

#[test]
fn izarra_bios_gate_rejects_incomplete_duplicate_missing_and_failed_records() {
    let mut incomplete = passing_izarra_results();
    incomplete.declared_record_count += 1;
    assert!(
        izarra_bios_failure_summary(&incomplete)
            .unwrap()
            .contains("declared 20 records but parsed 19")
    );

    let mut duplicate = passing_izarra_results();
    duplicate.records.push(duplicate.records[1].clone());
    duplicate.declared_record_count += 1;
    assert!(
        izarra_bios_failure_summary(&duplicate)
            .unwrap()
            .contains("duplicate records: self.framework")
    );

    let mut missing = passing_izarra_results();
    missing
        .records
        .retain(|record| record.name != "component.timer_pit");
    missing.declared_record_count -= 1;
    assert!(
        izarra_bios_failure_summary(&missing)
            .unwrap()
            .contains("missing required record: component.timer_pit")
    );

    let mut failed = passing_izarra_results();
    failed
        .records
        .iter_mut()
        .find(|record| record.name == "component.audio_opl")
        .unwrap()
        .status = SuiteRecordStatus::Fail;
    assert!(
        izarra_bios_failure_summary(&failed)
            .unwrap()
            .contains("failed required record: component.audio_opl")
    );
}

#[test]
fn izarra_bios_gate_rejects_empty_measurements_and_unexpected_records() {
    let mut empty_measurement = passing_izarra_results();
    empty_measurement
        .records
        .iter_mut()
        .find(|record| record.name == "memory.detected_kib")
        .unwrap()
        .value = None;
    assert!(
        izarra_bios_failure_summary(&empty_measurement)
            .unwrap()
            .contains("required measurement has no value: memory.detected_kib")
    );

    let mut unexpected = passing_izarra_results();
    unexpected.records[1].name = "component.unsupported".to_owned();
    let message = izarra_bios_failure_summary(&unexpected).unwrap();
    assert!(message.contains("unexpected records: component.unsupported"));
    assert!(message.contains("missing required record: self.framework"));
}

#[cfg(feature = "jit")]
#[test]
fn unit_sim_report_lines_format_headline_and_histogram() {
    let report = izarravm_cpu::SimReport {
        entries: 1000,
        retired_in_units: 3500,
        linked_transfers: 42,
        unresolved_exits: 7,
        side_exits_io: 3,
        side_exits_async: 2,
        sim_invalidations: 5,
        units_built: 120,
        units_rebuilt: 4,
        ..Default::default()
    };
    // Ten units: member counts 1..=10 spread across code/UMA/BIOS entry pages. p50 (nearest rank of
    // 10 items) lands on the 5th smallest (5), p90 on the 9th (9), max 10. Two units sit in the
    // excluded window (page 0xF0 and page 0xA0); one code unit exceeds 64 members.
    let histogram: Vec<(usize, u32)> = vec![
        (1, 0x0010),
        (2, 0x0010),
        (3, 0x0010),
        (4, 0x0011),
        (5, 0x0011),
        (6, 0x0012),
        (7, 0x00f0),
        (8, 0x00a0),
        (9, 0x0012),
        (65, 0x0013),
    ];
    let reports = vec![("L0", report, histogram)];
    let lines = unit_sim_report_lines(&reports);
    assert_eq!(
        lines,
        vec![
            "unit_sim cfg=L0 entries=1000 retired_in_units=3500 linked_transfers=42 loop_links=0 \
call_links=0 ret_links=0 itc_hits=0 ght_hits=0 ght_ret_hits=0 unresolved_exits=7 side_exits_io=3 \
side_exits_async=2 io_callouts=0 sim_invalidations=5 sim_restamps=0 units_built=120 \
units_rebuilt=4 elided_insns=0 elided_waits=0 wait_batch_ends=0 spin_noio_insns=0 \
insns_per_entry=3.500000 ipe_active=3.500000 ipe_active_slice=3.500000"
                .to_string(),
            "unit_sim_hist cfg=L0 units=10 members_p50=5 members_p90=9 members_max=65 \
units_over_64=1 units_over_128=0 units_over_256=0 excl_units=2"
                .to_string(),
        ]
    );
}

#[cfg(feature = "jit")]
#[test]
fn unit_sim_report_lines_format_p_rung_both_quotients() {
    // The P rung: elided iterations and absorbed budget yields make the two active-stream quotients
    // diverge from each other and from the structural metric. entries=100, retired=1000,
    // elided_insns=400, wait_batch_ends=100. insns_per_entry = 1000/100 = 10; ipe_active =
    // (1000-400)/100 = 6; ipe_active_slice = 600/(100+100) = 3.
    let report = izarravm_cpu::SimReport {
        entries: 100,
        retired_in_units: 1000,
        io_callouts: 50,
        elided_insns: 400,
        elided_waits: 3,
        wait_batch_ends: 100,
        spin_noio_insns: 20,
        ..Default::default()
    };
    let reports = vec![("P", report, Vec::new())];
    let lines = unit_sim_report_lines(&reports);
    assert_eq!(
        lines[0],
        "unit_sim cfg=P entries=100 retired_in_units=1000 linked_transfers=0 loop_links=0 \
call_links=0 ret_links=0 itc_hits=0 ght_hits=0 ght_ret_hits=0 unresolved_exits=0 side_exits_io=0 \
side_exits_async=0 io_callouts=50 sim_invalidations=0 sim_restamps=0 units_built=0 units_rebuilt=0 \
elided_insns=400 elided_waits=3 wait_batch_ends=100 spin_noio_insns=20 insns_per_entry=10.000000 \
ipe_active=6.000000 ipe_active_slice=3.000000"
    );
}

#[cfg(feature = "jit")]
#[test]
fn io_hist_lines_format_top_ports_descending() {
    // Already-sorted (count desc, port asc) input; the formatter emits `io_hist port=0xNNN count=...`
    // and caps at the top 16.
    let hist: Vec<(u16, u64)> = vec![(0x03da, 4000), (0x0061, 1200), (0x0388, 30)];
    let lines = io_hist_lines(&hist);
    assert_eq!(
        lines,
        vec![
            "io_hist port=0x03da count=4000".to_string(),
            "io_hist port=0x0061 count=1200".to_string(),
            "io_hist port=0x0388 count=30".to_string(),
        ]
    );

    // A run of 20 ports is capped at 16 lines.
    let many: Vec<(u16, u64)> = (0..20u16).map(|i| (i, 100 - u64::from(i))).collect();
    assert_eq!(io_hist_lines(&many).len(), 16);
}

#[test]
fn slow_read_histo_lines_split_the_low_megabyte_into_the_regions_n2_asks_about() {
    // One page in each of the five sub-megabyte regions plus one above 1 MiB, so every boundary in
    // `SLOW_READ_REGIONS` is exercised from BOTH sides by its neighbours: 0x9f/0xa0, 0xaf/0xb0,
    // 0xbf/0xc0, 0xef/0xf0 and 0xff/0x100. An off-by-one in any of them moves a whole region's
    // count, which is exactly the mistake that would have sent N2's next slice to the wrong place.
    let pages: Vec<(u32, u64)> = vec![
        (0xa8, 500), // mode-Y aperture
        (0xd4, 300), // UMB / EMS page frame
        (0x9f, 100), // last conventional page
        (0xb8, 40),  // text
        (0xf0, 10),  // BIOS
        (0x100, 1),  // first page above 1 MiB
    ];
    let lines = slow_read_histo_lines(&pages, 960, 900, 951);
    assert_eq!(
        lines[..6],
        [
            "slow_read_region conventional_00000_9FFFF count=100 pct=10.52",
            "slow_read_region vga_aperture_A0000_AFFFF count=500 pct=52.58",
            "slow_read_region text_B0000_BFFFF count=40 pct=4.21",
            "slow_read_region umb_ems_C0000_EFFFF count=300 pct=31.55",
            "slow_read_region bios_F0000_FFFFF count=10 pct=1.05",
            "slow_read_region above_1MiB count=1 pct=0.11",
        ]
    );
    assert_eq!(
        lines[6],
        "slow_read_page page=0x000a8 linear=0x000a8000 count=500"
    );
    // The total line carries the histogram's own sum against `data_slow_reads`. They agree here;
    // a REP CMPS-heavy workload (whose destination read holds a physical address and is
    // deliberately not bucketed) shows up as a shortfall rather than as a mislabelled bucket.
    // The alignment split is what separates `should_split` (a word at an odd address) from
    // `ram_lookup_page_is_direct` (a region that is not direct RAM). Both refuse the same access
    // and both land in `jit_direct_exit_cross_page_or_alignment`, which is one counter for two
    // unrelated causes -- so the region table without this line cannot answer N2 at all.
    assert_eq!(
        lines[lines.len() - 2],
        "slow_read_align misaligned=900 of=951 pct=94.64"
    );
    assert_eq!(
        lines.last().unwrap(),
        "slow_read_total bucketed=951 data_slow_reads=960 distinct_pages=6"
    );

    // A 40-page run prints the top 24 pages and no more; the six region lines and the total are
    // not part of that cap.
    let many: Vec<(u32, u64)> = (0..40u32).map(|i| (i, 100 - u64::from(i))).collect();
    assert_eq!(slow_read_histo_lines(&many, 0, 0, 0).len(), 6 + 24 + 2);
}

#[cfg(feature = "jit")]
#[test]
fn unit_sim_report_lines_handle_empty_run() {
    // Two empty rungs prove the per-rung fan-out: each rung emits its own labeled pair, so the
    // four-rung measurement set would produce eight lines.
    let reports = vec![
        ("L0", izarravm_cpu::SimReport::default(), Vec::new()),
        ("L1", izarravm_cpu::SimReport::default(), Vec::new()),
    ];
    let lines = unit_sim_report_lines(&reports);
    assert_eq!(
        lines,
        vec![
            "unit_sim cfg=L0 entries=0 retired_in_units=0 linked_transfers=0 loop_links=0 \
call_links=0 ret_links=0 itc_hits=0 ght_hits=0 ght_ret_hits=0 unresolved_exits=0 side_exits_io=0 \
side_exits_async=0 io_callouts=0 sim_invalidations=0 sim_restamps=0 units_built=0 units_rebuilt=0 \
elided_insns=0 elided_waits=0 wait_batch_ends=0 spin_noio_insns=0 insns_per_entry=0.000000 \
ipe_active=0.000000 ipe_active_slice=0.000000"
                .to_string(),
            "unit_sim_hist cfg=L0 units=0 members_p50=0 members_p90=0 members_max=0 units_over_64=0 \
units_over_128=0 units_over_256=0 excl_units=0"
                .to_string(),
            "unit_sim cfg=L1 entries=0 retired_in_units=0 linked_transfers=0 loop_links=0 \
call_links=0 ret_links=0 itc_hits=0 ght_hits=0 ght_ret_hits=0 unresolved_exits=0 side_exits_io=0 \
side_exits_async=0 io_callouts=0 sim_invalidations=0 sim_restamps=0 units_built=0 units_rebuilt=0 \
elided_insns=0 elided_waits=0 wait_batch_ends=0 spin_noio_insns=0 insns_per_entry=0.000000 \
ipe_active=0.000000 ipe_active_slice=0.000000"
                .to_string(),
            "unit_sim_hist cfg=L1 units=0 members_p50=0 members_p90=0 members_max=0 units_over_64=0 \
units_over_128=0 units_over_256=0 excl_units=0"
                .to_string(),
        ]
    );
}

/// The keystroke text of a parsed step, for tests that only care about keys.
fn injected_text(step: &Injection) -> &str {
    match &step.event {
        InjectionEvent::Keys(text) => text,
        InjectionEvent::Mouse(_) => panic!("expected a key step"),
    }
}

#[test]
fn key_injection_steps_parse_with_increasing_offsets() {
    let steps = parse_key_injections("100:a;200:\r").unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].at_cycles, 100);
    assert_eq!(injected_text(&steps[0]), "a");
    assert_eq!(steps[1].at_cycles, 200);
    assert_eq!(
        injected_text(&steps[1]),
        "\r",
        "\\r must expand to a carriage return"
    );
    // Empty segments are skipped rather than being an error, so a trailing ';'
    // from a shell loop does not fail the run.
    assert_eq!(parse_key_injections("100:a;").unwrap().len(), 1);
    assert!(parse_key_injections("").unwrap().is_empty());
}

#[test]
fn key_injection_rejects_non_increasing_offsets() {
    // Strictly increasing is what makes the schedule deterministic: a step at
    // or before its predecessor would inject at a cycle already burned, so the
    // keystroke would land at a run-dependent point and break equal-work.
    assert!(parse_key_injections("200:a;200:b").is_err());
    assert!(parse_key_injections("200:a;100:b").is_err());
    assert!(
        parse_key_injections("100").is_err(),
        "missing ':' must fail"
    );
}

#[test]
fn scancode_groups_cover_named_keys_and_plain_text() {
    // A bare modifier is the reason named keys exist: SHIFT is a make/break
    // pair with no character behind it, and Prince of Persia advances its
    // title screen on exactly that.
    assert_eq!(
        text_to_scancode_groups("{shift}").unwrap(),
        vec![vec![0x2a, 0xaa]]
    );
    assert_eq!(
        text_to_scancode_groups("{esc}").unwrap(),
        vec![vec![0x01, 0x81]]
    );
    // Plain characters still go through the ASCII path, one group per key.
    let mixed = text_to_scancode_groups("a{shift}").unwrap();
    assert_eq!(mixed.len(), 2);
    assert_eq!(mixed[1], vec![0x2a, 0xaa]);
    assert_eq!(mixed[0], ascii_to_set1('a'));

    assert!(text_to_scancode_groups("{nosuchkey}").is_err());
    assert!(text_to_scancode_groups("{shift").is_err(), "unclosed brace");
}

/// The action of a parsed mouse step.
fn injected_action(step: &Injection) -> &MouseAction {
    match &step.event {
        InjectionEvent::Mouse(action) => action,
        InjectionEvent::Keys(_) => panic!("expected a mouse step"),
    }
}

#[test]
fn mouse_injection_parses_every_action() {
    let steps =
        parse_mouse_injections("100:home;200:move:-40,15;300:down;400:up;500:click").unwrap();
    assert_eq!(steps.len(), 5);
    assert!(matches!(injected_action(&steps[0]), MouseAction::Home));
    assert!(matches!(
        injected_action(&steps[1]),
        MouseAction::Move { dx: -40, dy: 15 }
    ));
    assert!(matches!(injected_action(&steps[2]), MouseAction::Button(1)));
    assert!(matches!(injected_action(&steps[3]), MouseAction::Button(0)));
    assert!(matches!(injected_action(&steps[4]), MouseAction::Click));
}

#[test]
fn mouse_injection_rejects_malformed_actions() {
    assert!(parse_mouse_injections("100:wiggle").is_err());
    assert!(
        parse_mouse_injections("100:move:12").is_err(),
        "move needs both axes"
    );
    assert!(parse_mouse_injections("100:move:a,b").is_err());
    // The offset rule is shared with --inject-keys and matters for the same
    // reason: a schedule that is not strictly increasing is not replayable.
    assert!(parse_mouse_injections("200:home;100:click").is_err());
}

#[test]
fn mouse_move_splits_into_packets_a_real_mouse_could_send() {
    // inject_mouse_relative CLAMPS to +-255 rather than splitting, matching the
    // hardware, so a move longer than one packet has to be split here or the
    // pointer silently stops short.
    let mut buttons = 0;
    let packets = mouse_action_packets(&MouseAction::Move { dx: 600, dy: -20 }, &mut buttons);
    assert_eq!(packets.len(), 3);
    assert_eq!(packets.iter().map(|p| p.dx).sum::<i32>(), 600);
    assert_eq!(packets.iter().map(|p| p.dy).sum::<i32>(), -20);
    for p in &packets {
        assert!(
            p.dx.abs() <= 255 && p.dy.abs() <= 255,
            "packet {},{} exceeds the PS/2 range",
            p.dx,
            p.dy
        );
    }

    // A move that fits stays one packet.
    let packets = mouse_action_packets(&MouseAction::Move { dx: 10, dy: 10 }, &mut buttons);
    assert_eq!(packets.len(), 1);
    assert_eq!(
        (packets[0].dx, packets[0].dy, packets[0].buttons),
        (10, 10, 0)
    );
}

#[test]
fn mouse_buttons_are_held_across_moves_so_a_drag_is_a_drag() {
    let mut buttons = 0;
    let down = mouse_action_packets(&MouseAction::Button(1), &mut buttons);
    assert_eq!(down.len(), 1);
    assert_eq!(down[0].buttons, 1);
    assert_eq!(buttons, 1, "the press must persist past its own packet");
    // Motion while held carries the button, which is what makes it a drag
    // rather than a press that lets go the moment the pointer moves.
    let packets = mouse_action_packets(&MouseAction::Move { dx: 5, dy: 5 }, &mut buttons);
    assert_eq!(packets.len(), 1);
    assert_eq!(
        (packets[0].dx, packets[0].dy, packets[0].buttons),
        (5, 5, 1)
    );
    let up = mouse_action_packets(&MouseAction::Button(0), &mut buttons);
    assert_eq!(up[0].buttons, 0);
    assert_eq!(buttons, 0);
}

#[test]
fn mouse_home_sends_enough_packets_to_reach_the_corner() {
    // Homing works by overshooting into the driver's own clamp, so the packet
    // count has to cover the tallest space a guest may set up. At TOKAMOUS's
    // default vertical ratio one packet is 127 pixels; a game asking for a
    // ratio four times coarser gets 31, and 12 packets must still clear 480.
    let mut buttons = 0;
    let packets = mouse_action_packets(&MouseAction::Home, &mut buttons);
    let coarsest_pixels_per_packet = 255 * 8 / 64;
    assert!(
        packets.len() as i32 * coarsest_pixels_per_packet >= 480,
        "{} packets cannot cross a 480-pixel screen",
        packets.len()
    );
    for p in &packets {
        assert!(p.dx < 0 && p.dy < 0, "homing must drive toward the origin");
    }
}

#[test]
fn merged_schedule_orders_keys_and_mouse_by_offset() {
    // The two flags are parsed separately but must fire in one time order, or a
    // click scheduled between two keystrokes would arrive after both.
    let merged = merged_injections(Some("100:a;300:b"), Some("200:click")).unwrap();
    let offsets: Vec<u64> = merged.iter().map(|s| s.at_cycles).collect();
    assert_eq!(offsets, vec![100, 200, 300]);
    assert!(matches!(merged[1].event, InjectionEvent::Mouse(_)));
    // Each flag alone still works, and neither flag means no schedule at all.
    assert_eq!(merged_injections(Some("100:a"), None).unwrap().len(), 1);
    assert_eq!(merged_injections(None, Some("100:home")).unwrap().len(), 1);
    assert!(merged_injections(None, None).unwrap().is_empty());
}

#[test]
fn click_holds_the_button_long_enough_for_a_frame_poll() {
    // Regression: press and release one packet apart is 5 ms, and Grand Prix 2's
    // startup menu never saw it -- the pointer was verified to be on the button
    // and the click still did nothing, because the menu samples the button once
    // a frame. The hold has to outlast a frame at any plausible rate.
    let mut buttons = 0;
    let packets = mouse_action_packets(&MouseAction::Click, &mut buttons);
    assert_eq!(packets.len(), 2, "a click is one press and one release");
    assert_eq!(packets[0].buttons, 1);
    assert_eq!(packets[1].buttons, 0);
    let slowest_frame_ms = 1000 / 24;
    assert!(
        packets[0].dwell_ms > slowest_frame_ms,
        "a {} ms hold can fall between two polls",
        packets[0].dwell_ms
    );
    // Nothing repeats during the hold: a real PS/2 mouse sends one packet per
    // state change, and the driver latches it until the release arrives.
    assert_eq!(buttons, 0, "a click must not leave the button held");
}

#[test]
fn held_keys_emit_only_the_edge_they_name() {
    // A bare token is a tap: make then break.
    assert_eq!(
        text_to_scancode_groups("{right}").unwrap(),
        vec![vec![0x4d, 0xcd]]
    );
    // `+` presses and does NOT release, which is the whole point -- the guest
    // tracks key-down state from the scancode stream, so a tap cannot express
    // "keep running". Prince of Persia's prince stands still under a tap and
    // runs under a hold.
    assert_eq!(
        text_to_scancode_groups("{+right}").unwrap(),
        vec![vec![0x4d]]
    );
    // `-` releases.
    assert_eq!(
        text_to_scancode_groups("{-right}").unwrap(),
        vec![vec![0xcd]]
    );
    // The three must be distinguishable, or a schedule asking for a hold would
    // silently get a tap.
    let tap = text_to_scancode_groups("{right}").unwrap();
    let press = text_to_scancode_groups("{+right}").unwrap();
    assert_ne!(tap, press);
    // Modifiers take the prefixes too, and the name still resolves.
    assert_eq!(
        text_to_scancode_groups("{+shift}").unwrap(),
        vec![vec![0x2a]]
    );
    // An unknown name is still an error with the prefix stripped, not a silent
    // fallthrough that would inject nothing.
    assert!(text_to_scancode_groups("{+nosuchkey}").is_err());
}

/// The headless audio capture is an OBSERVER, not a run mode: it has to compose
/// with whatever else is pacing the run.
///
/// `IZARRAVM_AUDIO_WAV` used to win the run if/else outright, so a run that also
/// asked for `--inject-keys`/`--inject-mouse` got a WAV of a title that had never
/// received its input, with no warning. The fix routes every guest advance
/// through `run_sliced`, which is what the injection loop's `advance` now calls.
/// This drives `run_sliced` the way that loop does -- several advances of
/// different, short lengths -- and checks the capture keeps accumulating across
/// all of them and paces its window by the cycles each one actually ran.
#[test]
fn audio_capture_observes_every_advance_and_paces_by_cycles_run() {
    let hardware = HardwareProfile {
        cpu: GswMode::Gsw486,
        memory_mib: 16,
        video: VideoCard::Vega,
        sound_blaster: izarravm_core::SoundBlasterConfig::default(),
        wss: izarravm_core::WssConfig::default(),
    };
    let dir = munt_test_dir("audio-capture-compose");
    let wav = dir.join("capture.wav");
    let clock_hz = hardware.cpu.clock_rate().clocks_for_fraction_floor(1, 1);

    let build = || {
        // `jmp $`: runs for exactly the cycle budget, every time.
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &[0xEB, 0xFE])
            .expect("build raw machine")
    };

    // Ten advances of one guest millisecond each -- shorter than the capture's
    // own 10 ms slice, which is what an injection burst looks like.
    let mut machine = build();
    let mut capture = Some(AudioCapture::new(
        AudioSinkMode::Wav(wav.clone()),
        &hardware,
        AUDIO_CAPTURE_SLICE_MS,
    ));
    let per_advance = clock_hz / 1_000;
    let mut spent = 0u64;
    for _ in 0..10 {
        let (_, ran) = run_sliced(&mut machine, per_advance, &mut capture).unwrap();
        spent += ran;
    }
    assert_eq!(spent, per_advance * 10);
    let frames = capture.as_ref().unwrap().pcm.len();
    // Ten guest milliseconds at the 44.1 kHz DAC rate. A capture that rendered a
    // fixed 10 ms slice's worth per advance would land near ten times this.
    let expected = 44_100 * 10 / 1_000;
    assert!(
        frames.abs_diff(expected) < expected / 4,
        "expected ~{expected} frames for 10 guest ms, got {frames}"
    );
    capture.as_ref().unwrap().finish().unwrap();
    let written = std::fs::read(&wav).unwrap();
    assert_eq!(&written[..4], b"RIFF");
    assert_eq!(written.len(), 44 + frames * 4);

    // With no capture armed the same advance is ONE `run_until_halt_or_cycles`
    // call and renders nothing, so an unobserved run keeps its exact device
    // boundaries.
    let mut bare = build();
    let mut none = None;
    let (_, ran) = run_sliced(&mut bare, per_advance * 10, &mut none).unwrap();
    assert_eq!(ran, per_advance * 10);
    assert!(none.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

/// The `IZARRAVM_AUDIO_WAV` capture must write real stereo.
///
/// This instrument is what a stereo bug gets diagnosed WITH, so a capture that
/// summed or dropped a channel would manufacture the very symptom it is used to
/// investigate. Assert the declared channel count AND that two distinct input
/// channels come back distinct -- a header claiming stereo over duplicated
/// samples would pass the first check alone.
#[test]
fn the_audio_wav_capture_writes_distinct_left_and_right_channels() {
    let dir = std::env::temp_dir().join(format!(
        "izarravm_wav_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("capture.wav");

    // Hard-panned left, then hard-panned right, then a distinct pair.
    let pcm = [(20_000i16, 0i16), (0, -20_000), (1234, -5678)];
    write_wav(&path, &pcm, 44_100).expect("write the capture");
    let bytes = std::fs::read(&path).expect("read the capture back");

    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(
        u16::from_le_bytes([bytes[22], bytes[23]]),
        2,
        "the capture must declare two channels"
    );
    assert_eq!(
        u16::from_le_bytes([bytes[32], bytes[33]]),
        4,
        "block align must be 4 bytes: two 16-bit channels per frame"
    );
    assert_eq!(
        u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]),
        (pcm.len() * 4) as u32,
        "the data chunk must hold both channels of every frame"
    );

    let frames: Vec<(i16, i16)> = bytes[44..]
        .chunks_exact(4)
        .map(|f| {
            (
                i16::from_le_bytes([f[0], f[1]]),
                i16::from_le_bytes([f[2], f[3]]),
            )
        })
        .collect();
    assert_eq!(
        frames, pcm,
        "the capture must round-trip each channel untouched -- no downmix, no \
         channel dropped, no reordering"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The `IZARRAVM_AUDIO_WAV` capture records the MACHINE's output, not the host's
/// speakers, so the GUI's volume knob must never reach it.
///
/// The knob is a host playback level applied after `render_audio`, and it now
/// goes to 5x. If it ever leaked into the capture, a WAV would stop being
/// evidence about the machine: two recordings of the same title would differ by
/// whatever the panel happened to be set to, and one taken with the knob up
/// would show clipping the machine never produced. The capture takes its samples
/// straight from `render_audio` and this is the assertion that says so -- the
/// captured peak is the machine's own peak, unscaled.
///
/// Non-vacuous by construction: the guest drives the PC speaker, so the mix is
/// loud enough that any factor other than 1.0 would move the peak.
#[test]
fn the_audio_wav_capture_records_the_machine_unscaled_by_the_host_volume_knob() {
    // The profile the capture is told about and the profile the machine is
    // actually built with have to be the same CPU, or the capture paces its
    // window off a clock the guest is not running at.
    let hardware = audio_cost_hardware();
    let dir = munt_test_dir("audio-capture-no-host-gain");
    let clock_hz = hardware.cpu.clock_rate().clocks_for_fraction_floor(1, 1);
    let ten_ms = clock_hz / 100;
    let build = audio_beep_machine;
    let peak = |pcm: &[(i16, i16)]| -> i32 {
        pcm.iter()
            .map(|(l, r)| (*l as i32).abs().max((*r as i32).abs()))
            .max()
            .unwrap_or(0)
    };

    // Through the capture, the way a headless run records.
    let mut machine = build();
    let mut capture = Some(AudioCapture::new(
        AudioSinkMode::Wav(dir.join("beep.wav")),
        &hardware,
        AUDIO_CAPTURE_SLICE_MS,
    ));
    for _ in 0..5 {
        run_sliced(&mut machine, ten_ms, &mut capture).unwrap();
    }
    let captured = peak(&capture.as_ref().unwrap().pcm);

    // Straight off the machine, no capture in the path at all.
    let mut reference = build();
    let mut direct: Vec<(i16, i16)> = Vec::new();
    for _ in 0..5 {
        reference.run_until_halt_or_cycles(ten_ms).unwrap();
        direct.extend(reference.render_audio(497));
    }
    let unscaled = peak(&direct);

    assert!(
        unscaled > 0,
        "the beeper must actually reach render_audio, or this test proves nothing"
    );
    assert_eq!(
        captured, unscaled,
        "the capture is the machine's own mix at unity -- no host playback level"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// PIT channel 2 as a ~1 kHz square wave through port 0x61, then spin. Shared by
/// the audio-cost tests: an entirely silent guest still pays full OPL synthesis,
/// so a checksum assertion needs a guest that actually puts something on the
/// line-out or it cannot tell "the mix ran" from "the mix was elided".
const AUDIO_TEST_BEEP: &[u8] = &[
    0xB0, 0xB6, // mov al, 0xB6   -- channel 2, mode 3, lobyte/hibyte
    0xE6, 0x43, // out 0x43, al
    0xB0, 0xA9, // mov al, 0xA9   -- divisor 0x04A9 = 1193
    0xE6, 0x42, // out 0x42, al
    0xB0, 0x04, // mov al, 0x04
    0xE6, 0x42, // out 0x42, al
    0xB0, 0x03, // mov al, 0x03   -- gate + data enable
    0xE6, 0x61, // out 0x61, al
    0xEB, 0xFE, // jmp $
];

fn audio_cost_hardware() -> HardwareProfile {
    HardwareProfile {
        cpu: GswMode::Gsw386,
        memory_mib: 16,
        video: VideoCard::Vega,
        sound_blaster: izarravm_core::SoundBlasterConfig::default(),
        wss: izarravm_core::WssConfig::default(),
    }
}

fn audio_beep_machine() -> Machine {
    Machine::new_raw_program(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        AUDIO_TEST_BEEP,
    )
    .expect("build raw machine")
}

/// Run the beeper for `slices` ten-millisecond advances under one sink mode and
/// hand back the observer and the machine it observed. One `run_sliced` call per
/// advance, i.e. the shape an injection path produces.
fn run_audio_cost_leg(mode: AudioSinkMode, slices: u32) -> (AudioCapture, Machine) {
    let hardware = audio_cost_hardware();
    let ten_ms = hardware.cpu.clock_rate().clocks_for_fraction_floor(1, 100);
    let mut machine = audio_beep_machine();
    let mut capture = Some(AudioCapture::new(mode, &hardware, AUDIO_CAPTURE_SLICE_MS));
    for _ in 0..slices {
        run_sliced(&mut machine, ten_ms, &mut capture).unwrap();
    }
    (capture.unwrap(), machine)
}

/// The same guest work as [`run_audio_cost_leg`], but requested as ONE
/// `run_sliced` call, so the subdivision happens INSIDE the run loop.
fn run_audio_cost_leg_one_call(mode: AudioSinkMode, slices: u32) -> AudioCapture {
    let hardware = audio_cost_hardware();
    let mut machine = audio_beep_machine();
    let mut capture = Some(AudioCapture::new(mode, &hardware, AUDIO_CAPTURE_SLICE_MS));
    let cycles = capture.as_ref().unwrap().slice * u64::from(slices);
    let (_, ran) = run_sliced(&mut machine, cycles, &mut capture).unwrap();
    assert_eq!(ran, cycles, "the guest stopped short of the request");
    capture.unwrap()
}

/// `IZARRAVM_AUDIO_COST` decides which of two legs of a measurement this run is,
/// so a value it does not understand must FAIL rather than fall back.
///
/// The failure mode being closed off is silent and unfalsifiable from the
/// results: a typo that quietly disarmed the observer would produce two identical
/// legs, a wall ratio of 1.0, and the conclusion "audio is free" -- a wrong
/// answer where a crash would have been a missing one. The empty string is the
/// same hazard wearing house colours: in pwsh,
/// `[Environment]::SetEnvironmentVariable(name, $null, "Process")` leaves the
/// variable empty-but-set and children inherit it, so "cleared" and "set to
/// nothing" must not mean different things here.
#[test]
fn audio_cost_mode_parse_is_pinned() {
    assert_eq!(
        parse_audio_cost_mode("count").unwrap(),
        AudioSinkMode::Count
    );
    assert_eq!(
        parse_audio_cost_mode("  COUNT \n").unwrap(),
        AudioSinkMode::Count,
        "a value that arrived with whitespace or in another case is still a mode"
    );
    assert_eq!(parse_audio_cost_mode("off").unwrap(), AudioSinkMode::Skip);
    assert_eq!(
        parse_audio_cost_mode("skip").unwrap(),
        AudioSinkMode::Skip,
        "`skip` names the sink, `off` names the leg; both must reach the control leg"
    );
    for typo in ["on", "1", "true", "counting", "wav", ""] {
        assert!(
            parse_audio_cost_mode(typo).is_err(),
            "{typo:?} must not silently resolve to a mode"
        );
    }
    // The label is what the ladder greps out of the report line.
    assert_eq!(AudioSinkMode::Count.label(), "count");
    assert_eq!(AudioSinkMode::Skip.label(), "off");

    // An empty variable is an UNSET variable, in both directions, or the pwsh
    // clear-by-null trap arms the observer on every row of a scoreboard.
    assert_eq!(resolve_audio_sink_from(None, None).unwrap(), None);
    assert_eq!(
        resolve_audio_sink_from(Some(std::ffi::OsString::new()), Some(String::new())).unwrap(),
        None
    );
    assert_eq!(
        resolve_audio_sink_from(None, Some("count".into())).unwrap(),
        Some(AudioSinkMode::Count)
    );
    assert_eq!(
        resolve_audio_sink_from(Some("out.wav".into()), None).unwrap(),
        Some(AudioSinkMode::Wav(PathBuf::from("out.wav")))
    );
    // Two observers of the same call, and one would silently win.
    assert!(resolve_audio_sink_from(Some("out.wav".into()), Some("count".into())).is_err());

    assert_eq!(parse_audio_cost_slice_ms(None).unwrap(), 10);
    assert_eq!(
        parse_audio_cost_slice_ms(Some(String::new())).unwrap(),
        AUDIO_CAPTURE_SLICE_MS
    );
    assert_eq!(parse_audio_cost_slice_ms(Some("1".into())).unwrap(), 1);
    assert!(parse_audio_cost_slice_ms(Some("0".into())).is_err());
    assert!(parse_audio_cost_slice_ms(Some("ten".into())).is_err());
}

/// The armed leg has to actually mix, has to mix the SAME way twice, and has to
/// leave nothing behind on disk.
///
/// All three are load-bearing for the instrument. A sink cheap enough to be
/// elided would measure nothing; a non-deterministic mix would break the ladder's
/// per-role determinism rule; and a file write would put ~29 MB of buffer growth
/// and a serialize into the very wall being measured -- which is the whole reason
/// the WAV path cannot be used as the cost proxy.
#[test]
fn audio_cost_count_leg_folds_a_deterministic_mix_and_writes_nothing() {
    let dir = munt_test_dir("audio-cost-count");
    let before: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert!(before.is_empty(), "the scratch dir starts empty");

    let (first, _) = run_audio_cost_leg(AudioSinkMode::Count, 5);
    assert!(first.windows > 0, "the observer never ran a window");
    assert!(first.out_frames > 0, "the mix returned no frames");
    assert_ne!(
        first.checksum, 0,
        "the beeper is on the line-out: a zero fold means the mix never happened"
    );
    assert!(
        first.pcm.is_empty(),
        "the counting sink must not retain one frame"
    );

    let (second, _) = run_audio_cost_leg(AudioSinkMode::Count, 5);
    assert_eq!(
        (second.windows, second.native_samples, second.out_frames),
        (first.windows, first.native_samples, first.out_frames),
        "the same guest work must ask for the same windows"
    );
    assert_eq!(
        second.checksum, first.checksum,
        "the mix must be deterministic, or armed observations are not comparable"
    );

    // POSITIVE CONTROL first, or "no file appeared" proves only that this test
    // was looking at a directory nothing writes to. The WAV sink finishing into
    // THIS directory must produce a file here.
    let hardware = audio_cost_hardware();
    let ten_ms = hardware.cpu.clock_rate().clocks_for_fraction_floor(1, 100);
    let mut wav_machine = audio_beep_machine();
    let mut wav_capture = Some(AudioCapture::new(
        AudioSinkMode::Wav(dir.join("control.wav")),
        &hardware,
        AUDIO_CAPTURE_SLICE_MS,
    ));
    for _ in 0..5 {
        run_sliced(&mut wav_machine, ten_ms, &mut wav_capture).unwrap();
    }
    wav_capture.as_ref().unwrap().finish().unwrap();
    let control: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(
        control.len(),
        1,
        "the WAV sink wrote no file, so the assertion below would be vacuous"
    );

    // Now the armed leg into the SAME directory: `finish()` is mode-aware, runs
    // before the wall reading, and the cost modes have no path to write to.
    first.finish().unwrap();
    let after: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(
        after, control,
        "the counting sink added a file to a directory it must not write to"
    );
    let report = first.cost_report();
    assert!(
        report.starts_with("audio cost: mode=count pacing=guest slice_ms=10 windows="),
        "unexpected report line: {report}"
    );
    assert!(report.contains(&format!("out_frames={}", first.out_frames)));
    assert!(report.contains("render_ns="));

    let _ = std::fs::remove_dir_all(&dir);
}

/// The counting sink must see the SAME mix the WAV capture records.
///
/// `Count` is the armed leg of a cost measurement, and the only evidence that it
/// prices the real thing is that the frames it folded are the frames the shipped
/// observer would have written. Same guest, same cadence, two sinks: the frame
/// count has to match and the fold over the WAV's own PCM has to reproduce the
/// counting sink's checksum exactly. A `Count` path that rendered a shorter
/// window, or a different mix, would otherwise be measuring something cheaper
/// than the thing the campaign is deciding about.
#[test]
fn audio_cost_count_leg_folds_exactly_what_the_wav_capture_records() {
    let hardware = audio_cost_hardware();
    let ten_ms = hardware.cpu.clock_rate().clocks_for_fraction_floor(1, 100);
    let dir = munt_test_dir("audio-cost-wav-parity");

    let mut wav_machine = audio_beep_machine();
    let mut wav = Some(AudioCapture::new(
        AudioSinkMode::Wav(dir.join("parity.wav")),
        &hardware,
        AUDIO_CAPTURE_SLICE_MS,
    ));
    for _ in 0..5 {
        run_sliced(&mut wav_machine, ten_ms, &mut wav).unwrap();
    }
    let wav = wav.unwrap();

    let (count, _) = run_audio_cost_leg(AudioSinkMode::Count, 5);

    assert!(!wav.pcm.is_empty(), "the WAV leg captured nothing");
    assert_eq!(
        count.out_frames,
        wav.pcm.len() as u64,
        "the counting sink saw a different number of frames than the WAV capture"
    );
    assert_eq!(
        (count.windows, count.native_samples),
        (wav.windows, wav.native_samples),
        "the two sinks did not ask for the same windows"
    );
    assert_eq!(
        count.checksum,
        fold_audio_frames(0, &wav.pcm),
        "the counting sink folded a different mix than the WAV capture recorded"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The observer has to count windows the same way when the subdivision happens
/// INSIDE one `run_sliced` call, which is the shape every fixture run takes.
///
/// The per-advance path is exercised elsewhere; this is the other one. A window
/// count that came out right only when the caller happened to hand over exactly
/// one slice at a time would make every fixture observation's `windows` -- and
/// therefore the cross-role window equality the ladder asserts -- a coincidence
/// of the harness rather than a property of the observer.
#[test]
fn audio_cost_counts_one_window_per_slice_inside_a_single_advance() {
    let armed = run_audio_cost_leg_one_call(AudioSinkMode::Count, 5);
    let control = run_audio_cost_leg_one_call(AudioSinkMode::Skip, 5);

    assert_eq!(armed.windows, 5, "five slices must render five windows");
    assert_eq!(
        control.windows, 5,
        "the control leg must subdivide identically, or the pair measures slicing"
    );
    assert_eq!(armed.native_samples, control.native_samples);
    assert!(armed.out_frames > 0);
    assert_eq!(control.out_frames, 0);
}

/// The disarmed leg is a CONTROL, not an absence: it slices the run at the same
/// cadence and computes the same window, and only skips `render_audio`.
///
/// If it were an ordinary unsliced run, the pair would measure SLICING -- arming
/// the observer moves where device events are serviced -- and the audio number
/// would be whatever that confound happened to be. The equalities below are what
/// prove the two legs simulated the same guest: `instructions` and
/// `elapsed_clocks` are exact, not toleranced, because both legs subdivide
/// identically. Any drift means the observer reached the guest.
#[test]
fn audio_cost_skip_leg_slices_identically_and_renders_nothing() {
    let (count, count_machine) = run_audio_cost_leg(AudioSinkMode::Count, 5);
    let (skip, skip_machine) = run_audio_cost_leg(AudioSinkMode::Skip, 5);

    assert_eq!(
        (skip.windows, skip.native_samples),
        (count.windows, count.native_samples),
        "the control leg must compute the identical window it declines to render"
    );
    assert_eq!(skip.out_frames, 0, "the control leg must not render");
    assert_eq!(skip.checksum, 0);
    assert_eq!(
        skip.render_ns, 0,
        "no render happened, so no render time can have been spent"
    );
    assert!(skip.pcm.is_empty());

    assert_eq!(
        skip_machine.cpu().perf_counters().instructions,
        count_machine.cpu().perf_counters().instructions,
        "guest-visible work differs between the legs -- the observer reached the guest"
    );
    assert_eq!(
        skip_machine.elapsed_clocks(),
        count_machine.elapsed_clocks(),
        "the two legs did not simulate the same amount of guest time"
    );

    assert!(
        skip.cost_report()
            .starts_with("audio cost: mode=off pacing=guest slice_ms=10 windows="),
        "unexpected report line: {}",
        skip.cost_report()
    );
    assert!(
        skip.cost_report()
            .contains("out_frames=0 checksum=0x0000000000000000")
    );
}

/// Every counter the open-area profiling protocol correlates against must actually reach the
/// series JSON. `PhaseMark.perf` carries the whole `PerfCounters` struct, so the failure mode
/// this guards is silent: a column that is simply absent reads, downstream, as a counter that
/// did not move. Each value below is distinct so a copy-paste between two keys fails here.
#[test]
fn phase_mark_series_carries_the_break_and_smc_scan_columns() {
    let perf = izarravm_cpu::PerfCounters {
        straight_line_runs: 900,
        brk_decode_or_branch: 1,
        brk_cont_decode_miss: 2,
        brk_cont_not_continuable: 3,
        brk_cont_page_cross: 4,
        brk_step: 5,
        brk_interrupt: 6,
        brk_cap: 7,
        brk_halt: 8,
        smc_scan_calls: 9,
        smc_scan_keys: 10,
        smc_heat_chunks_hot: 11,
        jit_direct_side_exits: 12,
        jit_direct_unresolved_exits: 13,
        jit_direct_unresolved_static_unbound: 14,
        jit_direct_unresolved_static_hidden: 15,
        jit_direct_unresolved_dynamic_miss_or_unbound: 16,
        jit_direct_unresolved_dynamic_hidden: 17,
        direct_page_hits: 18,
        direct_page_misses: 19,
        jit_direct_arena_compactions: 20,
        jit_direct_arena_compaction_ns: 21,
        ..Default::default()
    };

    let mark = izarravm_machine::PhaseMark {
        id: izarravm_machine::phase_mark::PERIODIC,
        wall: std::time::Instant::now(),
        master_ticks: 0,
        elapsed_clocks: 0,
        perf,
        machine_phases: Default::default(),
        katea: None,
        io_stall_ticks: 0,
        halted_ticks: 0,
        int13: Default::default(),
        fast_map_audit: Default::default(),
        cpu_profile: None,
    };

    let rows = phase_mark_series_json(std::slice::from_ref(&mark));
    let row = &rows[0];
    for (key, expected) in [
        ("straight_line_runs", 900),
        ("brk_decode_or_branch", 1),
        ("brk_cont_decode_miss", 2),
        ("brk_cont_not_continuable", 3),
        ("brk_cont_page_cross", 4),
        ("brk_step", 5),
        ("brk_interrupt", 6),
        ("brk_cap", 7),
        ("brk_halt", 8),
        ("smc_scan_calls", 9),
        ("smc_scan_keys", 10),
        ("smc_heat_chunks_hot", 11),
        ("jit_direct_side_exits", 12),
        ("jit_direct_unresolved_exits", 13),
        ("jit_direct_unresolved_static_unbound", 14),
        ("jit_direct_unresolved_static_hidden", 15),
        ("jit_direct_unresolved_dynamic_miss_or_unbound", 16),
        ("jit_direct_unresolved_dynamic_hidden", 17),
        ("direct_page_hits", 18),
        ("direct_page_misses", 19),
        ("jit_direct_arena_compactions", 20),
        ("jit_direct_arena_compaction_ns", 21),
    ] {
        assert_eq!(
            row.get(key).and_then(|v| v.as_u64()),
            Some(expected),
            "{key} must be serialised into the phase-mark series"
        );
    }
}
