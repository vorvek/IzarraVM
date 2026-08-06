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
        last_read_insn: 0,
        last_read_page: u32::MAX,
    };

    let report = bench::perf_counters_json(
        &perf,
        izarravm_cpu::PollSkipMemoryCounters::default(),
        fast_map_probe,
        fast_map_audit,
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
        last_read_insn: _,
        last_read_page: _,
    } = izarravm_cpu::FastMapAuditCounters::default();
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
