// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

struct TokaEmmScenario {
    machine: Machine,
    _scratch: TokaScratch,
}

impl TokaEmmScenario {
    fn new(
        label: &str,
        profile: MachineProfile,
        ordered_overrides: Vec<(String, Vec<u8>)>,
    ) -> Self {
        let driver_count = ordered_overrides
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("TOKAEMM.SYS"))
            .count();
        assert_eq!(
            driver_count, 1,
            "{label}: expected exactly one TOKAEMM.SYS override"
        );

        let scratch = TokaScratch::new(label);
        let mut machine =
            Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
        machine
            .mount_hdd_folder_with(scratch.path(), ordered_overrides)
            .expect("mount host folder with overrides");

        Self {
            machine,
            _scratch: scratch,
        }
    }
}

/// Run the machine in fine bursts, reporting every sample point that lands with
/// the CPU in V86, and stopping early once `done` is satisfied. Returns
/// (samples in V86, whether `done` was reached).
///
/// Sampling `in_v86()` is the only way to observe V86 residency -- nothing
/// counts it -- and it has two biases that make a coarse sample useless:
///
/// * A burst ends where the timing model puts a seam, and seams cluster on
///   device deadlines and interrupt entries, which is exactly where the CPU is
///   inside the ring-0 monitor. Measured over a default boot, only about 2% of
///   200k-cycle sample points land in V86 at all.
/// * The guest only *runs* while it has work. At an idle DOS prompt it halts,
///   and the halt is taken in the monitor, so `in_v86()` reads false from then
///   on, permanently.
///
/// Together those mean a handful of samples taken after the boot has settled
/// will never see V86, however healthy the machine is. Sample finely, sample
/// while the guest is busy, and judge on the accumulated count.
fn sample_v86_while_busy(
    machine: &mut Machine,
    max_samples: u32,
    mut on_v86_sample: impl FnMut(&Machine),
    mut done: impl FnMut(&Machine) -> bool,
) -> (u32, bool) {
    const BURST: u64 = 200_000;
    let mut in_v86_samples = 0;
    for _ in 0..max_samples {
        let stop = machine
            .run_until_halt_or_cycles(BURST)
            .expect("machine run");
        if let StopReason::CpuError(message) = &stop {
            panic!(
                "CPU fault while sampling V86 residency: {message}\n{}",
                machine.screen_text().as_text()
            );
        }
        if machine.in_v86() {
            in_v86_samples += 1;
            on_v86_sample(machine);
        }
        if done(machine) {
            return (in_v86_samples, true);
        }
    }
    (in_v86_samples, false)
}

/// Enough V86 sample points to prove the guest really ran there, with room for
/// the rate to move: a default boot yields around 50, so a healthy machine
/// clears this by an order of magnitude, and a machine that stopped entering
/// V86 at all fails loudly rather than passing an invariant vacuously.
const MIN_V86_SAMPLES: u32 = 5;

#[test]
#[should_panic(expected = "expected exactly one TOKAEMM.SYS override")]
fn tokaemm_scenario_rejects_missing_driver_override() {
    let _ = TokaEmmScenario::new(
        "missing-driver",
        MachineProfile::gsw_386(16, VideoCard::Vega),
        Vec::new(),
    );
}

#[test]
#[should_panic(expected = "expected exactly one TOKAEMM.SYS override")]
fn tokaemm_scenario_rejects_duplicate_driver_overrides_case_insensitively() {
    let _ = TokaEmmScenario::new(
        "duplicate-driver",
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
            (
                "tokaemm.sys".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
        ],
    );
}

#[test]
fn tokaemm_scenario_owns_mounted_machine_and_scratch() {
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw386Slow;
    let expected_profile = profile.clone();
    let scratch_path = {
        let mut scenario = TokaEmmScenario::new(
            "scenario-owner",
            profile,
            vec![(
                "tOkAeMm.SyS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            )],
        );
        let scratch_path = scenario._scratch.path().to_owned();
        assert_eq!(scenario.machine.profile(), &expected_profile);
        assert_eq!(scenario.machine.read_physical_u8(0x475), 1);
        assert!(scratch_path.is_dir());
        scratch_path
    };
    assert!(!scratch_path.exists());
}

/// `DEVICE=C:\DOS\TOKAEMM.SYS` puts the running kernel into V86
/// under TOKAEMM's ring-0 monitor at SYSINIT, and real FreeDOS still finishes
/// booting to C:\> — every instruction and hardware IRQ from the DEVICE= line
/// onward runs virtualized. The gate: the DOS prompt reaches the screen.
///
/// CONFIG.SYS and TOKAEMM.SYS are both passed as `mount_hdd_folder_with`
/// overrides (which replace/append onto the committed system files). The host
/// `dir` stays empty: a CONFIG.SYS written there would collide with the
/// system CONFIG.SYS whose 8.3 name is reserved first, and lose the `~n` fold.
#[test]
#[ignore = "boots a full DOS image (slow in debug); run with --ignored"]
fn tokaemm_m0_freedos_survives_in_v86() {
    // The stock CONFIG.SYS (from the committed image) plus a DEVICE= line for
    // the bespoke driver. Passed as an override so it replaces the system copy.
    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();

    let mut scenario = TokaEmmScenario::new(
        "tokaemm-t3a",
        MachineProfile::gsw_386(16, VideoCard::Vega),
        vec![
            ("CONFIG.SYS".to_string(), config),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let (stop, _) = run_until_toka_condition(machine, 500_000_000, current_root_prompt);
    let text = machine.screen_text().as_text();
    // FreeDOS boots to the C:\> prompt with the whole system running in V86
    // under TOKAEMM's monitor (SYSINIT + FreeCOM + every IRQ virtualized).
    if !current_root_prompt(machine) {
        panic!("FreeDOS did not reach C:\\> in V86 (stop={stop:?}).\n{text}");
    }
    let prompts_before = text.to_ascii_lowercase().matches("c:\\>").count();

    // Run a command at the virtualized prompt: type `VER` and confirm the shell
    // executes it and returns to a fresh prompt — interactive DOS in V86.
    for ch in "ver\r".chars() {
        for code in ascii_to_set1(ch) {
            machine.inject_key_scancodes(&[code]);
        }
        let _ = machine.run_until_halt_or_cycles(20_000_000);
    }
    let _ = run_until_toka_condition(machine, 60_000_000, |machine| {
        current_root_prompt(machine)
            && machine
                .screen_text()
                .as_text()
                .to_ascii_lowercase()
                .matches("c:\\>")
                .count()
                > prompts_before
    });
    let after = machine.screen_text().as_text();
    let prompts = after.to_lowercase().matches("c:\\>").count();
    assert!(
        current_root_prompt(machine) && prompts > prompts_before,
        "VER did not run at the V86 prompt (expected a second C:\\>).\n{after}"
    );
}

#[test]
#[ignore = "boots six full DOS images in V86 (slow in debug); run with --ignored"]
fn tokaemm_small_ram_layouts_do_not_expose_out_of_range_pools() {
    for (memory_mib, emm_arg) in [1, 2, 4]
        .into_iter()
        .flat_map(|memory_mib| [(memory_mib, "RAM"), (memory_mib, "NOEMS")])
    {
        let config = format!(
            "FILES=20\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS {emm_arg}\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:1024 /P=C:\\AUTOEXEC.BAT\r\n"
        )
        .into_bytes();
        let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nVCPILOW\r\n".to_vec();
        let profile = MachineProfile::gsw_386(memory_mib, VideoCard::Vega);
        let mut scenario = TokaEmmScenario::new(
            &format!("tokaemm-small-{memory_mib}-{emm_arg}"),
            profile,
            vec![
                ("CONFIG.SYS".to_string(), config),
                ("AUTOEXEC.BAT".to_string(), autoexec),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "VCPILOW.COM".to_string(),
                    izarravm_firmware::vcpilow_com().to_vec(),
                ),
            ],
        );
        let machine = &mut scenario.machine;

        let stop = machine
            .run_until_halt_or_cycles(500_000_000)
            .expect("machine run");
        let text = machine.screen_text().as_text();
        assert_eq!(
            stop,
            StopReason::TestExit { code: 0xA5 },
            "small-memory pool probe failed with {memory_mib} MiB {emm_arg} \
             (stop={stop:?}); a 0xEn code names the failed step.\n{text}"
        );

        // Which of the two table placements ran, read off the CPU rather than
        // off anything the driver says about itself. CR3 is the page directory
        // the monitor is actually using, so this cannot be satisfied by a
        // driver that reports one thing and does another.
        //
        // Without this the fixture proved nothing about placement: it reads
        // pool bounds only, and at 1 MiB the arena is empty so it takes its own
        // empty-pool branch. Four of the six iterations run the high path, and
        // nothing here could tell.
        let cr3 = machine.cpu().control.cr3;
        if memory_mib == 1 {
            // Bound the fallback against the driver's own load base, not just
            // against 1 MB. "Below 1 MB" is satisfied by CR3 = 0, by tables
            // sitting inside the core, and by tables over the IVT, none of
            // which is what this path is supposed to do. The driver hooks
            // INT 67h at INIT, so its load segment is readable straight out of
            // the vector table, which is independent of anything the driver
            // reports about itself.
            //
            // Where the reservation STARTS is derived from the shipped image's
            // length, so it tracks the core automatically: the file ends
            // exactly at resident_core_end now that the tables are not emitted.
            //
            // TABLES_BYTES is the driver's own EQU and has no runtime trace, so
            // it has to be mirrored. That mirror went stale: it was 0x7000
            // (PD + 6 PT) until the 64 MB machine took it to 0x11000
            // (PD + 16 PT) in 95169c4e, and this test kept 0x7000 for two days.
            const TABLES_BYTES: u32 = 0x11000;
            const TABLES_SLACK: u32 = 0xFF0;
            let vector = 0x67 * 4 + 2;
            let drv_seg = u32::from(machine.read_physical_u8(vector))
                | (u32::from(machine.read_physical_u8(vector + 1)) << 8);
            let base = drv_seg << 4;
            let tables_off = (izarravm_firmware::tokaemm_sys().len() as u32).next_multiple_of(4096);
            assert!(
                cr3 >= base + tables_off,
                "at {memory_mib} MiB CR3 ({cr3:#010x}) is below the reservation \
                 at {:#010x}, so the tables are inside the driver core, not in \
                 the region reserved past the end of the file",
                base + tables_off
            );

            // The stale literal above was invisible because the bound it fed
            // could not fail. It read
            //     cr3 + TABLES_BYTES <= base + tables_off + TABLES_BYTES + SLACK
            // in which TABLES_BYTES CANCELS. Whatever value it held, the test
            // only ever asserted that CR3's page-rounding stayed under 0xFF0 --
            // never that the tables fit the reservation, which is what its
            // message claimed. Deriving the reservation end from the same
            // formula the driver uses cannot check that the driver's formula is
            // right. Keep the rounding check, but say what it is:
            assert!(
                cr3 - (base + tables_off) <= TABLES_SLACK,
                "at {memory_mib} MiB CR3 ({cr3:#010x}) rounds up more than the \
                 {TABLES_SLACK:#x} the reservation budgets over the \
                 4096-aligned TABLES_OFF at {:#010x}",
                base + tables_off
            );

            // For "DOS actually kept it", the bound has to come from DOS, not
            // from the driver's arithmetic. The lowest independent witness
            // available at this point is the running fixture itself: VCPILOW is
            // a .COM, so CS is its PSP + 0x10, and DOS placed it wherever the
            // reservation left off. Tables overlapping it means DOS handed out
            // memory the monitor is paging through.
            //
            // Resolution is ~132 KB at 1 MiB (COMMAND.COM, its environment and
            // the fixture sit between), so this catches a badly under-reported
            // break, not a marginal one. A tight bound needs an MCB-chain walk
            // and there is no MCB helper in the test suite yet; that is the
            // follow-up, not a reason to keep a bound that cannot fail at all.
            let fixture_lin = u32::from(machine.cpu().registers.cs().selector) << 4;
            assert!(
                cr3 + TABLES_BYTES <= fixture_lin,
                "at {memory_mib} MiB the seventeen tables at CR3 ({cr3:#010x}) \
                 run up to {:#010x}, into the memory DOS gave the running \
                 fixture at {fixture_lin:#010x}: the reported break did not \
                 cover them",
                cr3 + TABLES_BYTES
            );
        } else {
            assert!(
                cr3 >= 0x0013_8000,
                "at {memory_mib} MiB the tables must be reserved above the UMB \
                 window, but CR3 is {cr3:#010x}"
            );
        }
    }
}

/// A guest program install-checks XMS, allocates a 64 KB EMB,
/// locks it, moves a pattern conventional->EMB->conventional, verifies it, then
/// unlocks and frees — all in V86 under TOKAEMM's monitor (block MOVE traps to
/// the monitor's flat memcpy). XMSTEST.COM signals 0xA5 (success) via the
/// unit-tester exit port; any other code names the step that broke, and the
/// fixture's own failure-label block is the key.
///
/// The arena-SHAPE assertions this used to end with now live in XMSARENA.COM
/// (see the test below). They were last, behind all fifteen steps here, so any
/// regression above them reported an unrelated code and left them unrun.
///
/// The config is NOEMS so host EMS reserves no extended RAM and the guest XMS
/// driver owns all of it. EMS coexistence is covered separately.
/// XMSTEST runs from AUTOEXEC, so the machine stops as soon as it signals — no
/// interactive settling needed.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m1_xms_alloc_move_free_in_v86() {
    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nXMSTEST\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-m1",
        profile,
        vec![
            ("CONFIG.SYS".to_string(), config),
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
            (
                "XMSTEST.COM".to_string(),
                izarravm_firmware::xmstest_com().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "XMS round-trip did not report success (stop={stop:?}); \
             a 0xE0-0xEE code names the failed step.\n{text}"
    );
}

/// The arena-SHAPE assertions, split out of XMSTEST so a regression anywhere in
/// the XMS round trip cannot leave them unrun. 08h must report the largest free
/// block separately from the total, and a 1 KB request must cost 1 KB rather
/// than a whole page, which is what a 4 KB-page arena would charge.
///
/// XMSARENA.COM signals 0xA5 on success. 0xEF and 0xF0 are the two ASSERTIONS;
/// 0xD0, 0xD1, 0xF1 and 0xF2 are setup, kept distinct so an absent driver or an
/// arena too small to fragment can never be read as a failed assertion.
///
/// Same NOEMS config as the round-trip test above: under RAM, EMS draws from
/// the same shared arena and the free totals the fixture reasons about move.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m1b_xms_arena_shape_in_v86() {
    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nXMSARENA\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-m1b",
        profile,
        vec![
            ("CONFIG.SYS".to_string(), config),
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
            (
                "XMSARENA.COM".to_string(),
                izarravm_firmware::xmsarena_com().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "arena shape did not report success (stop={stop:?}); 0xEF means 08h \
             collapsed largest into total, 0xF0 means a 1 KB block did not cost \
             1 KB, and anything else is setup.\n{text}"
    );
}

/// With DEVICE=TOKAEMM.SYS + DOS=UMB, a guest program sets
/// the high-first allocation strategy and AH=48h-allocates a block that lands in
/// upper memory (segment >= 0xC800) with real RAM behind it (write/read a
/// pattern) — proving TOKAEMM page-mapped extended RAM into the upper holes and
/// FreeDOS's DOS=UMB linked our region. UMBTEST signals 0xA5 via the exit port;
/// a 0xEn code names the failed step.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m3_umb_load_high_in_v86() {
    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDOS=UMB\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nUMBTEST\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-m3",
        profile,
        vec![
            ("CONFIG.SYS".to_string(), config),
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
            (
                "UMBTEST.COM".to_string(),
                izarravm_firmware::umbtest_com().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "UMB load-high did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// Drives TOKAEMM's XMS 10h/11h/12h directly (no
/// DOS=UMB) to exercise the allocator paths the DOS=UMB e2e doesn't reach — the
/// too-big probe, alloc, grow, release, reuse-after-free — plus a write/read of
/// the paged RAM. UMBMECH signals 0xA5; a 0xEn code names the failed step.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m3_umb_direct_xms_in_v86() {
    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nUMBMECH\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-m3d",
        profile,
        vec![
            ("CONFIG.SYS".to_string(), config),
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
            (
                "UMBMECH.COM".to_string(),
                izarravm_firmware::umbmech_com().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "UMB mechanism round-trip did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// With DEVICE=TOKAEMM.SYS RAM, a guest program drives the
/// LIM EMS 4.0 API — version, frame segment, page counts, allocate — then maps
/// logical pages through the frame slots, writing distinct patterns and reading
/// them back through OTHER slots: the runtime page remap through the paged
/// frame, serviced by the monitor's INT 0xC0 'PM' PTE-rewrite. EMSTEST signals
/// 0xA5 via the exit port; a 0xEn code names the failed step.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m2_ems_map_write_read_in_v86() {
    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS RAM\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nEMSTEST\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-m2",
        profile,
        vec![
            ("CONFIG.SYS".to_string(), config),
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
            (
                "EMSTEST.COM".to_string(),
                izarravm_firmware::emstest_com().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "EMS map/write/read round-trip did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// With DEVICE=TOKAEMM.SYS RAM and DOS=UMB, the UMB
/// window ends below the EMS page frame (umb_win_end = 0xE000) and DOS=UMB
/// still links and allocates upper memory from the carved window — the frame
/// and the UMBs share the upper area under the guest driver's own bookkeeping.
/// Reuses the UMBTEST fixture (seg >= 0xC800 + write/read pattern).
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m2_umb_coexists_with_ems_frame_in_v86() {
    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDOS=UMB\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS RAM\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nUMBTEST\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-m2u",
        profile,
        vec![
            ("CONFIG.SYS".to_string(), config),
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
            (
                "UMBTEST.COM".to_string(),
                izarravm_firmware::umbtest_com().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "UMB alongside the EMS frame did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// DEVICE=TOKAEMM.SYS NOEMS
/// presents a FRAMELESS manager — INT 67h answers present/version 4.0, the
/// frame query returns 80h, page counts are zero, and allocation is refused
/// with 87h (the EMM386 NOEMS contract). EMSNONE signals 0xA5 / 0xEn.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m2_ems_frameless_noems_in_v86() {
    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nEMSNONE\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-m2f",
        profile,
        vec![
            ("CONFIG.SYS".to_string(), config),
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
            (
                "EMSNONE.COM".to_string(),
                izarravm_firmware::emsnone_com().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "frameless-default EMS contract did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// VCPI presence under DEVICE=TOKAEMM.SYS NOEMS (frameless mode,
/// no EMS pool — the stock-boot shape), INT 67h AX=DE00h answers VCPI 1.0
/// present (AH=0, BX=0100h), a not-yet-implemented DExx subfunction
/// answers 8Fh, untouched registers survive the call, and the plain EMS
/// interface keeps working on the shared vector. VCPIDET signals
/// 0xA5 / 0xEn.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_vcpi_m0_de00_present_on_frameless_noems() {
    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nVCPIDET\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-vcpi0",
        profile,
        vec![
            ("CONFIG.SYS".to_string(), config),
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
            (
                "VCPIDET.COM".to_string(),
                izarravm_firmware::vcpidet_com().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "VCPI DE00 presence contract did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// VCPI queries and page-pool behavior under RAM and NOEMS at 16 and 24 MiB.
/// The DE02-DE0B set answers free-page count over a real pool, max-page
/// query, alloc/free round-trip with 12-LSB masking, bad-free and
/// double-free rejection, V86 page-table lookups (identity + out-of-range
/// 8Bh), CR0 with PE|PG, the debug-register array shape, and the 8259
/// mapping report/record round-trip. VCPIMEM signals 0xA5 / 0xEn.
#[test]
#[ignore = "boots four full DOS images in V86 (slow in debug); run with --ignored"]
fn tokaemm_vcpi_m1_queries_and_page_pool() {
    for (memory_mib, emm_arg) in [(16, "RAM"), (16, "NOEMS"), (24, "RAM"), (24, "NOEMS")] {
        let config = format!(
            "FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS {emm_arg}\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        )
        .into_bytes();
        let profile = MachineProfile::gsw_386(memory_mib, VideoCard::Vega);
        let mut scenario = TokaEmmScenario::new(
            &format!("tokaemm-vcpi1-{memory_mib}-{emm_arg}"),
            profile,
            vec![
                ("CONFIG.SYS".to_string(), config),
                (
                    "AUTOEXEC.BAT".to_string(),
                    b"@ECHO OFF\r\nPATH C:\\DOS\r\nVCPIMEM\r\n".to_vec(),
                ),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "VCPIMEM.COM".to_string(),
                    izarravm_firmware::vcpimem_com().to_vec(),
                ),
            ],
        );
        let machine = &mut scenario.machine;
        let stop = machine
            .run_until_halt_or_cycles(800_000_000)
            .expect("machine run");
        let text = machine.screen_text().as_text();
        assert_eq!(
            stop,
            StopReason::TestExit { code: 0xA5 },
            "VCPI query/page-pool contract failed with {memory_mib} MiB {emm_arg} \
             (stop={stop:?}); a 0xEn code names the failed step.\n{text}"
        );
    }
}

/// Under DEVICE=TOKAEMM.SYS NOEMS, VCPI DE01 Get Protected Mode
/// Interface fills the client page-table buffer (identity first-MB
/// entries, software bits 9-11 cleared, exactly 0x110 entries, DI
/// advanced), furnishes the three server GDT descriptors (32-bit CPL0
/// code / flat-4GB data / driver data sharing the code base), and
/// returns a nonzero in-segment PM entry offset. VCPIIF signals
/// 0xA5 / 0xEn. The protected-mode entry is exercised by the switch
/// fixture because it can only be far-called from protected mode.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_vcpi_m2_de01_pm_interface() {
    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nVCPIIF\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-vcpi2",
        profile,
        vec![
            ("CONFIG.SYS".to_string(), config),
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
            (
                "VCPIIF.COM".to_string(),
                izarravm_firmware::vcpiif_com().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "VCPI DE01 interface contract did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// A minimal real VCPI client uses DE0C to walk the full extender
/// lifecycle under RAM and NOEMS at 16 and 24 MiB: DE01 interface setup,
/// DE0C into 16-bit protected mode under its own CR3/GDT/TSS (the
/// JEMM-traced switch flow), far-calls to the server PM entry (DE03
/// equal to the V86 baseline, DE04/DE05 round-trip), DE0C back to V86,
/// with marker registers proving the spec's register-preservation
/// contract across both switches and the pool balanced at the end.
/// VCPISW signals 0xA5 / 0xEn.
#[test]
#[ignore = "boots four full DOS images in V86 (slow in debug); run with --ignored"]
fn tokaemm_vcpi_m3_de0c_switch_round_trip() {
    for (memory_mib, emm_arg) in [(16, "RAM"), (16, "NOEMS"), (24, "RAM"), (24, "NOEMS")] {
        let config = format!(
            "FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS {emm_arg}\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        )
        .into_bytes();
        let profile = MachineProfile::gsw_386(memory_mib, VideoCard::Vega);
        let mut scenario = TokaEmmScenario::new(
            &format!("tokaemm-vcpi3-{memory_mib}-{emm_arg}"),
            profile,
            vec![
                ("CONFIG.SYS".to_string(), config),
                (
                    "AUTOEXEC.BAT".to_string(),
                    b"@ECHO OFF\r\nPATH C:\\DOS\r\nVCPISW\r\n".to_vec(),
                ),
                (
                    "TOKAEMM.SYS".to_string(),
                    izarravm_firmware::tokaemm_sys().to_vec(),
                ),
                (
                    "VCPISW.COM".to_string(),
                    izarravm_firmware::vcpisw_com().to_vec(),
                ),
            ],
        );
        let machine = &mut scenario.machine;
        let stop = machine
            .run_until_halt_or_cycles(800_000_000)
            .expect("machine run");
        let text = machine.screen_text().as_text();
        assert_eq!(
            stop,
            StopReason::TestExit { code: 0xA5 },
            "VCPI switch round-trip failed with {memory_mib} MiB {emm_arg} \
             (stop={stop:?}); a 0xEn code names the failed step.\n{text}"
        );
    }
}

/// A V86 program hooks INT 0Dh and
/// executes a privileged instruction the monitor does not emulate (the
/// literal DOS16M o32 LGDT startup shape) receives its own reflected
/// fault with fault-IP semantics and can skip-and-resume — instead of
/// the old signal32 diagnostic abort. GPREFLCT signals 0xA5 / 0xEn.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_vcpi_m4_unhandled_gp_reflects_to_guest() {
    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nGPREFLCT\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-vcpi4",
        profile,
        vec![
            ("CONFIG.SYS".to_string(), config),
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
            (
                "GPREFLCT.COM".to_string(),
                izarravm_firmware::gpreflct_com().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "V86 #GP reflection contract did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// VCPI privileged-0F emulation for the 386MAX GP_ESCOD surface: a V86
/// task executes MOV r32,CR0/CR3/CR2, MOV CR0,r32 (with PE|PG cleared in
/// the source — the monitor must force them back on), CLTS, and LMSW —
/// all #GP at CPL 3 — and the monitor must EMULATE them transparently
/// (the extender CR0-probe path) instead of reflecting a fault.
/// GPEMUL signals 0xA5 / 0xEn.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_vcpi_m6_privileged_0f_emulation() {
    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nGPEMUL\r\n".to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-vcpi6",
        profile,
        vec![
            ("CONFIG.SYS".to_string(), config),
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
            (
                "GPEMUL.COM".to_string(),
                izarravm_firmware::gpemul_com().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "V86 privileged-0F emulation did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// A fresh empty user folder gets the current defaults seeded
/// (`ensure_user_config`): DEVICE=TOKAEMM.SYS RAM + DOS=HIGH,UMB + LH
/// TOKAMOUS — and the boot reaches a C:\> prompt RUNNING IN V86 under the
/// TOKAEMM monitor, with the driver's signon banner on screen.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m4_default_boot_runs_v86() {
    let scratch = TokaScratch::new("tokaemm-m4");
    let dir = scratch.path();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut machine =
        Machine::new(profile, izarravm_firmware::izarra_bios()).expect("build machine");
    machine.mount_hdd_folder(dir).expect("mount host folder");

    // The seeding wrote real, editable defaults into the user folder.
    let seeded = std::fs::read_to_string(dir.join("CONFIG.SYS")).expect("seeded CONFIG.SYS");
    assert!(
        seeded.contains("DEVICE=C:\\DOS\\TOKAEMM.SYS RAM") && seeded.contains("DOS=HIGH,UMB"),
        "seeded CONFIG.SYS lacks the expected defaults:\n{seeded}"
    );

    // V86 residency is observed DURING the boot, not after it. The boot is the
    // work; once the prompt is up the guest halts and the CPU parks in the
    // monitor, so a sample taken then reads false no matter how healthy the
    // machine is. See sample_v86_while_busy.
    let (in_v86_samples, reached) = sample_v86_while_busy(
        &mut machine,
        4_000,
        |_| {},
        |machine| {
            current_root_prompt(machine)
                && machine
                    .screen_text()
                    .as_text()
                    .to_ascii_lowercase()
                    .contains("tokaemm xms/umb/ems memory manager; system running in v86")
        },
    );
    let text = machine.screen_text().as_text();
    let lower = text.to_ascii_lowercase();
    assert!(
        reached && current_root_prompt(&machine),
        "no C:\\> prompt on the default boot.\n{text}"
    );
    assert!(
        lower.contains("tokaemm xms/umb/ems memory manager; system running in v86"),
        "the TOKAEMM signon banner is missing.\n{text}"
    );
    assert!(
        in_v86_samples >= MIN_V86_SAMPLES,
        "the default boot must run the guest in V86; only {in_v86_samples} of \
         the sampled points were (needed {MIN_V86_SAMPLES}).\n{text}"
    );
    let prompts_before = lower.matches("c:\\>").count();

    // Presentation leak guard (audit item 9): run `ver /w` at the live prompt,
    // which used to print FreeDOS/Tim-Norman/sourceforge.net copyright text
    // straight from FreeCOM's DEFAULT.lng. The whole in-universe boot+shell
    // transcript (banner through the VER output) must stay leak-free.
    for ch in "ver /w\r".chars() {
        for code in ascii_to_set1(ch) {
            machine.inject_key_scancodes(&[code]);
        }
        let _ = machine.run_until_halt_or_cycles(20_000_000);
    }
    let _ = run_until_toka_condition(&mut machine, 60_000_000, |machine| {
        current_root_prompt(machine)
            && machine
                .screen_text()
                .as_text()
                .to_ascii_lowercase()
                .matches("c:\\>")
                .count()
                > prompts_before
    });
    let ver_text = machine.screen_text().as_text();
    let ver_lower = ver_text.to_ascii_lowercase();
    assert!(
        current_root_prompt(&machine) && ver_lower.matches("c:\\>").count() > prompts_before,
        "VER /W did not return to the V86 prompt.\n{ver_text}"
    );
    assert!(
        !ver_lower.contains("freedos"),
        "boot/VER transcript leaks \"FreeDOS\" branding.\n{ver_text}"
    );
    assert!(
        !ver_lower.contains("sourceforge"),
        "boot/VER transcript leaks a sourceforge.net URL.\n{ver_text}"
    );
}

/// Code 3 boots as 386-slow with TOKAEMM resident and DOS=HIGH,UMB. AUTOEXEC
/// checks the removed 286 name, selects 386-slow by its canonical name, runs
/// VER, then switches to 586. The commands come from AUTOEXEC.BAT because the
/// default keyboard layout can garble injected punctuation.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_and_gswmode_support_code_3_as_386_slow() {
    let config = b"FILES=40\r\nLASTDRIVE=D\r\n\
DEVICE=C:\\DOS\\TOKAEMM.SYS NOEMS\r\nDOS=HIGH,UMB\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nGSWMODE 286\r\n\
GSWMODE 386-slow\r\nVER\r\nGSWMODE 586\r\n"
        .to_vec();

    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.cpu = GswMode::Gsw386Slow;
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-gsw386slow",
        profile,
        vec![
            ("CONFIG.SYS".to_string(), config),
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
            (
                "GSWMODE.COM".to_string(),
                izarravm_firmware::gswmode_com().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let (stop, requested_cycles) =
        run_until_toka_condition_with_frozen_clock(machine, 800_000_000, |machine| {
            if machine.active_mode() != GswMode::Gsw586 || !current_root_prompt(machine) {
                return false;
            }
            let lower = machine.screen_text().as_text().to_ascii_lowercase();
            lower.contains("tokaemm xms/umb/ems memory manager; system running in v86")
                && lower.contains("switched to 386-slow")
                && lower.contains("switched to 586")
                && lower.contains("cpu mode '286' was removed; use '386-slow'")
        });
    if let StopReason::CpuError(msg) = &stop {
        let text = machine.screen_text().as_text();
        panic!(
            "CPU fault after the GSWMODE 386-slow switch while TOKAEMM's ring-0 \
                 monitor was resident after {requested_cycles} requested cycles: \
                 {msg}\nstop={stop:?}\n{text}"
        );
    }
    let text = machine.screen_text().as_text();

    assert_eq!(
        machine.active_mode(),
        GswMode::Gsw586,
        "GSWMODE 386-slow then GSWMODE 586 should leave the machine at 586 \
             after {requested_cycles} requested cycles (stop={stop:?}).\n{text}"
    );
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("tokaemm xms/umb/ems memory manager; system running in v86"),
        "TOKAEMM did not install while code 3 was active (stop={stop:?}).\n{text}"
    );
    assert!(
        lower.contains("switched to 386-slow") && lower.contains("switched to 586"),
        "GSWMODE confirmation output missing for one of the two switches.\n{text}"
    );
    assert!(
        lower.contains("cpu mode '286' was removed; use '386-slow'"),
        "GSWMODE did not explain how to migrate the removed 286 name.\n{text}"
    );
    assert!(
        current_root_prompt(machine),
        "no C:\\> prompt after the GSWMODE 386-slow/VER/GSWMODE 586 sequence \
             (stop={stop:?}).\n{text}"
    );
}

/// Audit item 10: the vendored FreeDOS MEM (toka-dos/freedos/mem) runs under
/// the default V86 boot and both `MEM` and `MEM /P` produce sane output.
/// Toka-DOS diverges from upstream MEM here: upstream's `/P` is only a
/// prefix of `/PAGE` (pause after each screenful); the owner's spec wants
/// `/P` to list resident programs with size + segment, so mem2.c's main()
/// was patched to make `/PAGE` (and therefore `/P`) imply `/FULL` and omit
/// the summary unless `/SUMMARY` is given (see toka-dos/freedos/VENDOR.md).
/// Each invocation gets its own boot because the 25-row text console cannot
/// hold both outputs at once. AUTOEXEC.BAT drives the commands, with no
/// injected typing.
struct MemScreen {
    text: String,
    columns: usize,
    cells: Vec<(u8, u8)>,
}

fn run_mem_autoexec_with_emm(
    dir_suffix: &str,
    commands: &str,
    emm_arg: Option<&str>,
) -> (TokaEmmScenario, MemScreen, StopReason) {
    let autoexec =
        format!("@ECHO OFF\r\nPATH C:\\DOS\r\nLH TOKAMOUS\r\n{commands}\r\n").into_bytes();
    let profile = MachineProfile::gsw_386(24, VideoCard::Vega);
    let mut overrides = vec![("AUTOEXEC.BAT".to_string(), autoexec)];
    if let Some(emm_arg) = emm_arg {
        overrides.push((
            "CONFIG.SYS".to_string(),
            format!(
                "FILES=40\r\nLASTDRIVE=D\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS {emm_arg}\r\n\
DOS=HIGH,UMB\r\nSHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
            )
            .into_bytes(),
        ));
    }
    overrides.push((
        "TOKAEMM.SYS".to_string(),
        izarravm_firmware::tokaemm_sys().to_vec(),
    ));
    let mut scenario =
        TokaEmmScenario::new(&format!("tokaemm-mem-{dir_suffix}"), profile, overrides);
    let machine = &mut scenario.machine;

    // /P retains upstream's /PAGE pauses. Keep the existing phase budgets,
    // but stop at the live root prompt and inject Enter only after a phase
    // expires at a possible pager prompt.
    let (mut stop, _) = run_until_toka_condition(machine, 200_000_000, current_root_prompt);
    for _ in 0..4 {
        if current_root_prompt(machine) || !matches!(stop, StopReason::CycleLimit { .. }) {
            break;
        }
        machine.inject_key_scancodes(&[0x1c, 0x9c]); // Enter: dismiss any pager
        (stop, _) = run_until_toka_condition(machine, 150_000_000, current_root_prompt);
    }
    let frame = machine.screen_text();
    if !matches!(stop, StopReason::CpuError(_)) && !current_root_prompt(machine) {
        let text = frame.as_text();
        panic!("MEM command did not return to C:\\> (stop={stop:?}).\n{text}");
    }
    let screen = MemScreen {
        text: frame.as_text(),
        columns: frame.columns,
        cells: frame
            .cells
            .iter()
            .map(|cell| (cell.character, cell.attribute))
            .collect(),
    };
    (scenario, screen, stop)
}

fn run_mem_autoexec(dir_suffix: &str, commands: &str) -> (TokaEmmScenario, MemScreen, StopReason) {
    run_mem_autoexec_with_emm(dir_suffix, commands, None)
}

fn run_mem_command(dir_suffix: &str, mem_args: &str) -> (TokaEmmScenario, MemScreen, StopReason) {
    run_mem_autoexec(dir_suffix, &format!("MEM {mem_args}"))
}

fn assert_extended_category(screen: &MemScreen, total: &str, free: &str) {
    let line = screen
        .text
        .lines()
        .find(|line| line.starts_with("Extended (XMS)"))
        .unwrap_or_else(|| panic!("MEM extended-memory row missing.\n{}", screen.text));
    assert!(
        line.contains(total) && line.contains(free),
        "MEM reported the wrong combined XMS/VCPI total or free space.\n{}",
        screen.text
    );
}

fn memory_map_rows(screen: &MemScreen) -> Vec<&[(u8, u8)]> {
    screen
        .cells
        .chunks_exact(screen.columns)
        .filter_map(|row| {
            let map = &row[..79];
            (map.iter()
                .all(|(character, _)| matches!(*character, 0xB0 | 0xB2))
                && row[79].0 == b' ')
                .then_some(map)
        })
        .collect()
}

#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_mem_plain_reports_conventional_memory() {
    let (_scenario, screen, stop) = run_mem_command("plain", "");
    let text = &screen.text;
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while running MEM under V86: {msg}\n{text}");
    }
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("c:\\>"),
        "no C:\\> prompt after MEM ran (stop={stop:?}).\n{text}"
    );
    assert!(
        lower.contains("conventional"),
        "MEM output doesn't mention conventional memory (stop={stop:?}).\n{text}"
    );
    assert!(
        !lower.contains("ems internal error"),
        "MEM could not enumerate TokaEMM's EMS handles.\n{text}"
    );
    assert!(
        lower.contains("toka-dos is resident in the high memory area"),
        "MEM should use the Toka-DOS name for the HMA resident.\n{text}"
    );
    // MS-DOS 6.22's shape for a manager that simulates EMS out of extended
    // memory: no separate `Expanded (EMS)` row (a shared pool would be
    // double-counted), one starred extended row instead.
    assert!(
        !text.contains("Expanded (EMS)"),
        "MEM still prints a separate EMS row over a shared pool:\n{text}"
    );
    for (label, total) in [
        ("Conventional", "640K"),
        ("Upper", "384K"),
        ("Extended (XMS)*", "23,552K"),
    ] {
        let line = text
            .lines()
            .find(|line| line.starts_with(label))
            .unwrap_or_else(|| panic!("MEM row {label:?} missing.\n{text}"));
        assert!(
            line.contains(total),
            "MEM row {label:?} has the wrong total.\n{text}"
        );
    }
    let conventional = text
        .lines()
        .find(|line| line.starts_with("Conventional"))
        .unwrap();
    assert_eq!(
        conventional.split_whitespace().nth(3),
        Some("582K"),
        "the high page tables should leave about 582 KiB conventional memory free \
         (the 64 MB arena bitmaps cost ~11 KiB of low core over the 24 MB era).\n{text}"
    );
    assert_extended_category(&screen, "23,552K", "23,165K");

    let rows = memory_map_rows(&screen);
    assert_eq!(rows.len(), 4, "MEM map should occupy four rows.\n{text}");
    let map = rows.into_iter().flatten().copied().collect::<Vec<_>>();
    assert_eq!(map.len(), 316);
    assert!(
        map.iter()
            .all(|(character, _)| matches!(*character, 0xB0 | 0xB2))
    );
    assert!(map.iter().any(|(character, _)| *character == 0xB0));
    assert!(map.iter().any(|(character, _)| *character == 0xB2));
    // Three bands now (conventional/upper/extended), not four: the shared
    // pool folded EMS into the extended row, so the map's last band runs
    // straight to the end instead of stopping short for a separate EMS slice.
    for (range, attribute) in [(0..8, 0x09), (8..13, 0x0B), (13..316, 0x0A)] {
        assert!(
            map[range.clone()]
                .iter()
                .all(|(_, actual)| *actual == attribute),
            "MEM map range {range:?} should use attribute {attribute:#04x}.\n{text}"
        );
    }
}

/// MS-DOS 6.22's MEM does not print an `Expanded (EMS)` row when the manager
/// simulates EMS out of extended memory -- it prints one starred extended row
/// and a footnote, because there is one pool and a second row would
/// double-count it. TOKAEMM is that kind of manager now, so MEM must say so.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_mem_folds_ems_into_the_extended_row() {
    let (_scenario, screen, stop) = run_mem_command("shared-pool", "");
    let text = &screen.text;
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while running MEM under V86: {msg}\n{text}");
    }
    assert!(
        text.to_ascii_lowercase().contains("c:\\>"),
        "no C:\\> prompt after MEM ran (stop={stop:?}).\n{text}"
    );
    assert!(
        !text.contains("Expanded (EMS)"),
        "MEM still prints a separate EMS row over a shared pool:\n{text}"
    );
    assert!(
        text.contains("Extended (XMS)*"),
        "MEM did not star the extended row:\n{text}"
    );
    assert!(
        text.contains("simulate EMS memory as needed"),
        "MEM did not print the shared-pool footnote:\n{text}"
    );
}

#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_mem_noems_reports_combined_extended_free() {
    let (_scenario, screen, stop) = run_mem_autoexec_with_emm("noems", "MEM", Some("NOEMS"));
    if let StopReason::CpuError(msg) = &stop {
        panic!(
            "CPU fault while running MEM with NOEMS: {msg}\n{}",
            screen.text
        );
    }
    assert!(
        screen.text.to_ascii_lowercase().contains("c:\\>"),
        "no C:\\> prompt after MEM ran with NOEMS (stop={stop:?}).\n{}",
        screen.text
    );
    assert_extended_category(&screen, "23,552K", "23,165K");
}

#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_mem_redirect_keeps_raw_uncolored_bars() {
    let (_scenario, screen, stop) =
        run_mem_autoexec("redirect", "MEM > C:\\MEM.TXT\r\nTYPE C:\\MEM.TXT");
    if let StopReason::CpuError(msg) = &stop {
        panic!(
            "CPU fault while redirecting MEM under V86: {msg}\n{}",
            screen.text
        );
    }
    let rows = memory_map_rows(&screen);
    assert_eq!(
        rows.len(),
        4,
        "redirected MEM map should occupy four rows.\n{}",
        screen.text
    );
    let map = rows.into_iter().flatten().copied().collect::<Vec<_>>();
    assert_eq!(map.len(), 316);
    assert!(
        map.iter()
            .all(|(character, _)| matches!(*character, 0xB0 | 0xB2))
    );
    assert!(
        map.iter()
            .all(|(_, attribute)| !matches!(*attribute, 0x09 | 0x0A | 0x0B | 0x0D))
    );
}

#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_mem_p_lists_resident_programs() {
    let (_scenario, screen, stop) = run_mem_command("p", "/P");
    let text = &screen.text;
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while running MEM /P under V86: {msg}\n{text}");
    }
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("c:\\>"),
        "no C:\\> prompt after MEM /P ran (stop={stop:?}).\n{text}"
    );
    // Toka-DOS divergence check: /P must produce the per-program size and
    // segment listing (upstream /P is only pagination). TOKAMOUS was loaded
    // high right before MEM ran, so it must appear in that listing. /P omits
    // the large summary unless /SUMMARY is also specified, which keeps the
    // final program rows visible.
    let upper = text.to_ascii_uppercase();
    assert!(
        upper.contains("TOKAMOUS"),
        "MEM /P output doesn't list the resident TOKAMOUS module \
             (stop={stop:?}).\n{text}"
    );
    assert!(
        upper.contains("UPPER MEMORY DETAIL:"),
        "MEM /P should label the upper-memory section.\n{text}"
    );
    assert!(
        !lower.contains("memory map:"),
        "bare MEM /P should leave the summary out.\n{text}"
    );
}

#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_mem_p_labels_both_memory_areas() {
    let commands = "MEM /P > C:\\MEMP.TXT\r\n\
                    FIND \"Memory Detail:\" C:\\MEMP.TXT\r\n\
                    FIND \"TOKAMOUS\" C:\\MEMP.TXT";
    let (_scenario, screen, stop) = run_mem_autoexec("p_areas", commands);
    if let StopReason::CpuError(msg) = &stop {
        panic!(
            "CPU fault while checking MEM /P area headers: {msg}\n{}",
            screen.text
        );
    }
    let upper = screen.text.to_ascii_uppercase();
    assert!(
        upper.contains("CONVENTIONAL MEMORY DETAIL:") && upper.contains("UPPER MEMORY DETAIL:"),
        "MEM /P should label both memory areas.\n{}",
        screen.text
    );
    assert!(
        upper.contains("TOKAMOUS"),
        "the redirected MEM /P listing should contain TOKAMOUS.\n{}",
        screen.text
    );
}

#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_mem_classify_reports_reduced_low_resident_size() {
    let (_scenario, screen, stop) = run_mem_command("classify", "/CLASSIFY /NOSUMMARY");
    if let StopReason::CpuError(msg) = &stop {
        panic!(
            "CPU fault while running MEM /CLASSIFY: {msg}\n{}",
            screen.text
        );
    }
    let tokaemm = screen
        .text
        .lines()
        .find(|line| {
            // The signon banner also starts with "TOKAEMM " now that the /T
            // restyle dropped the colon, so require a digit right after the
            // name+whitespace run to land on the table row, not the banner.
            line.trim_start()
                .strip_prefix("TOKAEMM")
                .map(|rest| rest.trim_start().starts_with(|c: char| c.is_ascii_digit()))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("MEM /CLASSIFY did not list TOKAEMM.\n{}", screen.text));
    assert!(
        tokaemm.contains("(40K)"),
        "TokaEMM should retain only its ~40 KiB low core: the 24 MB-era 29 KiB \
         of code and state plus the 64 MB machine's arena bitmaps and EMS \
         chain table.\n{}",
        screen.text
    );
    let free = screen
        .text
        .lines()
        .find(|line| line.trim_start().starts_with("Free"))
        .unwrap_or_else(|| panic!("MEM /CLASSIFY did not list free memory.\n{}", screen.text));
    assert_eq!(
        free.split_whitespace().nth(4),
        Some("(582K)"),
        "MEM /CLASSIFY should report about 582 KiB conventional free.\n{}",
        screen.text
    );
}

#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_mem_p_summary_restores_memory_map() {
    let (_scenario, screen, stop) = run_mem_command("p_summary", "/P /SUMMARY");
    if let StopReason::CpuError(msg) = &stop {
        panic!(
            "CPU fault while running MEM /P /SUMMARY under V86: {msg}\n{}",
            screen.text
        );
    }
    assert!(
        screen.text.to_ascii_lowercase().contains("memory map:"),
        "MEM /P /SUMMARY should restore the memory summary.\n{}",
        screen.text
    );
    assert_eq!(
        memory_map_rows(&screen).len(),
        4,
        "MEM /P /SUMMARY should restore all four map rows.\n{}",
        screen.text
    );
}

/// Regression for the V86 IRET/IOPL gate (vorvek/v86-iret-iopl): TOKAEMM
/// virtualizes IF by trapping CLI/STI/PUSHF/POPF/INT n/IRET to the monitor
/// and stamping the guest IRET frame's image-IF from its own VIF (often 0 in
/// ISR context). If IRET is not IOPL-gated like its siblings, a V86 guest's
/// own IRET pops that monitor-stamped image straight into REAL EFLAGS via
/// load_flags (no IOPL gating) -- killing real IF inside V86 so interrupts
/// never deliver again (this was the Prince of Persia livelock root cause).
/// This test samples real IF at several points across a real TOKAEMM boot
/// and asserts it is never 0 while the guest is in V86 mode -- the invariant
/// that would have caught this whole class of bug. Cheap: reuses the MEM
/// harness's boot (LH TOKAMOUS + MEM reaches a prompt in ~200-350M cycles),
/// split into small bursts so the sample points fall throughout the run
/// rather than only at the very end.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_real_if_never_zero_in_v86_across_a_boot() {
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nLH TOKAMOUS\r\nMEM\r\n".to_vec();
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-ifinvariant",
        profile,
        vec![
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    const FLAG_IF: u32 = 0x0000_0200;

    // Every sample point that lands in V86 is one check of the invariant, so
    // the fine sampling is not only what makes the test reliable -- it is what
    // gives it teeth. The old 20M-cycle bursts got roughly one V86 sample per
    // run, when they got one at all.
    let (saw_v86_samples, _) = sample_v86_while_busy(
        machine,
        4_000,
        |machine| {
            assert_ne!(
                machine.cpu().registers.eflags & FLAG_IF,
                0,
                "real IF was 0 while the guest was in V86 mode"
            );
        },
        |_| false,
    );
    let text = machine.screen_text().as_text();

    assert!(
        saw_v86_samples >= MIN_V86_SAMPLES,
        "the boot never entered V86 mode; the invariant was never exercised \
         ({saw_v86_samples} samples, needed {MIN_V86_SAMPLES}).\n{text}"
    );
}

/// Audit items 3+10 external tool batch (toka-dos/freedos/VENDOR.md): smoke
/// tests three of the newly-vendored tools in one boot -- ATTRIB (set +
/// query the read-only flag), CHOICE (piped default answer), and FIND
/// (string match against a text file) -- each producing assertable screen
/// output. The rest of the batch (MORE, LABEL, DELTREE) are covered by "the
/// image builds and boots" (the default-boot e2e test above stays green).
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_tool_batch_attrib_choice_find_smoke() {
    // A two-line text file so FIND's match is unambiguous against the
    // non-matching line right next to it.
    let hello_txt = b"Hello from Toka-DOS\r\nWelcome to the IZARRA 3000\r\n".to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\n\
ATTRIB +R HELLO.TXT\r\n\
ATTRIB HELLO.TXT\r\n\
ECHO Y | CHOICE /C:YN Continue\r\n\
FIND \"IZARRA\" HELLO.TXT\r\n"
        .to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-toolbatch",
        profile,
        vec![
            ("AUTOEXEC.BAT".to_string(), autoexec),
            ("HELLO.TXT".to_string(), hello_txt),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let (stop, _) = run_until_toka_condition(machine, 400_000_000, |machine| {
        if !current_root_prompt(machine) {
            return false;
        }
        let upper = machine.screen_text().as_text().to_ascii_uppercase();
        upper.contains("[---RA]") && upper.contains("CONTINUE") && upper.contains("IZARRA 3000")
    });
    let text = machine.screen_text().as_text();
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while running the tool batch under V86: {msg}\n{text}");
    }

    assert!(
        current_root_prompt(machine),
        "no C:\\> prompt after the tool batch ran (stop={stop:?}).\n{text}"
    );

    // ATTRIB: the second invocation (plain query, no +/-) must show the R
    // flag the first invocation just set. Attribute column order is
    // D,H,S,R,A (attr2str in ATTRIB.C), so a read-only, non-hidden,
    // non-system, archived file prints "[---RA]".
    let upper = text.to_ascii_uppercase();
    assert!(
        upper.contains("[---RA]"),
        "ATTRIB HELLO.TXT didn't show the R flag set by ATTRIB +R \
             (stop={stop:?}).\n{text}"
    );

    // CHOICE: piped "Y" must be accepted (not left hanging on a prompt);
    // the prompt text itself must have appeared on screen.
    assert!(
        upper.contains("CONTINUE"),
        "CHOICE prompt text didn't appear on screen (stop={stop:?}).\n{text}"
    );

    // FIND: must print the matching line, not the non-matching one.
    assert!(
        upper.contains("IZARRA 3000"),
        "FIND didn't print the matching line (stop={stop:?}).\n{text}"
    );
}

/// XCOPY (toka-dos/tools-src/xcopy/xcopy.c, an original Toka-DOS project
/// tool, not vendored -- see toka-dos/msdos4/VENDOR.md): builds a small
/// source tree (a top-level file plus a subdirectory with its own file),
/// copies it recursively with `/S /Y`, then verifies the copy landed at
/// the right depth (TYPE on the nested file) and that DIR + the XCOPY
/// summary line both show up on screen.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_tool_xcopy_recursive_smoke() {
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\n\
MD SRC\r\n\
ECHO hello > SRC\\A.TXT\r\n\
MD SRC\\SUB\r\n\
ECHO world > SRC\\SUB\\B.TXT\r\n\
XCOPY SRC DEST /S /Y\r\n\
TYPE DEST\\SUB\\B.TXT\r\n\
DIR DEST\r\n"
        .to_vec();

    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-xcopy",
        profile,
        vec![
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let (stop, _) = run_until_toka_condition(machine, 500_000_000, |machine| {
        if !current_root_prompt(machine) {
            return false;
        }
        let text = machine.screen_text().as_text();
        let lower = text.to_ascii_lowercase();
        let upper = text.to_ascii_uppercase();
        lower.contains("world")
            && upper.contains("A.TXT")
            && upper.contains("SUB")
            && upper.contains("2 FILE(S) COPIED")
    });
    let text = machine.screen_text().as_text();
    if let StopReason::CpuError(msg) = &stop {
        panic!("CPU fault while running the XCOPY batch under V86: {msg}\n{text}");
    }

    let lower = text.to_ascii_lowercase();
    assert!(
        current_root_prompt(machine),
        "no C:\\> prompt after the XCOPY batch ran (stop={stop:?}).\n{text}"
    );

    let upper = text.to_ascii_uppercase();

    // TYPE DEST\SUB\B.TXT: the nested file was copied to the right depth
    // and its contents are intact.
    assert!(
        lower.contains("world"),
        "TYPE didn't print the recursively-copied nested file's contents \
             (stop={stop:?}).\n{text}"
    );

    // DIR DEST: the top-level copied file and the copied subdirectory
    // both show up in the destination.
    assert!(
        upper.contains("A.TXT") && upper.contains("SUB"),
        "DIR DEST didn't list the copied file and subdirectory \
             (stop={stop:?}).\n{text}"
    );

    // XCOPY prints a final "N File(s) copied" summary; two files (A.TXT,
    // SUB\B.TXT) were copied.
    assert!(
        upper.contains("2 FILE(S) COPIED"),
        "XCOPY's File(s) copied summary line didn't show the expected count \
             (stop={stop:?}).\n{text}"
    );
}

/// The PS/2 mouse works under the default V86 boot. A host-injected
/// wheel detent travels 8042 -> slave IRQ12 -> vector 0x74 -> the monitor's
/// slave reflect stub -> guest INT 74h -> TOKAMOUS (loaded HIGH) -> INT 33h
/// fn 03h, where MOUSETST polls it. Signals 0xA5; a 0xEn names the step.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m4_mouse_wheel_under_v86() {
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nLH TOKAMOUS\r\nMOUSETST\r\n".to_vec();
    let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-m4m",
        profile,
        vec![
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "MOUSETST.COM".to_string(),
                izarravm_firmware::mousetst_com().to_vec(),
            ),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    // Run in chunks, injecting a wheel detent between them: the fixture polls
    // fn 03h in a bounded loop, so extra/early detents are harmless and a late
    // boot still sees one.
    let mut stop = machine
        .run_until_halt_or_cycles(200_000_000)
        .expect("machine run");
    for _ in 0..10 {
        if matches!(stop, StopReason::TestExit { .. } | StopReason::CpuError(_)) {
            break;
        }
        machine.inject_mouse_wheel(1);
        stop = machine
            .run_until_halt_or_cycles(200_000_000)
            .expect("machine run");
    }
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "mouse wheel under V86 did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// Under V86, SB16 IRQ5 lands on vector 13, shared with #GP,
/// and the monitor's discriminator must route each correctly. SNDTST hooks
/// INT 0Dh, resets the DSP, then requests immediate 8-bit IRQs (DSP 0xF2)
/// inside a CLI/STI-dense loop. Signals 0xA5; a 0xEn names the step.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_m4_sb16_irq5_under_v86() {
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nSNDTST\r\n".to_vec();
    // Pinned to IRQ5 EXPLICITLY rather than taken from the default, which is now
    // IRQ7. The whole point of this fixture is that IRQ5 lands on vector 13, the
    // vector the monitor also uses for #GP; on IRQ7 there is no collision and it
    // would quietly stop testing the discriminator while still passing.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.sound_blaster.irq = izarravm_core::SbIrq::I5;
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-m4s",
        profile,
        vec![
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "SNDTST.COM".to_string(),
                izarravm_firmware::sndtst_com().to_vec(),
            ),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "SB16 IRQ5 under V86 did not report success (stop={stop:?}); \
             a 0xEn code names the failed step.\n{text}"
    );
}

/// V86 trap tax regression: IRQ5 delivered while the interrupted code sits
/// at IP == 0. The vec13 frame-shape check cannot decide this case alone --
/// the error-code slot reads 0 for a #GP AND for an IRQ frame whose return
/// EIP is 0 -- so the monitor must fall through to its opcode-peek + cold
/// PIC-probe layers. A slot-only discriminator mis-routed such a delivery
/// into the #GP path, hit the non-sensitive byte at CS:0, and hard-killed
/// the VM (the review probe); this pins the three-layer scheme.
///
/// IRQ5IP0 makes IP == 0 the common case with SB16 auto-init DMA (NOT the
/// one-shot DSP 0xF2, whose re-arm races the ISR -- see the fixture header):
/// once armed, the DMA block boundary raises IRQ5 continuously on the card's
/// own schedule while the guest simply parks on a `jmp $` at offset 0 of a
/// segment, so deliveries land at IP == 0 with no re-arm. This test is RED
/// on the buggy slot-only monitor (the VM dies, a foreign TestExit code) and
/// GREEN only on the three-layer fix.
#[test]
#[ignore = "boots a full FreeDOS image (slow); run with --ignored"]
fn tokaemm_irq5_at_ip0_discriminated_under_v86() {
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nIRQ5IP0\r\n".to_vec();
    // Pinned to IRQ5 EXPLICITLY rather than taken from the default, which is now
    // IRQ7. The whole point of this fixture is that IRQ5 lands on vector 13, the
    // vector the monitor also uses for #GP; on IRQ7 there is no collision and it
    // would quietly stop testing the discriminator while still passing.
    let mut profile = MachineProfile::gsw_386(16, VideoCard::Vega);
    profile.sound_blaster.irq = izarravm_core::SbIrq::I5;
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-ip0",
        profile,
        vec![
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "IRQ5IP0.COM".to_string(),
                izarravm_firmware::irq5ip0_com().to_vec(),
            ),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;

    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "IRQ5 at IP==0 under V86 did not report success (stop={stop:?}); \
             0xE1 = DSP reset failed, a hang/CycleLimit or a foreign TestExit \
             code means the discriminator mis-routed the delivery.\n{text}"
    );
}

/// DOS/16M decides whether XMS shares a pool with the other memory interfaces
/// by taking every free XMS kilobyte and re-reading their free counts. TOKAEMM
/// must answer honestly in both directions or a DOS/4GW client keeps the XMS
/// block and finds the VCPI pool empty -- the `DOS/16M error: [23] no memory
/// for VCPI page table` that this whole change exists to fix. EMS enabled: the
/// NOEMS path was always honest because it probes VCPI DE03 instead.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_pool_overlap_is_visible_to_both_interfaces() {
    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS RAM\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nEMMPROBE\r\n".to_vec();

    let profile = MachineProfile::gsw_386(24, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-emmprobe",
        profile,
        vec![
            ("CONFIG.SYS".to_string(), config),
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
            (
                "EMMPROBE.COM".to_string(),
                izarravm_firmware::emmprobe_com().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;
    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "pool-overlap probe failed (stop={stop:?}); 0xE4 means the EMS free \
         count did not move while XMS held the whole arena, and any other \
         0xEn names a different step -- read the failure-label block in \
         emmprobe.asm before believing the 0xE4 story.\n{text}"
    );
}

/// EMS backs its pages from the shared arena (Task 6): `ems_page_alloc` takes
/// one 16 KB page at a time off the same bitmap XMS and VCPI draw from, so a
/// handle's logical pages need not land in one contiguous run. This fixture
/// proves that against a deliberately fragmented pool: it reads the pool's
/// actual free count (`AH=42h`; there is no fixed-192-page partition to pin a
/// baseline to any more, and the pool need not even start fully free -- the
/// shell's own XMS-swap block for running this child may already hold part
/// of it, which is now normal since EMS and XMS share one pool), derives a
/// uniform hole size from that total split across 32 handles, drains the
/// pool with those handles, and frees every other one to leave 16 isolated
/// same-size holes. A real `INT 67h AH=43h` probe for one page more than a
/// single hole then doubles as a discriminator rather than a hard gate: on
/// the non-contiguous allocator it always succeeds (satisfying it only costs
/// one free page at a time, not a run), so the fixture releases that probe
/// handle and moves on to a fresh, larger request -- proved to succeed by
/// writing and reading back a per-page signature through the frame after
/// mapping each of its logical pages.
#[test]
#[ignore = "boots a full DOS image in V86 (slow in debug); run with --ignored"]
fn tokaemm_ems_allocates_from_a_fragmented_shared_arena() {
    let config = b"FILES=40\r\nLASTDRIVE=Z\r\nDEVICE=C:\\DOS\\TOKAEMM.SYS RAM\r\n\
SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n"
        .to_vec();
    let autoexec = b"@ECHO OFF\r\nPATH C:\\DOS\r\nEMSFRAG\r\n".to_vec();

    let profile = MachineProfile::gsw_386(24, VideoCard::Vega);
    let mut scenario = TokaEmmScenario::new(
        "tokaemm-emsfrag",
        profile,
        vec![
            ("CONFIG.SYS".to_string(), config),
            ("AUTOEXEC.BAT".to_string(), autoexec),
            (
                "TOKAEMM.SYS".to_string(),
                izarravm_firmware::tokaemm_sys().to_vec(),
            ),
            (
                "EMSFRAG.COM".to_string(),
                izarravm_firmware::emsfrag_com().to_vec(),
            ),
        ],
    );
    let machine = &mut scenario.machine;
    let stop = machine
        .run_until_halt_or_cycles(800_000_000)
        .expect("machine run");
    let text = machine.screen_text().as_text();
    assert_eq!(
        stop,
        StopReason::TestExit { code: 0xA5 },
        "fragmented-EMS allocation failed (stop={stop:?}); 0xEA is the \
         defect this fixture exists to catch (EMS refused a request that \
         fit only non-contiguously, with plenty of pages free but none of \
         them in one big enough run), 0xE1-0xE9 name a setup or premise \
         failure in the fixture itself (read emsfrag.asm), and 0xEB-0xED \
         mean EMS granted the pages but mapped one of them wrong -- read \
         the failure-label block in emsfrag.asm before trusting any \
         particular story.\n{text}"
    );
}
