// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_machine::Machine;

/// Keep a real-BIOS fixture running after POST under the fail-closed boot policy.
pub fn mount_idle_boot_floppy(machine: &mut Machine) {
    let mut image = vec![0u8; 1_474_560];
    image[..3].copy_from_slice(&[0xfb, 0xeb, 0xfd]); // sti; jmp $
    image[510] = 0x55;
    image[511] = 0xaa;
    machine.mount_floppy(image).unwrap();
}
