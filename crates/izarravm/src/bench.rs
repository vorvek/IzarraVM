// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// One bench run result: raw guest clocks, the reported iteration count and
/// self-check value, and the host wall time.
struct BenchRun {
    clocks: u64,
    iterations: u32,
    aux: u32,
    wall: std::time::Duration,
    /// Host-side perf counters for this run (decode-cache + straight-line diagnostics).
    perf: PerfCounters,
    /// Memory-poll subset, stored outside PerfCounters on the CPU (layout
    /// preservation; see PollSkipMemoryCounters) and captured here alongside.
    poll_skip_memory: izarravm_cpu::PollSkipMemoryCounters,
    /// Clif churn subset (C1e), captured alongside for the same layout-preservation
    /// reason.
    jit_clif: izarravm_cpu::JitClifCounters,
    machine_profile: MachineHostProfileSnapshot,
    cpu_profile: CpuProfileSnapshot,
}

/// How a benchmark payload is loaded: baked into the Neurketa boot image and
/// chosen by a selector byte, or a local DOS executable under `.bench`.
#[derive(Debug)]
enum BenchSource {
    BootSelector(u8),
    LocalDosExe(&'static str),
    DosExe(Vec<u8>),
}

fn run_bench_one(
    hardware: &HardwareProfile,
    mode: GswMode,
    source: &BenchSource,
    budget: u64,
) -> Result<BenchRun, Box<dyn Error>> {
    // Calibration census tool (mirrors run_boot_hdd_folder):
    // IZARRAVM_CPU_PROFILE=<stride> samples the per-opcode CPU profile for
    // every bench run; the caller prints it when the env is set.
    let stride = std::env::var("IZARRAVM_CPU_PROFILE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    run_bench_one_profiled(hardware, mode, source, budget, stride)
}

fn run_bench_one_profiled(
    hardware: &HardwareProfile,
    mode: GswMode,
    source: &BenchSource,
    budget: u64,
    sample_stride: Option<u64>,
) -> Result<BenchRun, Box<dyn Error>> {
    let profile = MachineProfile::from_hardware_profile(hardware);
    let mut machine = match source {
        BenchSource::BootSelector(selector) => {
            let mut m = Machine::new_boot_image(profile, neurketa_image())?;
            m.set_bench_selector(*selector);
            m
        }
        BenchSource::LocalDosExe(path) => {
            let exe = std::fs::read(path).map_err(|error| {
                format!("cannot load local benchmark {path}: {error}; place the licensed DOS executable in .bench")
            })?;
            Machine::new_raw_program(profile, &exe)?
        }
        BenchSource::DosExe(exe) => Machine::new_raw_program(profile, exe)?,
    };
    machine.set_mode(mode);
    if let Some(sample_stride) = sample_stride {
        machine.enable_host_profiling(sample_stride);
    }
    let started = std::time::Instant::now();
    let stop = machine.run_until_halt_or_cycles(budget)?;
    let wall = started.elapsed();
    if !matches!(stop, StopReason::TestExit { .. }) {
        return Err(format!(
            "bench {} {source:?} did not exit cleanly: {stop:?}",
            mode.canonical_name()
        )
        .into());
    }
    let mut perf = machine.cpu().perf_counters().clone();
    perf.cache_tier_lookups = machine.cache_tier_lookups();
    Ok(BenchRun {
        clocks: machine.elapsed_clocks(),
        iterations: machine.bench_iterations(),
        aux: machine.bench_aux(),
        wall,
        perf,
        poll_skip_memory: machine.cpu().poll_skip_memory(),
        jit_clif: machine.cpu().jit_clif_counters(),
        machine_profile: machine.host_profile_snapshot(),
        cpu_profile: machine.cpu().profile_snapshot(),
    })
}

/// A benchmark payload the harness can run: a display name, how the payload is
/// loaded, and the lowest GSW mode it applies to. FP payloads need an FPU, so
/// they start at 486.
struct Bench {
    name: &'static str,
    source: BenchSource,
    min_mode: GswMode,
    /// Floating-point operations per reported iteration. When set, the harness
    /// reports `MFLOPS = iters_per_sec * flops_per_iter / 1e6` and bands against it
    /// (Whetstone, the FP oracle). `None` benches band against raw `iters/sec`.
    flops_per_iter: Option<f64>,
}

const BENCHES: &[Bench] = &[
    Bench {
        name: "sieve",
        source: BenchSource::BootSelector(1),
        min_mode: GswMode::Gsw386Slow,
        flops_per_iter: None,
    },
    Bench {
        name: "fp-mandel",
        source: BenchSource::BootSelector(3),
        min_mode: GswMode::Gsw486,
        flops_per_iter: None,
    },
    Bench {
        name: "dhrystone",
        source: BenchSource::LocalDosExe(".bench/dhrystone.exe"),
        min_mode: GswMode::Gsw386Slow,
        flops_per_iter: None,
    },
    // Whetstone: the FP oracle (486+). `flops_per_iter` is the per-sweep FLOP weight,
    // anchored so the era-calibrated 486 lands at ~6.5 MFLOPS (Roy Longbottom); the
    // 586 is then tuned to ~34.5 MFLOPS via fp_timing(I586).
    Bench {
        name: "whetstone",
        source: BenchSource::LocalDosExe(".bench/whetstone.exe"),
        min_mode: GswMode::Gsw486,
        flops_per_iter: Some(WHETSTONE_FLOPS_PER_SWEEP),
    },
];

/// FLOP weight per Whetstone sweep (the value reported as one iteration). Anchored
/// to the era 486DX2-66 Whetstone figure (~6.5 MFLOPS): the measured 486 throughput
/// (250.0 sweeps/sec, era-calibrated 486 timing) times this over 1e6 == 6.5. A pure
/// units constant; the physical 586/486 ratio lives in fp_timing(I586).
const WHETSTONE_FLOPS_PER_SWEEP: f64 = 26000.0;

/// Small real-mode polling and memory loops used for host-performance diagnosis.
/// They run from a bare boot ROM for a fixed guest-time budget and never affect
/// the reference-band result.
struct Microbench {
    name: &'static str,
    code: &'static [u8],
}

// Small enough that the slowest pattern (poll-3da, which pays a full batch
// epilogue per iteration) still finishes in a couple of seconds at
// 486/586, large enough for a stable rt_factor (thousands of guest batches
// even in the worst case).
const MICROBENCH_BUDGET: u64 = 20_000_000;

const MICROBENCHES: &[Microbench] = &[
    Microbench {
        name: "poll-3da",
        // mov dx, 0x3DA
        // wait: in al, dx
        //       test al, 0x08
        //       jz wait          (spin while the vretrace bit is clear)
        //       jmp wait         (re-poll once seen set, an unconditional spin)
        // Tight polling makes the per-port-read host cost visible.
        code: &[
            0xBA, 0xDA, 0x03, // mov dx, 0x03DA
            0xEC, // wait: in al, dx
            0xA8, 0x08, // test al, 0x08
            0x74, 0xFB, // jz wait
            0xEB, 0xF9, // jmp wait
        ],
    },
    Microbench {
        name: "poll-61",
        // wait: in al, 0x61
        //       test al, 0x10
        //       jz wait          (spin while the DRAM-refresh heartbeat bit is clear)
        //       jmp wait         (re-poll once seen set, an unconditional spin)
        // Mirrors poll-3da's shape against the PIT channel 1 refresh heartbeat.
        code: &[
            0xE4, 0x61, // wait: in al, 0x61
            0xA8, 0x10, // test al, 0x10
            0x74, 0xFA, // jz wait (rel8 -6: wait=0, jz ends at 6)
            0xEB, 0xF8, // jmp wait (rel8 -8: wait=0, jmp ends at 8)
        ],
    },
    Microbench {
        name: "adlib-detect",
        // The canonical AdLib detection idiom (Ralf Brown's probe, mirrored
        // from OplChip's own `adlib_detection_sequence_reports_present` unit
        // test): reset both timers, arm timer 1 to overflow in one 80us step,
        // start it, then poll the status port for the timer-1 flag. The setup
        // writes once and the hot loop contains only status reads.
        //
        // Byte layout (hand-checked, see the poll-61 lesson: a IN AL,imm8's
        // 2-byte encoding vs IN AL,DX's 1-byte moved the branch target and
        // silently turned that fixture into a port-free loop for one commit):
        //   0  BA 88 03        mov dx, 0x0388
        //   3  B0 04           mov al, 0x04
        //   5  EE              out dx, al         ; latch reg 4
        //   6  42              inc dx             ; dx = 0x0389
        //   7  B0 60           mov al, 0x60
        //   9  EE              out dx, al         ; mask both timers
        //  10  4A              dec dx             ; dx = 0x0388
        //  11  B0 04           mov al, 0x04
        //  13  EE              out dx, al         ; latch reg 4
        //  14  42              inc dx             ; dx = 0x0389
        //  15  B0 80           mov al, 0x80
        //  17  EE              out dx, al         ; reset IRQ flags
        //  18  4A              dec dx             ; dx = 0x0388
        //  19  B0 02           mov al, 0x02
        //  21  EE              out dx, al         ; latch reg 2 (timer1 preset)
        //  22  42              inc dx             ; dx = 0x0389
        //  23  B0 FF           mov al, 0xff
        //  25  EE              out dx, al         ; preset 0xff: overflow in 1 step
        //  26  4A              dec dx             ; dx = 0x0388
        //  27  B0 04           mov al, 0x04
        //  29  EE              out dx, al         ; latch reg 4
        //  30  42              inc dx             ; dx = 0x0389
        //  31  B0 21           mov al, 0x21
        //  33  EE              out dx, al         ; start timer1, mask timer2
        //  34  4A              dec dx             ; dx = 0x0388 (status port)
        //  35  EC        wait: in al, dx
        //  36  A8 40           test al, 0x40      ; timer-1 flag
        //  38  74 FB           jz wait            ; rel8 = 35 - 40 = -5 (0xFB)
        //  40  EB F9           jmp wait           ; rel8 = 35 - 42 = -7 (0xF9)
        code: &[
            0xBA, 0x88, 0x03, // mov dx, 0x0388
            0xB0, 0x04, 0xEE, // mov al, 0x04 ; out dx, al
            0x42, 0xB0, 0x60, 0xEE, // inc dx ; mov al, 0x60 ; out dx, al
            0x4A, 0xB0, 0x04, 0xEE, // dec dx ; mov al, 0x04 ; out dx, al
            0x42, 0xB0, 0x80, 0xEE, // inc dx ; mov al, 0x80 ; out dx, al
            0x4A, 0xB0, 0x02, 0xEE, // dec dx ; mov al, 0x02 ; out dx, al
            0x42, 0xB0, 0xFF, 0xEE, // inc dx ; mov al, 0xff ; out dx, al
            0x4A, 0xB0, 0x04, 0xEE, // dec dx ; mov al, 0x04 ; out dx, al
            0x42, 0xB0, 0x21, 0xEE, // inc dx ; mov al, 0x21 ; out dx, al
            0x4A, // dec dx
            0xEC, // wait: in al, dx
            0xA8, 0x40, // test al, 0x40
            0x74, 0xFB, // jz wait
            0xEB, 0xF9, // jmp wait
        ],
    },
    Microbench {
        name: "vram-write",
        // mov ax, 0x0013 ; int 0x10          (mode 13h: 0xA0000 is the LFB)
        // mov ax, 0xA000 ; mov es, ax
        // xor di, di
        // write: mov [es:di], al ; inc di ; jmp write
        // No port I/O at all: an isolated memory-bound hot loop, the
        // counterpart the plan's decision 4 step 7 checks stays UNCHANGED by
        // any later port-laziness slice (it never touches a lazy port).
        code: &[
            0xB8, 0x13, 0x00, // mov ax, 0x0013
            0xCD, 0x10, // int 0x10
            0xB8, 0x00, 0xA0, // mov ax, 0xA000
            0x8E, 0xC0, // mov es, ax
            0x31, 0xFF, // xor di, di
            0x26, 0x88, 0x05, // write: mov [es:di], al
            0x47, // inc di
            0xEB, 0xFA, // jmp write
        ],
    },
    Microbench {
        name: "register-only",
        // xor ax, ax ; loop: inc ax ; jmp loop. Pure ALU + branch, the
        // compute-only floor: no memory or port access whatsoever.
        code: &[
            0x31, 0xC0, // xor ax, ax
            0x40, // loop: inc ax
            0xEB, 0xFD, // jmp loop
        ],
    },
];

/// Wrap `code` as a bare boot ROM: the payload at offset 0 (physical 0xF0000,
/// where CS:IP F000:0000 lands after reset), an IRET stub at 0xF000 (some
/// BIOS-intercepted service vectors return through it), and a far jump at the
/// reset vector (0xFFF0) into the payload. Mirrors
/// `paced_wall_topup_lets_a_polling_guest_catch_vretrace_windows`'s
/// `rom_with_code` test helper (izarravm-machine/src/lib.rs).
fn microbench_rom(code: &[u8]) -> Vec<u8> {
    let mut rom = vec![0u8; izarravm_machine::BIOS_ROM_SIZE];
    rom[..code.len()].copy_from_slice(code);
    rom[0xF000] = 0xCF; // IRET
    rom[0xfff0..0xfff5].copy_from_slice(&[0xea, 0x00, 0x00, 0x00, 0xf0]); // jmp F000:0000
    rom
}

/// Run every diagnostic loop in every GSW mode. These host-speed rows remain
/// informational because they have no period-hardware reference target.
fn run_microbench(hardware: &HardwareProfile) -> Result<(), Box<dyn Error>> {
    println!();
    println!("=== poll-loop microbench (host diagnostics) ===");
    println!(
        "{:<14} {:<5} {:>10} {:>9} {:>10}",
        "pattern", "mode", "guest_ms", "wall_ms", "rt_factor"
    );
    let modes = [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ];
    for bench in MICROBENCHES {
        for mode in modes {
            let profile = MachineProfile::from_hardware_profile(hardware);
            let mut machine = Machine::new(profile, microbench_rom(bench.code))?;
            machine.set_mode(mode);
            let started = std::time::Instant::now();
            let stop = machine.run_until_halt_or_cycles(MICROBENCH_BUDGET)?;
            let wall = started.elapsed();
            let guest_secs = mode
                .clock_rate()
                .seconds_for_clocks(machine.elapsed_clocks());
            let wall_secs = wall.as_secs_f64();
            let rt = if wall_secs > 0.0 {
                guest_secs / wall_secs
            } else {
                0.0
            };
            // The payloads never halt or exit by design, so the only clean stop
            // is the clock budget running out. Anything else (an early HLT, a
            // fault, a stray DOS/test exit) means the row measured a truncated
            // run and its rt_factor is skewed: mark it visibly so a permanent
            // fixture can never silently report corrupted numbers.
            let early_stop = match stop {
                StopReason::CycleLimit { .. } => String::new(),
                other => format!(" [early-stop: {other:?}]"),
            };
            println!(
                "{:<14} {:<5} {:>10.3} {:>9.3} {:>10.3} [info]{}{}",
                bench.name,
                mode.canonical_name(),
                guest_secs * 1000.0,
                wall_secs * 1000.0,
                rt,
                band_tag(bench.name, mode, rt),
                early_stop,
            );
        }
    }
    Ok(())
}

/// Rank the modes from slowest to fastest so a benchmark's min_mode gates which
/// modes it runs in.
fn mode_rank(mode: GswMode) -> u8 {
    match mode {
        GswMode::Gsw386Slow => 0,
        GswMode::Gsw386 => 1,
        GswMode::Gsw486 => 2,
        GswMode::Gsw586 => 3,
    }
}

/// Run every benchmark in each CPU mode it applies to, printing one labeled row
/// per benchmark per mode. The per-mode baseline (boot and report overhead) is
/// measured once per mode and subtracted from BootSelector payloads. DosExe
/// payloads have their own startup and report the full elapsed clocks.
pub(super) fn run_bench(hardware: &HardwareProfile) -> Result<(), Box<dyn Error>> {
    // The run stops at the guest's CMD_EXIT, so this is only a safety cap.
    const BENCH_BUDGET: u64 = 50_000_000_000;

    let modes = [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ];

    // Per-mode baseline clocks for BootSelector benches, indexed by mode_rank.
    let mut baseline = [0u64; 4];
    for mode in modes {
        baseline[mode_rank(mode) as usize] =
            run_bench_one(hardware, mode, &BenchSource::BootSelector(0), BENCH_BUDGET)?.clocks;
    }

    println!(
        "{:<10} {:<5} {:>12} {:>8} {:>9} {:>12} {:>10} {:>9} {:>10}",
        "bench",
        "mode",
        "cyc/iter",
        "iters",
        "aux",
        "iters/sec",
        "guest_ms",
        "wall_ms",
        "rt_factor"
    );
    // Collected for the host-side perf summary printed after the table.
    let mut perf_rows: Vec<(&'static str, GswMode, PerfCounters)> = Vec::new();
    let mut out_of_band = false;
    for bench in BENCHES {
        let mut slow_row: Option<(u64, f64)> = None;
        for mode in modes {
            if mode_rank(mode) < mode_rank(bench.min_mode) {
                continue;
            }
            let run = run_bench_one(hardware, mode, &bench.source, BENCH_BUDGET)?;
            if std::env::var("IZARRAVM_CPU_PROFILE")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .is_some()
            {
                println!("--- census {} {} ---", bench.name, mode.canonical_name());
                print_cpu_profile(&run.cpu_profile);
            }
            perf_rows.push((bench.name, mode, run.perf.clone()));
            let baseline_clocks = match &bench.source {
                BenchSource::BootSelector(_) => baseline[mode_rank(mode) as usize],
                BenchSource::LocalDosExe(_) | BenchSource::DosExe(_) => 0,
            };
            let work = run.clocks.saturating_sub(baseline_clocks);
            let iters = u64::from(run.iterations.max(1));
            let cyc_per_iter = work as f64 / iters as f64;
            let guest_secs = mode.clock_rate().seconds_for_clocks(work);
            let iters_per_sec = if guest_secs > 0.0 {
                iters as f64 / guest_secs
            } else {
                0.0
            };
            let wall_secs = run.wall.as_secs_f64();
            let rt = if wall_secs > 0.0 {
                guest_secs / wall_secs
            } else {
                0.0
            };
            // For an FP bench (flops_per_iter set) the metric of record is MFLOPS;
            // it is what we print and band against. Other benches band on iters/sec.
            let (band_value, mflops_suffix) = match bench.flops_per_iter {
                Some(w) => {
                    let mflops = iters_per_sec * w / 1e6;
                    (mflops, format!(" mflops={mflops:.2}"))
                }
                None => (iters_per_sec, String::new()),
            };
            match mode {
                GswMode::Gsw386Slow => slow_row = Some((work, band_value)),
                GswMode::Gsw386 => {
                    if let Some((slow_work, slow_value)) = slow_row {
                        if slow_work != work {
                            return Err(format!(
                                "{} retired different architectural work in 386-slow ({slow_work}) and 386 ({work})",
                                bench.name
                            )
                            .into());
                        }
                        let ratio = slow_value * 3.0 / band_value;
                        if (ratio - 1.0).abs() > 0.005 {
                            return Err(format!(
                                "{} 386-slow throughput is {:.4} of the exact one-third target",
                                bench.name, ratio
                            )
                            .into());
                        }
                    }
                }
                GswMode::Gsw486 | GswMode::Gsw586 => {}
            }
            print!(
                "{:<10} {:<5} {:>12.2} {:>8} {:>9} {:>12.1} {:>10.3} {:>9.3} {:>10.3}",
                bench.name,
                mode.canonical_name(),
                cyc_per_iter,
                run.iterations,
                run.aux,
                iters_per_sec,
                guest_secs * 1000.0,
                wall_secs * 1000.0,
                rt,
            );
            if bench_reference::band_for(bench.name, mode).is_some_and(|band| {
                band.verdict(band_value) != bench_reference::BandVerdict::InBand
            }) {
                out_of_band = true;
            }
            println!(
                "{}{}",
                mflops_suffix,
                band_tag(bench.name, mode, band_value)
            );
        }
    }
    run_microbench(hardware)?;
    // Host-side perf summary (RPCS3 idea #1): decode-cache hit rate, average
    // straight-line run length, and why each run ended. Diagnostics only; lines are
    // prefixed "perf" so they never parse as a bench row. The counters are host-side
    // and do not affect cyc/iter (the guest clock metric).
    println!();
    println!("=== perf counters (host-side diagnostics; off the guest-timing path) ===");
    for (name, mode, perf) in &perf_rows {
        let instructions = perf.instructions.max(1);
        let decode_hit = 100.0 * (1.0 - perf.decode_misses as f64 / instructions as f64);
        let insns_per_run = perf.instructions as f64 / perf.straight_line_runs.max(1) as f64;
        println!(
            "perf  {:<10} {:<5} instr={:>13}  decode_hit={:>6.2}%  insns/run={:>9.1}  \
             brk[branch/step/int/cap/halt]={}/{}/{}/{}/{}  \
             data[rd d/s wr d/s]={}/{}/{}/{}  ptr[rd/wr]={}/{}  \
             page[h/m]={}/{}  fetch_page[h/m slow_refill]={}/{}/{}  \
             map_inv={}  rep[fast/all]={}/{}  flags_mat={}  cache_lookups={}  \
             jit[entries/insns/native/helper]={}/{}/{}/{}  \
             jit_mem[load/store/tlb/helper]={}/{}/{}/{}  jit_time[ns/samples]={}/{}",
            name,
            mode.canonical_name(),
            perf.instructions,
            decode_hit,
            insns_per_run,
            perf.brk_decode_or_branch,
            perf.brk_step,
            perf.brk_interrupt,
            perf.brk_cap,
            perf.brk_halt,
            perf.data_direct_reads,
            perf.data_slow_reads,
            perf.data_direct_writes,
            perf.data_slow_writes,
            perf.direct_data_pointer_reads,
            perf.direct_data_pointer_writes,
            perf.direct_page_hits,
            perf.direct_page_misses,
            perf.fetch_page_hits,
            perf.fetch_page_misses,
            perf.slow_prefetch_refills,
            perf.direct_map_invalidations,
            perf.rep_string_fast_iterations,
            perf.rep_string_iterations,
            perf.flag_materializations,
            perf.cache_tier_lookups,
            perf.jit_region_entries,
            perf.jit_region_insns,
            perf.jit_native_insns,
            perf.jit_helper_exits,
            perf.jit_native_load_hits,
            perf.jit_native_store_hits,
            perf.jit_paged_tlb_successes,
            perf.jit_native_memory_helpers,
            perf.jit_native_block_ns,
            perf.jit_native_block_samples,
        );
    }
    if out_of_band {
        return Err("a CPU benchmark row is outside its hard reference band"
            .to_string()
            .into());
    }
    Ok(())
}

pub(super) fn run_bench_exe(path: &Path, hardware: &HardwareProfile) -> Result<(), Box<dyn Error>> {
    const BENCH_BUDGET: u64 = 50_000_000_000;
    let exe = std::fs::read(path)?;
    let mode = GswMode::Gsw586;
    let run = run_bench_one(hardware, mode, &BenchSource::DosExe(exe), BENCH_BUDGET)?;
    let iters = u64::from(run.iterations.max(1));
    let cyc_per_iter = run.clocks as f64 / iters as f64;
    let guest_secs = mode.clock_rate().seconds_for_clocks(run.clocks);
    let iters_per_sec = if guest_secs > 0.0 {
        iters as f64 / guest_secs
    } else {
        0.0
    };
    let wall_secs = run.wall.as_secs_f64();
    let rt = if wall_secs > 0.0 {
        guest_secs / wall_secs
    } else {
        0.0
    };
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("exe");
    println!(
        "{:<10} {:<5} {:>12} {:>8} {:>9} {:>12} {:>10} {:>9} {:>10}",
        "bench",
        "mode",
        "cyc/iter",
        "iters",
        "aux",
        "iters/sec",
        "guest_ms",
        "wall_ms",
        "rt_factor"
    );
    println!(
        "{:<10} {:<5} {:>12.2} {:>8} {:>9} {:>12.1} {:>10.3} {:>9.3} {:>10.3}",
        name,
        mode.canonical_name(),
        cyc_per_iter,
        run.iterations,
        run.aux,
        iters_per_sec,
        guest_secs * 1000.0,
        wall_secs * 1000.0,
        rt,
    );
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct BenchMetrics {
    cycles_per_iter: f64,
    guest_ms: f64,
    wall_ms: f64,
    rt_factor: f64,
    iters_per_sec: f64,
}

fn bench_metrics(run: &BenchRun, mode: GswMode) -> BenchMetrics {
    let iters = u64::from(run.iterations.max(1));
    let cycles_per_iter = run.clocks as f64 / iters as f64;
    let guest_secs = mode.clock_rate().seconds_for_clocks(run.clocks);
    let iters_per_sec = if guest_secs > 0.0 {
        iters as f64 / guest_secs
    } else {
        0.0
    };
    let wall_secs = run.wall.as_secs_f64();
    let rt_factor = if wall_secs > 0.0 {
        guest_secs / wall_secs
    } else {
        0.0
    };
    BenchMetrics {
        cycles_per_iter,
        guest_ms: guest_secs * 1000.0,
        wall_ms: wall_secs * 1000.0,
        rt_factor,
        iters_per_sec,
    }
}

fn print_single_bench_row(name: &str, mode: GswMode, run: &BenchRun) {
    let metrics = bench_metrics(run, mode);
    println!(
        "{:<10} {:<5} {:>12.2} {:>8} {:>9} {:>12.1} {:>10.3} {:>9.3} {:>10.3}",
        name,
        mode.canonical_name(),
        metrics.cycles_per_iter,
        run.iterations,
        run.aux,
        metrics.iters_per_sec,
        metrics.guest_ms,
        metrics.wall_ms,
        metrics.rt_factor,
    );
}

pub(super) fn run_profile_exe(
    path: &Path,
    json_path: Option<&Path>,
    sample_stride: u64,
    hardware: &HardwareProfile,
) -> Result<(), Box<dyn Error>> {
    const BENCH_BUDGET: u64 = 50_000_000_000;
    let exe = std::fs::read(path)?;
    let mode = GswMode::Gsw586;
    let source = BenchSource::DosExe(exe);
    let baseline = run_bench_one_profiled(hardware, mode, &source, BENCH_BUDGET, None)?;
    let profiled =
        run_bench_one_profiled(hardware, mode, &source, BENCH_BUDGET, Some(sample_stride))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("exe");

    println!("# baseline");
    println!(
        "{:<10} {:<5} {:>12} {:>8} {:>9} {:>12} {:>10} {:>9} {:>10}",
        "bench",
        "mode",
        "cyc/iter",
        "iters",
        "aux",
        "iters/sec",
        "guest_ms",
        "wall_ms",
        "rt_factor"
    );
    print_single_bench_row(name, mode, &baseline);

    let baseline_metrics = bench_metrics(&baseline, mode);
    println!();
    print_machine_profile(&profiled.machine_profile, profiled.wall);
    println!();
    print_cpu_profile(&profiled.cpu_profile);
    println!();
    println!("=== perf counters ===");
    print_perf_counter_row("profile", mode, &profiled.perf);

    if let Some(json_path) = json_path {
        write_profile_json(
            path,
            json_path,
            sample_stride,
            &baseline,
            &profiled,
            baseline_metrics,
        )?;
    }
    Ok(())
}

pub(super) fn print_machine_profile(
    snapshot: &MachineHostProfileSnapshot,
    total_wall: std::time::Duration,
) {
    let mut phases = snapshot.phases.clone();
    phases.sort_by_key(|phase| Reverse(phase.wall_ns));
    let classified_ns = phases.iter().map(|phase| phase.wall_ns).sum::<u64>();
    let total_ns = total_wall.as_nanos().min(u128::from(u64::MAX)).max(1) as u64;
    println!("=== machine phases ===");
    println!(
        "{:<20} {:>12} {:>10} {:>8}",
        "phase", "wall_ms", "count", "share"
    );
    for phase in phases
        .iter()
        .filter(|phase| phase.count > 0 || phase.wall_ns > 0)
    {
        println!(
            "{:<20} {:>12.3} {:>10} {:>7.2}%",
            phase.name,
            phase.wall_ns as f64 / 1_000_000.0,
            phase.count,
            100.0 * phase.wall_ns as f64 / total_ns as f64,
        );
    }
    let unattributed_ns = total_ns.saturating_sub(classified_ns);
    println!(
        "{:<20} {:>12.3} {:>10} {:>7.2}%",
        "unattributed",
        unattributed_ns as f64 / 1_000_000.0,
        1,
        100.0 * unattributed_ns as f64 / total_ns as f64,
    );
}

pub(super) fn print_cpu_profile(snapshot: &CpuProfileSnapshot) {
    let mut groups = snapshot.groups.clone();
    groups.sort_by_key(|group| Reverse(group.sample_wall_ns));
    let total_instructions = groups
        .iter()
        .map(|group| group.instructions)
        .sum::<u64>()
        .max(1);
    let total_guest = groups
        .iter()
        .map(|group| group.guest_core_clocks)
        .sum::<u64>()
        .max(1);
    let total_sample = groups
        .iter()
        .map(|group| group.sample_wall_ns)
        .sum::<u64>()
        .max(1);
    println!(
        "=== cpu groups (sample_stride={}) ===",
        snapshot.sample_stride
    );
    println!(
        "{:<18} {:>13} {:>8} {:>13} {:>8} {:>12} {:>8} {:>9}",
        "group", "instr", "instr%", "guest_clk", "guest%", "sample_ms", "sample%", "samples"
    );
    for group in groups
        .iter()
        .filter(|group| group.instructions > 0 || group.samples > 0)
    {
        println!(
            "{:<18} {:>13} {:>7.2}% {:>13} {:>7.2}% {:>12.3} {:>7.2}% {:>9}",
            group.name,
            group.instructions,
            100.0 * group.instructions as f64 / total_instructions as f64,
            group.guest_core_clocks,
            100.0 * group.guest_core_clocks as f64 / total_guest as f64,
            group.sample_wall_ns as f64 / 1_000_000.0,
            100.0 * group.sample_wall_ns as f64 / total_sample as f64,
            group.samples,
        );
    }

    let mut opcodes = snapshot.opcodes.clone();
    opcodes.sort_by_key(|opcode| Reverse((opcode.sample_wall_ns, opcode.instructions)));
    println!();
    println!(
        "=== cpu opcodes (top {}, sample_stride={}) ===",
        CPU_OPCODE_PROFILE_PRINT_LIMIT, snapshot.sample_stride
    );
    println!(
        "{:<8} {:<18} {:>13} {:>8} {:>13} {:>8} {:>12} {:>8} {:>9} {:>9} {:>9}",
        "opcode",
        "group",
        "instr",
        "instr%",
        "guest_clk",
        "guest%",
        "sample_ms",
        "sample%",
        "samples",
        "reg_i",
        "mem_i"
    );
    for opcode in opcodes
        .iter()
        .filter(|opcode| opcode.instructions > 0 || opcode.samples > 0)
        .take(CPU_OPCODE_PROFILE_PRINT_LIMIT)
    {
        println!(
            "{:<8} {:<18} {:>13} {:>7.2}% {:>13} {:>7.2}% {:>12.3} {:>7.2}% {:>9} {:>9} {:>9}",
            format_profile_opcode(opcode.opcode),
            opcode.group,
            opcode.instructions,
            100.0 * opcode.instructions as f64 / total_instructions as f64,
            opcode.guest_core_clocks,
            100.0 * opcode.guest_core_clocks as f64 / total_guest as f64,
            opcode.sample_wall_ns as f64 / 1_000_000.0,
            100.0 * opcode.sample_wall_ns as f64 / total_sample as f64,
            opcode.samples,
            opcode.register_instructions,
            opcode.memory_instructions,
        );
    }

    if !snapshot.hot_addrs.is_empty() {
        let total_samples: u64 = snapshot
            .hot_addrs
            .iter()
            .map(|&(_, s)| s)
            .sum::<u64>()
            .max(1);
        println!();
        println!(
            "=== hot sampled addresses (top {}, sample_stride={}) ===",
            snapshot.hot_addrs.len(),
            snapshot.sample_stride
        );
        println!("{:<10} {:>9} {:>8}", "linear", "samples", "top64%");
        for &(lin, samples) in &snapshot.hot_addrs {
            println!(
                "{lin:08X}   {samples:>9} {:>7.2}%",
                100.0 * samples as f64 / total_samples as f64
            );
        }
    }

    if !snapshot.smc_flush_blocks.is_empty() {
        println!();
        println!(
            "=== smc flush sources (top {}, 64-byte physical blocks) ===",
            snapshot.smc_flush_blocks.len()
        );
        println!("{:<10} {:>9}", "physical", "flushes");
        for &(block, flushes) in &snapshot.smc_flush_blocks {
            println!("{block:08X}   {flushes:>9}");
        }
    }
}

fn format_profile_opcode(opcode: u16) -> String {
    if opcode & 0x8000 != 0 {
        format!("{:02X}/{:02X}", 0xd8 + ((opcode >> 8) & 7), opcode as u8)
    } else if opcode & 0xff00 == 0x0f00 {
        format!("0F {:02X}", opcode as u8)
    } else {
        format!("{:02X}", opcode as u8)
    }
}

fn write_profile_json(
    exe_path: &Path,
    json_path: &Path,
    sample_stride: u64,
    baseline: &BenchRun,
    profiled: &BenchRun,
    baseline_metrics: BenchMetrics,
) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = json_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        && !parent.exists()
    {
        return Err(format!(
            "profile JSON parent directory does not exist: {}",
            parent.display()
        )
        .into());
    }

    let mut machine_phases = profiled.machine_profile.phases.clone();
    machine_phases.sort_by_key(|phase| Reverse(phase.wall_ns));
    let mut cpu_groups = profiled.cpu_profile.groups.clone();
    cpu_groups.sort_by_key(|group| Reverse(group.sample_wall_ns));
    let mut cpu_opcodes = profiled.cpu_profile.opcodes.clone();
    cpu_opcodes.sort_by_key(|opcode| Reverse((opcode.sample_wall_ns, opcode.instructions)));
    let report = json!({
        "schema": "izarravm-profile-v1",
        "exe": exe_path.display().to_string(),
        "mode": "586",
        "sample_stride": sample_stride.max(1),
        "baseline": {
            "machine_phase_timing_enabled": baseline.machine_profile.machine_phase_timing_enabled,
            "wall_ms": baseline_metrics.wall_ms,
            "guest_ms": baseline_metrics.guest_ms,
            "rt_factor": baseline_metrics.rt_factor,
            "cycles_per_iter": baseline_metrics.cycles_per_iter,
            "iters": baseline.iterations,
            "aux": baseline.aux,
        },
        "profile": {
            "wall_ms": profiled.wall.as_secs_f64() * 1000.0,
            "machine_phase_timing_enabled": profiled.machine_profile.machine_phase_timing_enabled,
            "machine_phases": machine_phases.iter().map(|phase| json!({
                "name": phase.name,
                "wall_ns": phase.wall_ns,
                "count": phase.count,
            })).collect::<Vec<_>>(),
            "cpu_groups": cpu_groups.iter().map(|group| json!({
                "name": group.name,
                "instructions": group.instructions,
                "guest_core_clocks": group.guest_core_clocks,
                "sample_wall_ns": group.sample_wall_ns,
                "samples": group.samples,
            })).collect::<Vec<_>>(),
            "cpu_opcodes": cpu_opcodes.iter().map(|opcode| json!({
                "opcode": format_profile_opcode(opcode.opcode),
                "opcode_raw": opcode.opcode,
                "group": opcode.group,
                "instructions": opcode.instructions,
                "guest_core_clocks": opcode.guest_core_clocks,
                "sample_wall_ns": opcode.sample_wall_ns,
                "samples": opcode.samples,
                "register_instructions": opcode.register_instructions,
                "memory_instructions": opcode.memory_instructions,
                "register_samples": opcode.register_samples,
                "memory_samples": opcode.memory_samples,
            })).collect::<Vec<_>>(),
            "perf": perf_counters_json(&profiled.perf, profiled.poll_skip_memory, profiled.jit_clif),
        },
    });
    std::fs::write(json_path, serde_json::to_string_pretty(&report)?)?;
    Ok(())
}

pub(super) fn perf_counters_json(
    perf: &PerfCounters,
    poll_skip_memory: izarravm_cpu::PollSkipMemoryCounters,
    jit_clif: izarravm_cpu::JitClifCounters,
) -> serde_json::Value {
    json!({
        "instructions": perf.instructions,
        "decode_misses": perf.decode_misses,
        "straight_line_runs": perf.straight_line_runs,
        "brk_decode_or_branch": perf.brk_decode_or_branch,
        "brk_step": perf.brk_step,
        "brk_interrupt": perf.brk_interrupt,
        "brk_cap": perf.brk_cap,
        "brk_halt": perf.brk_halt,
        "poll_skip_spans": perf.poll_skip_spans,
        "poll_skip_iterations": perf.poll_skip_iterations,
        "poll_neg_cache_hits": perf.poll_neg_cache_hits,
        "poll_neg_cache_stores": perf.poll_neg_cache_stores,
        "poll_neg_cache_volatile": perf.poll_neg_cache_volatile,
        "poll_head_prefilter_rejects": perf.poll_head_prefilter_rejects,
        "poll_skip_memory_spans": poll_skip_memory.spans,
        "poll_skip_memory_iterations": poll_skip_memory.iterations,
        "code_invalidations": perf.code_invalidations,
        "data_direct_reads": perf.data_direct_reads,
        "data_slow_reads": perf.data_slow_reads,
        "data_direct_writes": perf.data_direct_writes,
        "data_slow_writes": perf.data_slow_writes,
        "direct_page_hits": perf.direct_page_hits,
        "direct_page_misses": perf.direct_page_misses,
        "direct_data_pointer_reads": perf.direct_data_pointer_reads,
        "direct_data_pointer_writes": perf.direct_data_pointer_writes,
        "fetch_page_hits": perf.fetch_page_hits,
        "fetch_page_misses": perf.fetch_page_misses,
        "slow_prefetch_refills": perf.slow_prefetch_refills,
        "direct_map_invalidations": perf.direct_map_invalidations,
        "rep_string_iterations": perf.rep_string_iterations,
        "rep_string_fast_iterations": perf.rep_string_fast_iterations,
        "flag_materializations": perf.flag_materializations,
        "cache_tier_lookups": perf.cache_tier_lookups,
        "smc_narrow_kills": perf.smc_narrow_kills,
        "device_write_ranges": perf.device_write_ranges,
        "device_write_bytes": perf.device_write_bytes,
        "device_write_code_hits": perf.device_write_code_hits,
        "device_write_coarse_resets": perf.device_write_coarse_resets,
        "jit_region_entries": perf.jit_region_entries,
        "jit_region_insns": perf.jit_region_insns,
        "jit_native_insns": perf.jit_native_insns,
        "jit_helper_exits": perf.jit_helper_exits,
        "jit_native_memory_helpers": perf.jit_native_memory_helpers,
        "jit_direct_entries": perf.jit_direct_entries,
        "jit_direct_insns": perf.jit_direct_insns,
        "jit_direct_side_exits": perf.jit_direct_side_exits,
        "jit_direct_exit_cross_page_or_alignment": perf.jit_direct_exit_cross_page_or_alignment,
        "jit_direct_exit_unavailable_or_kind": perf.jit_direct_exit_unavailable_or_kind,
        "jit_direct_exit_permission": perf.jit_direct_exit_permission,
        "jit_direct_exit_code_watch": perf.jit_direct_exit_code_watch,
        "jit_direct_exit_other": perf.jit_direct_exit_other,
        "jit_direct_compile_attempts": perf.jit_direct_compile_attempts,
        "jit_direct_blocks_installed": perf.jit_direct_blocks_installed,
        "jit_direct_compile_ns": perf.jit_direct_compile_ns,
        "jit_direct_hot_hits": perf.jit_direct_hot_hits,
        "jit_direct_hash_hits": perf.jit_direct_hash_hits,
        "jit_direct_lookup_misses": perf.jit_direct_lookup_misses,
        "jit_direct_linked_transfers": perf.jit_direct_linked_transfers,
        "jit_direct_unresolved_exits": perf.jit_direct_unresolved_exits,
        "jit_direct_unresolved_static_unbound": perf.jit_direct_unresolved_static_unbound,
        "jit_direct_unresolved_static_hidden": perf.jit_direct_unresolved_static_hidden,
        "jit_direct_unresolved_dynamic_miss_or_unbound": perf.jit_direct_unresolved_dynamic_miss_or_unbound,
        "jit_direct_unresolved_dynamic_hidden": perf.jit_direct_unresolved_dynamic_hidden,
        "jit_direct_deferred_short": perf.jit_direct_deferred_short,
        "jit_direct_reject_observer": perf.jit_direct_reject_observer,
        "jit_direct_reject_interrupt_shadow": perf.jit_direct_reject_interrupt_shadow,
        "jit_direct_reject_aggregate_accounting": perf.jit_direct_reject_aggregate_accounting,
        "jit_direct_reject_mode_key": perf.jit_direct_reject_mode_key,
        "jit_direct_reject_x87_top": perf.jit_direct_reject_x87_top,
        "jit_direct_reject_cs_layout": perf.jit_direct_reject_cs_layout,
        "jit_direct_reject_cpl": perf.jit_direct_reject_cpl,
        "jit_direct_reject_data_segment": perf.jit_direct_reject_data_segment,
        "jit_direct_reject_alignment": perf.jit_direct_reject_alignment,
        "jit_direct_reject_fetch_limit": perf.jit_direct_reject_fetch_limit,
        "jit_direct_reject_zero_budget": perf.jit_direct_reject_zero_budget,
        "jit_direct_cache_resets": perf.jit_direct_cache_resets,
        "jit_direct_arena_compactions": perf.jit_direct_arena_compactions,
        "jit_direct_arena_compaction_live_blocks": perf.jit_direct_arena_compaction_live_blocks,
        "jit_direct_arena_compaction_bytes": perf.jit_direct_arena_compaction_bytes,
        "jit_direct_arena_compaction_failures": perf.jit_direct_arena_compaction_failures,
        "jit_direct_links_created": perf.jit_direct_links_created,
        "jit_direct_links_cleared": perf.jit_direct_links_cleared,
        "jit_direct_decode_dependencies_scanned": perf.jit_direct_decode_dependencies_scanned,
        "jit_direct_portals_hidden": perf.jit_direct_portals_hidden,
        "jit_native_block_ns": perf.jit_native_block_ns,
        "jit_native_block_samples": perf.jit_native_block_samples,
        "jit_native_load_hits": perf.jit_native_load_hits,
        "jit_native_store_hits": perf.jit_native_store_hits,
        "jit_paged_tlb_successes": perf.jit_paged_tlb_successes,
        "jit_clif_smc_unit_kills": jit_clif.smc_unit_kills,
        "jit_clif_smc_unit_restamps": jit_clif.smc_unit_restamps,
        "jit_clif_smc_unit_kills_no_layout": jit_clif.smc_unit_kills_no_layout,
        "jit_clif_smc_unit_kills_multi_slot": jit_clif.smc_unit_kills_multi_slot,
    })
}

pub(super) fn print_perf_counter_row(name: &str, mode: GswMode, perf: &PerfCounters) {
    let instructions = perf.instructions.max(1);
    let decode_hit = 100.0 * (1.0 - perf.decode_misses as f64 / instructions as f64);
    let insns_per_run = perf.instructions as f64 / perf.straight_line_runs.max(1) as f64;
    println!(
        "perf  {:<10} {:<5} instr={:>13}  decode_hit={:>6.2}%  insns/run={:>9.1}  \
         brk[branch/step/int/cap/halt]={}/{}/{}/{}/{}  \
         inval[cs/smc/other/all]={}/{}/{}/{} narrow={}  \
         data[rd d/s wr d/s]={}/{}/{}/{}  ptr[rd/wr]={}/{}  \
         page[h/m]={}/{}  fetch_page[h/m slow_refill]={}/{}/{}  \
         map_inv={}  dev_write[range/bytes/hit/coarse]={}/{}/{}/{}  rep[fast/all]={}/{}  flags_mat={}  cache_lookups={}  \
         jit[entries/insns/native/helper]={}/{}/{}/{} direct[e/i/x/link/unres/defer]={}/{}/{}/{}/{}/{}  \
         unresolved[static-unbound/static-hidden/dynamic-miss/dynamic-hidden]={}/{}/{}/{}  \
         compile[attempt/installed/ns]={}/{}/{} lookup[hot/hash/miss]={}/{}/{} links[new/clear/reset]={}/{}/{}  \
         portal[scan/hide]={}/{}  \
         arena[compact/live/bytes/fail]={}/{}/{}/{}  \
         gate[obs/shadow/agg/mode/top/cs/cpl/data/align/fetch/short/budget]={}/{}/{}/{}/{}/{}/{}/{}/{}/{}/{}/{}  \
         jit_mem[load/store/tlb/helper]={}/{}/{}/{}  jit_time[ns/samples]={}/{}",
        name,
        mode.canonical_name(),
        perf.instructions,
        decode_hit,
        insns_per_run,
        perf.brk_decode_or_branch,
        perf.brk_step,
        perf.brk_interrupt,
        perf.brk_cap,
        perf.brk_halt,
        perf.decode_inval_cs_load,
        perf.decode_inval_smc,
        perf.decode_inval_other,
        perf.code_invalidations,
        perf.smc_narrow_kills,
        perf.data_direct_reads,
        perf.data_slow_reads,
        perf.data_direct_writes,
        perf.data_slow_writes,
        perf.direct_data_pointer_reads,
        perf.direct_data_pointer_writes,
        perf.direct_page_hits,
        perf.direct_page_misses,
        perf.fetch_page_hits,
        perf.fetch_page_misses,
        perf.slow_prefetch_refills,
        perf.direct_map_invalidations,
        perf.device_write_ranges,
        perf.device_write_bytes,
        perf.device_write_code_hits,
        perf.device_write_coarse_resets,
        perf.rep_string_fast_iterations,
        perf.rep_string_iterations,
        perf.flag_materializations,
        perf.cache_tier_lookups,
        perf.jit_region_entries,
        perf.jit_region_insns,
        perf.jit_native_insns,
        perf.jit_helper_exits,
        perf.jit_direct_entries,
        perf.jit_direct_insns,
        perf.jit_direct_side_exits,
        perf.jit_direct_linked_transfers,
        perf.jit_direct_unresolved_exits,
        perf.jit_direct_deferred_short,
        perf.jit_direct_unresolved_static_unbound,
        perf.jit_direct_unresolved_static_hidden,
        perf.jit_direct_unresolved_dynamic_miss_or_unbound,
        perf.jit_direct_unresolved_dynamic_hidden,
        perf.jit_direct_compile_attempts,
        perf.jit_direct_blocks_installed,
        perf.jit_direct_compile_ns,
        perf.jit_direct_hot_hits,
        perf.jit_direct_hash_hits,
        perf.jit_direct_lookup_misses,
        perf.jit_direct_links_created,
        perf.jit_direct_links_cleared,
        perf.jit_direct_cache_resets,
        perf.jit_direct_decode_dependencies_scanned,
        perf.jit_direct_portals_hidden,
        perf.jit_direct_arena_compactions,
        perf.jit_direct_arena_compaction_live_blocks,
        perf.jit_direct_arena_compaction_bytes,
        perf.jit_direct_arena_compaction_failures,
        perf.jit_direct_reject_observer,
        perf.jit_direct_reject_interrupt_shadow,
        perf.jit_direct_reject_aggregate_accounting,
        perf.jit_direct_reject_mode_key,
        perf.jit_direct_reject_x87_top,
        perf.jit_direct_reject_cs_layout,
        perf.jit_direct_reject_cpl,
        perf.jit_direct_reject_data_segment,
        perf.jit_direct_reject_alignment,
        perf.jit_direct_reject_fetch_limit,
        perf.jit_direct_deferred_short,
        perf.jit_direct_reject_zero_budget,
        perf.jit_native_load_hits,
        perf.jit_native_store_hits,
        perf.jit_paged_tlb_successes,
        perf.jit_native_memory_helpers,
        perf.jit_native_block_ns,
        perf.jit_native_block_samples,
    );
    // Attribute combined decode-or-branch exits for profiling runs.
    println!(
        "  brk_attrib[decode_miss/not_continuable/page_cross]={}/{}/{}",
        perf.brk_cont_decode_miss, perf.brk_cont_not_continuable, perf.brk_cont_page_cross,
    );
}

/// Compare a measured `iters/sec` to the matching era reference band and return
/// a tag to append to the row: ` [in band]`, ` [LOW <ratio>]`, ` [HIGH <ratio>]`,
/// or empty when no band is encoded for this payload/mode.
fn band_tag(payload: &str, mode: GswMode, iters_per_sec: f64) -> String {
    use bench_reference::BandVerdict;
    let Some(band) = bench_reference::band_for(payload, mode) else {
        return String::new();
    };
    match band.verdict(iters_per_sec) {
        BandVerdict::InBand => " [in band]".to_string(),
        BandVerdict::Low => format!(" [LOW {:.2}]", iters_per_sec / band.target),
        BandVerdict::High => format!(" [HIGH {:.2}]", iters_per_sec / band.target),
    }
}

/// Block sizes swept by --headless-bandwidth, powers of two from 4 KB to 4 MB.
/// A block that fits the live mode's cache stays resident across passes; one that
/// exceeds it re-misses every pass. The largest block (4 MB) plus the 1 MB base
/// tops out at 5 MB, well inside the 24 MB machine.
const BANDWIDTH_BLOCKS: &[u32] = &[
    4 * 1024,
    8 * 1024,
    16 * 1024,
    32 * 1024,
    64 * 1024,
    128 * 1024,
    256 * 1024,
    512 * 1024,
    1024 * 1024,
    2 * 1024 * 1024,
    4 * 1024 * 1024,
];

/// A simplified SpeedSys-style memory-read bandwidth sweep. For each CPU mode it
/// drives the bus directly from the host (no guest program, so it can touch any
/// physical address) over a range of block sizes and prints MB/s per block, so a
/// human can see the L1/L2/RAM cache tiers as steps in the curve.
///
/// Every row is checked against the same hard reference window as the CPU
/// payloads. The curve must step down at each cache boundary.
pub(super) fn run_bandwidth(hardware: &HardwareProfile) -> Result<(), Box<dyn Error>> {
    // A fixed total budget per block: small blocks do many passes, large blocks a
    // few. 16 MB amortizes the cold first pass so the steady state dominates.
    const TOTAL: u64 = 16 * 1024 * 1024;

    let modes = [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ];

    println!(
        "memory read bandwidth sweep (TOTAL {} MB per block, base 0x{:06X})",
        TOTAL / (1024 * 1024),
        0x10_0000u32,
    );
    println!("(tier costs calibrated: the curve steps down at each cache boundary)");

    let mut out_of_band = false;
    for mode in modes {
        println!();
        println!(
            "mode {} @ {:.2} MHz  L1/L2 = {:?} KB",
            mode.canonical_name(),
            mode.clock_rate().as_hz_f64() / 1.0e6,
            mode.cache_kb(),
        );
        println!("{:>8} {:>12} {:>16}", "block", "MB/s", "band");
        for &block in BANDWIDTH_BLOCKS {
            // A fresh machine per (mode, size) so each measurement starts cold and
            // nothing carries over from the previous block.
            let mut machine = Machine::new_boot_image(
                MachineProfile::from_hardware_profile(hardware),
                izarravm_firmware::neurketa_image(),
            )?;
            machine.set_mode(mode);
            let sample = machine.measure_read_bandwidth(0x10_0000, block, TOTAL);
            let mb_per_sec = if sample.clocks > 0 {
                sample.bytes as f64 / mode.clock_rate().seconds_for_clocks(sample.clocks) / 1.0e6
            } else {
                0.0
            };
            let tag = bandwidth_band_tag(mode, block, mb_per_sec);
            if bench_reference::band_for(bandwidth_tier(mode, block), mode).is_some_and(|band| {
                band.verdict(mb_per_sec) != bench_reference::BandVerdict::InBand
            }) {
                out_of_band = true;
            }
            println!("{:>7}K {:>12.1} {:>16}", block / 1024, mb_per_sec, tag,);
        }
    }
    if out_of_band {
        Err("a memory-bandwidth row is outside its hard reference band".into())
    } else {
        Ok(())
    }
}

fn bandwidth_tier(mode: GswMode, block: u32) -> &'static str {
    let (l1_kb, l2_kb) = mode.cache_kb();
    let block_kb = block / 1024;
    if l1_kb != 0 && block_kb <= u32::from(l1_kb) {
        "bandwidth-l1"
    } else if l2_kb != 0 && block_kb <= u32::from(l2_kb) {
        "bandwidth-l2"
    } else {
        "bandwidth-ram"
    }
}

/// Tag a bandwidth row against the band for its active cache tier.
fn bandwidth_band_tag(mode: GswMode, block: u32, mb_per_sec: f64) -> String {
    use bench_reference::BandVerdict;
    let tier = bandwidth_tier(mode, block);
    let Some(band) = bench_reference::band_for(tier, mode) else {
        return String::new();
    };
    let label = tier.trim_start_matches("bandwidth-");
    match band.verdict(mb_per_sec) {
        BandVerdict::InBand => format!("{label} [in band]"),
        BandVerdict::Low => format!("{label} [LOW {:.2}]", mb_per_sec / band.target),
        BandVerdict::High => format!("{label} [HIGH {:.2}]", mb_per_sec / band.target),
    }
}
