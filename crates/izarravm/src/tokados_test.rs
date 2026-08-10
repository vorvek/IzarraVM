// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_machine::StopReason;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TOKA_SCRATCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TokaScratch {
    path: PathBuf,
}

impl TokaScratch {
    fn new(label: &str) -> Self {
        let label = label
            .chars()
            .map(|character| match character {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => character,
                _ => '_',
            })
            .collect::<String>();
        loop {
            let sequence = TOKA_SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "izarravm-toka-{label}-{}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self { path },
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!(
                    "create Toka-DOS scratch directory {}: {error}",
                    path.display()
                ),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TokaScratch {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "Toka-DOS scratch directory preserved after panic: {}",
                self.path.display()
            );
            return;
        }
        match fs::remove_dir_all(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!(
                "remove Toka-DOS scratch directory {}: {error}",
                self.path.display()
            ),
        }
    }
}

fn current_text_line(machine: &Machine) -> Option<String> {
    let frame = machine.screen_text();
    if frame.columns == 0 {
        return None;
    }
    let cursor_row = usize::from(frame.cursor_offset) / frame.columns;
    (cursor_row < frame.rows).then(|| frame.line_string(cursor_row))
}

fn current_prompt(machine: &Machine, prompt: &str) -> bool {
    current_text_line(machine).is_some_and(|line| line.trim().eq_ignore_ascii_case(prompt))
}

fn current_root_prompt(machine: &Machine) -> bool {
    current_prompt(machine, "C:\\>")
}

fn run_until_toka_condition(
    machine: &mut Machine,
    cycles: u64,
    complete: impl FnMut(&Machine) -> bool,
) -> (StopReason, u64) {
    run_until_toka_condition_with_clock(machine, cycles, None, complete)
}

fn run_until_toka_condition_with_frozen_clock(
    machine: &mut Machine,
    cycles: u64,
    complete: impl FnMut(&Machine) -> bool,
) -> (StopReason, u64) {
    let ticks_per_clock = machine
        .active_mode()
        .clock_rate()
        .master_ticks_per_clock()
        .expect("GSW CPU rate must divide the master clock");
    run_until_toka_condition_with_clock(machine, cycles, Some(ticks_per_clock), complete)
}

fn run_until_toka_condition_with_clock(
    machine: &mut Machine,
    cycles: u64,
    frozen_ticks_per_clock: Option<u64>,
    mut complete: impl FnMut(&Machine) -> bool,
) -> (StopReason, u64) {
    const BURST_CYCLES: u64 = 5_000_000;

    assert!(cycles > 0);
    let mut requested = 0;
    let stop = loop {
        let burst = (cycles - requested).min(BURST_CYCLES);
        let result = match frozen_ticks_per_clock {
            Some(ticks_per_clock) => {
                machine.run_master_ticks(burst.saturating_mul(ticks_per_clock))
            }
            None => machine.run_until_halt_or_cycles(burst),
        };
        let stop = result.expect("run Toka-DOS machine");
        requested += burst;

        if matches!(stop, StopReason::CpuError(_)) {
            break stop;
        }
        if complete(machine) {
            break stop;
        }
        if !matches!(stop, StopReason::CycleLimit { .. }) || requested == cycles {
            break stop;
        }
    };
    (stop, requested)
}

/// Boot the committed FAT32 HDD image (no floppy): INT 19h reads LBA 0 (the
/// MBR), which chains to the partition VBR, which loads KERNEL.SYS. The kernel
/// then mounts the FAT32 partition as C: and launches the shell.
fn boot_hdd(cycles: u64) -> (Machine, StopReason, u64) {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd(izarravm_firmware::tokados_hdd_img().to_vec());
    let (stop, requested) = run_until_toka_condition(&mut machine, cycles, current_root_prompt);
    (machine, stop, requested)
}

#[test]
fn toka_scratch_exists_until_normal_drop_then_is_removed() {
    let path = {
        let scratch = TokaScratch::new("normal-drop");
        let path = scratch.path().to_owned();
        assert!(path.is_dir());
        path
    };
    assert!(!path.exists());
}

#[test]
fn toka_scratch_preserves_a_panicking_run() {
    let scratch = TokaScratch::new("panic-preserve");
    let path = scratch.path().to_owned();
    let result = std::panic::catch_unwind(move || {
        let _scratch = scratch;
        panic!("preserve this scratch directory");
    });

    assert!(result.is_err());
    assert!(path.is_dir());
    fs::remove_dir_all(&path).expect("remove preserved scratch directory");
}

#[test]
fn toka_scratch_repeated_labels_get_distinct_paths() {
    let first = TokaScratch::new("repeated-label");
    let second = TokaScratch::new("repeated-label");

    assert_ne!(first.path(), second.path());
    assert!(first.path().is_dir());
    assert!(second.path().is_dir());
}

#[path = "tokados_katea_test.rs"]
mod katea;

#[path = "tokados_cd_test.rs"]
mod cd;

#[path = "tokados_tokaemm_test.rs"]
mod tokaemm;

#[path = "tokados_sndctrl_test.rs"]
mod sndctrl;

#[path = "tokados_sndmixer_test.rs"]
mod sndmixer;

#[path = "tokados_gswmode_test.rs"]
mod gswmode;

#[path = "tokados_bootscreen_test.rs"]
mod bootscreen;
