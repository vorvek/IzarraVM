// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_machine::StopReason;

/// Boot the committed FAT32 HDD image (no floppy): INT 19h reads LBA 0 (the
/// MBR), which chains to the partition VBR, which loads KERNEL.SYS. The kernel
/// then mounts the FAT32 partition as C: and launches the shell.
fn boot_hdd(cycles: u64) -> (Machine, StopReason) {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .expect("build machine");
    machine.mount_hdd(izarravm_firmware::tokados_hdd_img().to_vec());
    let stop = machine
        .run_until_halt_or_cycles(cycles)
        .expect("run machine");
    (machine, stop)
}

#[path = "tokados_katea_test.rs"]
mod katea;

#[path = "tokados_tokaemm_test.rs"]
mod tokaemm;
