// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

#![forbid(unsafe_code)]

pub const I386DX25_TEST_ROM: &[u8] = include_bytes!("../roms/i386dx25-test.bin");
pub const I386DX25_TEST_ROM_SOURCE: &str = include_str!("../roms/i386dx25-test.asm");
/// A standalone flat-ROM guest program that finds
/// the Distira 3D card via real PCI configuration space, maps BAR0, and draws
/// one flat-shaded triangle through direct SST-1 register pokes (the same
/// wire format real DOS Glide uses), with no Glide dependency.
pub const DISTTRI_BIN: &[u8] = include_bytes!("../roms/disttri.bin");
pub const DISTTRI_SOURCE: &str = include_str!("../roms/disttri.asm");
pub const X86_BOOT_TEST_IMAGE: &[u8] = include_bytes!("../roms/boot-suite/izarravm-test.img");
pub const X86_BOOT_TEST_BOOT_SOURCE: &str = include_str!("../roms/boot-suite/boot.asm");
pub const X86_BOOT_TEST_STAGE2_SOURCE: &str = include_str!("../roms/boot-suite/stage2.asm");
pub const X86_BOOT_TEST_RESULTS_SOURCE: &str = include_str!("../roms/boot-suite/results.inc");
pub const NEURKETA_IMAGE: &[u8] = include_bytes!("../roms/neurketa/neurketa.img");
pub const NEURKETA_STAGE2_SOURCE: &str = include_str!("../roms/neurketa/neurketa-stage2.asm");
pub const HELLO_COM: &[u8] = include_bytes!("../roms/dos/hello.com");
pub const HELLO_COM_SOURCE: &str = include_str!("../roms/dos/hello.asm");
pub const ECHO_COM: &[u8] = include_bytes!("../roms/dos/echo.com");
pub const ECHO_COM_SOURCE: &str = include_str!("../roms/dos/echo.asm");
pub const TYPE_COM: &[u8] = include_bytes!("../roms/dos/type.com");
pub const TYPE_COM_SOURCE: &str = include_str!("../roms/dos/type.asm");
pub const RUNNER_COM: &[u8] = include_bytes!("../roms/dos/runner.com");
pub const RUNNER_COM_SOURCE: &str = include_str!("../roms/dos/runner.asm");
pub const MARK_COM: &[u8] = include_bytes!("../roms/dos/mark.com");
pub const MARK_COM_SOURCE: &str = include_str!("../roms/dos/mark.asm");
pub const LOADTEST_COM: &[u8] = include_bytes!("../roms/dos/loadtest.com");
pub const LOADTEST_COM_SOURCE: &str = include_str!("../roms/dos/loadtest.asm");
pub const EXIT42_COM: &[u8] = include_bytes!("../roms/dos/exit42.com");
pub const EXIT42_COM_SOURCE: &str = include_str!("../roms/dos/exit42.asm");
pub const HLTTEST_COM: &[u8] = include_bytes!("../roms/dos/hlttest.com");
pub const HLTTEST_COM_SOURCE: &str = include_str!("../roms/dos/hlttest.asm");
pub const MULTIHLT_COM: &[u8] = include_bytes!("../roms/dos/multihlt.com");
pub const MULTIHLT_COM_SOURCE: &str = include_str!("../roms/dos/multihlt.asm");
pub const XMSTEST_COM: &[u8] = include_bytes!("../roms/dos/xmstest.com");
// No XMSARENA_COM_SOURCE: the sibling XMSTEST_COM_SOURCE has no consumer, and a
// second unused const would just be more of the same.
pub const XMSARENA_COM: &[u8] = include_bytes!("../roms/dos/xmsarena.com");
pub const XMSTEST_COM_SOURCE: &str = include_str!("../roms/dos/xmstest.asm");
pub const UMBTEST_COM: &[u8] = include_bytes!("../roms/dos/umbtest.com");
pub const UMBTEST_COM_SOURCE: &str = include_str!("../roms/dos/umbtest.asm");
pub const UMBMECH_COM: &[u8] = include_bytes!("../roms/dos/umbmech.com");
pub const UMBMECH_COM_SOURCE: &str = include_str!("../roms/dos/umbmech.asm");
pub const EMSTEST_COM: &[u8] = include_bytes!("../roms/dos/emstest.com");
pub const EMSTEST_COM_SOURCE: &str = include_str!("../roms/dos/emstest.asm");
pub const EMSNONE_COM: &[u8] = include_bytes!("../roms/dos/emsnone.com");
pub const EMSNONE_COM_SOURCE: &str = include_str!("../roms/dos/emsnone.asm");
pub const MOUSETST_COM: &[u8] = include_bytes!("../roms/dos/mousetst.com");
pub const MOUSETST_COM_SOURCE: &str = include_str!("../roms/dos/mousetst.asm");
pub const SNDTST_COM: &[u8] = include_bytes!("../roms/dos/sndtst.com");
pub const SNDTST_COM_SOURCE: &str = include_str!("../roms/dos/sndtst.asm");
pub const IRQ5IP0_COM: &[u8] = include_bytes!("../roms/dos/irq5ip0.com");
pub const IRQ5IP0_COM_SOURCE: &str = include_str!("../roms/dos/irq5ip0.asm");
pub const VCPIDET_COM: &[u8] = include_bytes!("../roms/dos/vcpidet.com");
pub const VCPIDET_COM_SOURCE: &str = include_str!("../roms/dos/vcpidet.asm");
pub const VCPIMEM_COM: &[u8] = include_bytes!("../roms/dos/vcpimem.com");
pub const VCPIMEM_COM_SOURCE: &str = include_str!("../roms/dos/vcpimem.asm");
pub const EMMPROBE_COM: &[u8] = include_bytes!("../roms/dos/emmprobe.com");
pub const EMMPROBE_COM_SOURCE: &str = include_str!("../roms/dos/emmprobe.asm");
pub const EMSFRAG_COM: &[u8] = include_bytes!("../roms/dos/emsfrag.com");
pub const EMSFRAG_COM_SOURCE: &str = include_str!("../roms/dos/emsfrag.asm");
pub const VCPILOW_COM: &[u8] = include_bytes!("../roms/dos/vcpilow.com");
pub const VCPILOW_COM_SOURCE: &str = include_str!("../roms/dos/vcpilow.asm");
pub const VCPIIF_COM: &[u8] = include_bytes!("../roms/dos/vcpiif.com");
pub const VCPIIF_COM_SOURCE: &str = include_str!("../roms/dos/vcpiif.asm");
pub const VCPISW_COM: &[u8] = include_bytes!("../roms/dos/vcpisw.com");
pub const VCPISW_COM_SOURCE: &str = include_str!("../roms/dos/vcpisw.asm");
pub const GPREFLCT_COM: &[u8] = include_bytes!("../roms/dos/gpreflct.com");
pub const GPREFLCT_COM_SOURCE: &str = include_str!("../roms/dos/gpreflct.asm");
pub const GPEMUL_COM: &[u8] = include_bytes!("../roms/dos/gpemul.com");
pub const GPEMUL_COM_SOURCE: &str = include_str!("../roms/dos/gpemul.asm");
pub const GPSTORM_COM: &[u8] = include_bytes!("../roms/dos/gpstorm.com");
pub const GPSTORM_COM_SOURCE: &str = include_str!("../roms/dos/gpstorm.asm");
pub const NOINTA_COM: &[u8] = include_bytes!("../roms/dos/nointa.com");
pub const NOINTA_COM_SOURCE: &str = include_str!("../roms/dos/nointa.asm");
pub const ISRSET_COM: &[u8] = include_bytes!("../roms/dos/isrset.com");
pub const ISRSET_COM_SOURCE: &str = include_str!("../roms/dos/isrset.asm");
pub const INT0DRFL_COM: &[u8] = include_bytes!("../roms/dos/int0drfl.com");
pub const INT0DRFL_COM_SOURCE: &str = include_str!("../roms/dos/int0drfl.asm");
pub const INT88RMP_COM: &[u8] = include_bytes!("../roms/dos/int88rmp.com");
pub const INT88RMP_COM_SOURCE: &str = include_str!("../roms/dos/int88rmp.asm");
pub const PICSTALE_COM: &[u8] = include_bytes!("../roms/dos/picstale.com");
pub const PICSTALE_COM_SOURCE: &str = include_str!("../roms/dos/picstale.asm");
pub const TOKAEMM_SYS: &[u8] = include_bytes!("../roms/dos/tokaemm.sys");
pub const TOKAEMM_SYS_SOURCE: &str = include_str!("../roms/dos/tokaemm.asm");
pub const TOKACD_SYS: &[u8] = include_bytes!("../roms/dos/tokacd.sys");
pub const TOKACD_SYS_SOURCE: &str = include_str!("../roms/dos/tokacd.asm");
pub const CDTEST_COM: &[u8] = include_bytes!("../roms/dos/cdtest.com");
pub const CDTEST_COM_SOURCE: &str = include_str!("../roms/dos/cdtest.asm");
pub const CDPROT_COM: &[u8] = include_bytes!("../roms/dos/cdprot.com");
pub const CDPROT_COM_SOURCE: &str = include_str!("../roms/dos/cdprot.asm");
pub const CDAUDIO_COM: &[u8] = include_bytes!("../roms/dos/cdaudio.com");
pub const CDAUDIO_COM_SOURCE: &str = include_str!("../roms/dos/cdaudio.asm");
pub const GSWMODE_COM: &[u8] = include_bytes!("../roms/dos/gswmode.com");
pub const TOKAMOUS_COM: &[u8] = include_bytes!("../roms/dos/tokamous.com");
pub const MOUSEGFX_COM: &[u8] = include_bytes!("../roms/dos/mousegfx.com");
pub const MOUSEGFX_COM_SOURCE: &str = include_str!("../roms/dos/mousegfx.asm");
pub const UNHALT_COM: &[u8] = include_bytes!("../roms/dos/unhalt.com");
pub const UNHALT_COM_SOURCE: &str = include_str!("../roms/dos/unhalt.asm");
pub const SNDCTRL_COM: &[u8] = include_bytes!("../roms/dos/sndctrl.com");
pub const SNDCTRL_COM_SOURCE: &str = include_str!("../roms/dos/sndctrl.asm");
pub const SNDMIXER_COM: &[u8] = include_bytes!("../roms/dos/sndmixer.com");
pub const SNDMIXER_COM_SOURCE: &str = include_str!("../roms/dos/sndmixer.asm");
pub const GSWMODE_COM_SOURCE: &str = include_str!("../roms/dos/gswmode.asm");
pub const EXEHELLO_EXE: &[u8] = include_bytes!("../roms/dos/exehello.exe");
pub const EXEHELLO_EXE_SOURCE: &str = include_str!("../roms/dos/exehello.asm");
pub const RELOCCHK_EXE: &[u8] = include_bytes!("../roms/dos/relocchk.exe");
pub const RELOCCHK_EXE_SOURCE: &str = include_str!("../roms/dos/relocchk.asm");
/// A 512-byte boot sector that
/// enters Virtual-8086 mode under a hand-built monitor and signals success through
/// the unit-tester exit port. Run via `Machine::new_boot_image`.
pub const V86SPIKE_BIN: &[u8] = include_bytes!("../roms/dos/v86spike.bin");
pub const V86SPIKE_SOURCE: &str = include_str!("../roms/dos/v86spike.asm");
pub const KBD_BIOS: &[u8] = include_bytes!("../roms/kbd-bios.bin");
pub const KBD_BIOS_SOURCE: &str = include_str!("../roms/kbd-bios.asm");
pub const KBD_RESIDENT_BIOS: &[u8] = include_bytes!("../roms/kbd-resident.bin");
pub const KBD_RESIDENT_BIOS_SOURCE: &str = include_str!("../roms/kbd-resident.asm");
/// Segment the resident keyboard BIOS loads at (F000:0000). The INT 09h/16h
/// handlers run with CS set to this and use cs-relative table lookups, so the
/// installer must place the image at this segment's offset 0.
pub const KBD_RESIDENT_BIOS_SEG: u16 = 0xf000;
pub const IZARRA_BIOS: &[u8] = include_bytes!("../roms/izarra-bios.bin");
pub const IZARRA_BIOS_SOURCE: &str = include_str!("../roms/izarra-bios.asm");
pub const IZARRA_BIOS_DEFS_SOURCE: &str = include_str!("../roms/izbios-defs.inc");
pub const IZARRA_BIOS_VBEPM_SOURCE: &str = include_str!("../roms/izbios-vbepm.inc");

/// ROM offset of the VBE 2.0 protected-mode interface block that INT 10h
/// AX=4F0Ah hands out, and of the word holding its length. The BIOS is mapped
/// at `ROM_SEG` (0xF000), so the guest-visible far pointer is F000:F100.
///
/// The length is READ FROM THE ROM rather than duplicated here: `izbios-vbepm.inc`
/// emits `vbe_pm_block_end - vbe_pm_block` in the two bytes just below the block,
/// so a routine growing inside the stub cannot leave a stale constant behind.
pub const IZARRA_BIOS_VBE_PM_OFFSET: u16 = 0xf100;
/// Real-mode segment the 64 KiB BIOS shadow occupies, matching `ROM_SEG` in
/// izbios-defs.inc.
pub const IZARRA_BIOS_SEG: u16 = 0xf000;
const IZARRA_BIOS_VBE_PM_LEN_OFFSET: usize = 0xf0fe;

/// Length in bytes of the 4F0Ah block, the value the function returns in CX.
pub fn izarra_bios_vbe_pm_len() -> u16 {
    u16::from_le_bytes([
        IZARRA_BIOS[IZARRA_BIOS_VBE_PM_LEN_OFFSET],
        IZARRA_BIOS[IZARRA_BIOS_VBE_PM_LEN_OFFSET + 1],
    ])
}

/// ROM offset of the NUL-terminated OEM identification string, which
/// `VbeInfoBlock.OemStringPtr` points at. It sits immediately past the PM block
/// so that neither offset has to be written down twice.
pub fn izarra_bios_vbe_oem_string_offset() -> u16 {
    IZARRA_BIOS_VBE_PM_OFFSET + izarra_bios_vbe_pm_len()
}

/// The five code-page fonts (437, 850, 860, 863, 865), each at 8x16, 8x14, then
/// 8x8. Code-page-major: block `cp` at `cp * 9728`, sizes at 0 / 4096 / 7680.
/// The machine banks one page at a time into a 4 KB window (0xC4000) when the
/// guest writes a selector to Lotura port 0xE7; the BIOS then copies that page
/// into the VGA character generator.
pub const CODEPAGE_FONTS: &[u8] = include_bytes!("../roms/codepage-fonts.bin");

/// The izarra flash chip is 256 KiB. The board shadows only the top 64 KiB to
/// 0xF0000, exactly like a period board where the BIOS shadow is a slice of a
/// larger flash. The lower 192 KiB is reserved (room for uncompressed art, a
/// VGA option ROM, etc.) and is not CPU-addressable.
pub const IZARRA_FLASH_SIZE: usize = 256 * 1024;

static IZARRA_FLASH: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
    let mut flash = vec![0u8; IZARRA_FLASH_SIZE];
    let top = IZARRA_FLASH_SIZE - IZARRA_BIOS.len();
    flash[top..].copy_from_slice(IZARRA_BIOS);
    flash
});

/// The Toka-DOS hard-disk image: a partitioned, bootable FAT32 disk image with a
/// standard MBR, one primary FAT32-LBA partition, and a complete Toka-DOS system
/// (KERNEL.SYS, COMMAND.COM, CONFIG.SYS, AUTOEXEC.BAT, HELLO.TXT). Mount with
/// `Machine::mount_hdd`; INT 19h boots LBA 0 (the MBR), which chains to the
/// partition's FAT32 VBR. Built by scripts/build-freedos-hdd-image.py.
pub const TOKADOS_HDD_IMG: &[u8] = include_bytes!("../roms/tokados-hdd.img");

pub const I386DX25_TEST_ROM_SIZE: usize = 64 * 1024;
pub const X86_BOOT_TEST_IMAGE_SIZE: usize = 1440 * 1024;
pub const X86_BOOT_RESULT_BLOCK_ADDRESS: usize = 0x9000;
pub const X86_BOOT_RESULT_MAGIC: &[u8; 4] = b"VDTS";

pub fn test_rom() -> &'static [u8] {
    I386DX25_TEST_ROM
}

pub fn kbd_bios() -> &'static [u8] {
    KBD_BIOS
}

pub fn kbd_resident_bios() -> &'static [u8] {
    KBD_RESIDENT_BIOS
}

pub fn izarra_bios() -> &'static [u8] {
    &IZARRA_FLASH
}

pub fn boot_test_image() -> &'static [u8] {
    X86_BOOT_TEST_IMAGE
}

/// The Neurketa benchmark boot image: a 1.44 MiB floppy that boots a 16-bit
/// loader plus the Sieve payload. Run with `Machine::new_boot_image`, preload
/// the selector with `Machine::set_bench_selector`, and read the results back
/// with `Machine::bench_iterations` / `bench_aux` after the `TestExit` stop.
pub fn neurketa_image() -> &'static [u8] {
    NEURKETA_IMAGE
}

pub fn tokados_hdd_img() -> &'static [u8] {
    TOKADOS_HDD_IMG
}

pub fn hello_com() -> &'static [u8] {
    HELLO_COM
}

pub fn echo_com() -> &'static [u8] {
    ECHO_COM
}

/// The `--katea-run` harness: EXECs the named program, captures its exit code, and
/// reports it to the unit-tester exit port. Overlaid onto C: as `RUNNER.COM`.
pub fn runner_com() -> &'static [u8] {
    RUNNER_COM
}

/// Places a boot-profiler phase boundary and returns. Appended to AUTOEXEC.BAT
/// by `--headless-boot-profile` to signal that Toka-DOS has finished loading.
pub fn mark_com() -> &'static [u8] {
    MARK_COM
}

/// The boot profiler's hard-drive load workload: reads a host-folder file end to
/// end, bracketed by phase marks, then stops the machine.
pub fn loadtest_com() -> &'static [u8] {
    LOADTEST_COM
}

/// A test program that terminates with DOS exit code 42; the katea-run e2e fixture.
pub fn exit42_com() -> &'static [u8] {
    EXIT42_COM
}

/// Enables interrupts, runs a real HLT, then exits with code 1: the katea-run e2e
/// fixture for guest HLT under TOKAEMM's V86 monitor. HLT is privileged on real
/// 386+ (CPL != 0 -> #GP(0)); a V86 task is always CPL 3, so this traps into
/// TOKAEMM, which emulates the guest's halt with a real ring-0 `sti; hlt` and
/// resumes the guest past the F4 byte once an IRQ wakes it. A non-1 exit or a
/// stop other than `TestExit`/`DosExit` means that round trip broke.
pub fn hlttest_com() -> &'static [u8] {
    HLTTEST_COM
}

/// Like `hlttest_com`, but loops a real HLT five times before exiting with code
/// 7. Catches drift across repeated guest halts (e.g. a corrupted saved
/// register or a stack-depth leak in TOKAEMM's HLT emulation that only shows up
/// on the second or later wake).
pub fn multihlt_com() -> &'static [u8] {
    MULTIHLT_COM
}

/// The XMS round-trip fixture: install-check, get entry, version,
/// alloc a 64 KB EMB, lock, move a pattern conventional->EMB->conventional, verify,
/// unlock, free — then signal 0xA5 (success) via the unit-tester exit port. Runs in
/// V86 under TOKAEMM; a non-0xA5 exit code names the step that broke.
pub fn xmstest_com() -> &'static [u8] {
    XMSTEST_COM
}

/// The XMS arena-SHAPE fixture: 08h must report the largest free block
/// separately from the total, and a 1 KB request must cost 1 KB rather than a
/// whole page. Split out of `xmstest_com` so a regression in the round trip
/// cannot leave these two assertions unrun. Signals 0xA5 on success; 0xEF and
/// 0xF0 are the assertions, every other code is setup.
pub fn xmsarena_com() -> &'static [u8] {
    XMSARENA_COM
}

/// The UMB fixture: with DOS=UMB, set the high-first allocation
/// strategy, AH=48h-allocate a block, assert it landed in upper memory (segment
/// 0xC800 or above) with real RAM behind it (write/read a pattern) — proving
/// TOKAEMM page-mapped extended RAM into the upper holes and DOS=UMB consumed it.
/// Runs in V86; signals 0xA5 (or a 0xEn step code) via the unit-tester exit port.
pub fn umbtest_com() -> &'static [u8] {
    UMBTEST_COM
}

/// The EMS fixture, which needs TOKAEMM loaded with the RAM argument:
/// version, frame segment, page counts, allocate, then map logical pages
/// through the frame slots writing distinct patterns and reading them back
/// through other slots — proving the runtime page remap through the paged
/// frame. Signals 0xA5 (or a 0xEn step code) via the unit-tester exit port.
pub fn emstest_com() -> &'static [u8] {
    EMSTEST_COM
}

/// The mouse-under-V86 fixture: after LH TOKAMOUS, polls the INT 33h
/// wheel counter for a host-injected detent — proving slave IRQ12 -> vector
/// 0x74 -> INT 74h reflection under the monitor. Signals 0xA5 / 0xEn.
pub fn mousetst_com() -> &'static [u8] {
    MOUSETST_COM
}

/// The SB16-IRQ5-under-V86 fixture: hooks INT 0Dh, resets the DSP,
/// and requests immediate 8-bit IRQs (DSP 0xF2) inside a CLI/STI-dense loop —
/// IRQ5 deliveries interleave with #GPs on the shared vector 13, exercising
/// the monitor's discriminator. Signals 0xA5 / 0xEn.
pub fn sndtst_com() -> &'static [u8] {
    SNDTST_COM
}

/// The IRQ5-at-IP==0 discriminator regression fixture (V86 trap tax): arms SB16
/// auto-init DMA for a continuous IRQ5 stream, then parks on a `jmp $` at
/// seg:0000 so those IRQ5 frames carry return-IP 0. Under the old
/// error-code-VALUE discriminator this was the ambiguous case (opcode peek +
/// PIC probe); the frame-ORIGIN basis decides it from the EFLAGS.VM slot at
/// any IP, and this fixture pins that.
pub fn irq5ip0_com() -> &'static [u8] {
    IRQ5IP0_COM
}

/// The default-off EMS fixture (bare DEVICE=C:\DOS\TOKAEMM.SYS): the
/// manager must answer INT 67h frameless — present, version 4.0, zero pages,
/// 41h returns 80h, allocation refused with 87h. Signals 0xA5 / 0xEn.
pub fn emsnone_com() -> &'static [u8] {
    EMSNONE_COM
}

/// The VCPI presence fixture (bare DEVICE=C:\DOS\TOKAEMM.SYS): INT 67h
/// AX=DE00h must answer AH=0/BX=0100h (VCPI 1.0 present, even frameless), a
/// not-yet-implemented subfunction must answer 8Fh, untouched registers must
/// survive, and plain EMS must keep answering on the shared vector. Signals
/// 0xA5 / 0xEn.
pub fn vcpidet_com() -> &'static [u8] {
    VCPIDET_COM
}

/// The VCPI query/page-pool fixture (bare DEVICE=C:\DOS\TOKAEMM.SYS):
/// exercises DE02-DE0B — pool count/alloc/free round-trip with 12-LSB
/// masking, bad-free and double-free rejection, the V86 page-table query,
/// CR0, the debug-register array, and the 8259 report/record round-trip.
/// Signals 0xA5 / 0xEn.
pub fn vcpimem_com() -> &'static [u8] {
    VCPIMEM_COM
}

/// The DOS/16M pool-overlap probe fixture.
pub fn emmprobe_com() -> &'static [u8] {
    EMMPROBE_COM
}

/// EMS allocation against a deliberately fragmented pool.
pub fn emsfrag_com() -> &'static [u8] {
    EMSFRAG_COM
}

/// Small-memory XMS/VCPI bounds and empty-pool fixture.
pub fn vcpilow_com() -> &'static [u8] {
    VCPILOW_COM
}

/// The VCPI DE01 fixture (bare DEVICE=C:\DOS\TOKAEMM.SYS): validates the
/// Get Protected Mode Interface call's V86-observable outputs — the 0x110-
/// entry page-table copy (identity mappings, software bits 9-11 cleared,
/// exact write extent, DI advance), the three furnished GDT descriptors,
/// and the in-segment PM entry offset. Signals 0xA5 / 0xEn.
pub fn vcpiif_com() -> &'static [u8] {
    VCPIIF_COM
}

/// The VCPI mode-switch fixture: a minimal real VCPI client. DE01
/// interface setup, DE0C into 16-bit protected mode under its own
/// CR3/GDT/TSS, PM far-calls to the server entry (DE03 vs the V86 baseline,
/// DE04/DE05 round-trip), DE0C back to V86, marker-register and
/// pool-balance verification. Signals 0xA5 / 0xEn.
pub fn vcpisw_com() -> &'static [u8] {
    VCPISW_COM
}

/// The V86 #GP-reflection fixture hooks INT 0Dh, executes an o32
/// LGDT (the literal DOS16M startup shape), and verifies the monitor
/// reflects the fault to the guest handler with fault-IP semantics and that
/// skip-and-resume works. Signals 0xA5 / 0xEn.
pub fn gpreflct_com() -> &'static [u8] {
    GPREFLCT_COM
}

/// The V86 privileged-0F emulation fixture (386MAX-surface port): executes
/// MOV r32,CR0/CR3/CR2, MOV CR0,r32 (PE|PG forced), CLTS, and LMSW from V86
/// (all #GP at CPL 3) and verifies the monitor emulates them transparently
/// instead of reflecting a fault. Signals 0xA5 / 0xEn.
pub fn gpemul_com() -> &'static [u8] {
    GPEMUL_COM
}

/// The ring-0 #GP diagnostic fixture: a minimal VCPI client whose PM->V86
/// DE0C return frame carries an EIP above 0xFFFF, so the monitor's own
/// IRETD faults #GP(0) at ring 0 (the stage-1 G1 fault-storm iteration 0).
/// The monitor must exit through its ring-0 #GP diagnostic (0xD3), not
/// storm. The fixture itself signals only failure codes (0xE1/0xE2/0xE5).
pub fn gpstorm_com() -> &'static [u8] {
    GPSTORM_COM
}

/// The no-INTA-under-a-guest-CLI fixture: a V86 guest CLIs, polls the master
/// IRR through OCW3 0x0A until IR0 is REQUESTED (so the read below cannot be
/// early), then reads the ISR. Signals 0xA5 when the ISR is empty -- the
/// request outstanding, unacknowledged -- 0xE1 when something INTA'd on the
/// guest's behalf, 0xD1 when no request ever appeared.
pub fn nointa_com() -> &'static [u8] {
    NOINTA_COM
}

/// The E10 rule as a regression guard: the guest's own IVT[8] handler must see
/// its line IN SERVICE (master ISR bit 0 set) when it runs, before any EOI --
/// the state DJGPP's shared IRQ wrapper probes. Signals 0xA5 / 0xE1, with
/// 0xD1/0xD2 for the two setup steps.
pub fn isrset_com() -> &'static [u8] {
    ISRSET_COM
}

/// Routing row 1: a guest `INT 0Dh` reaches IVT[0x0D] as a software interrupt
/// both at the DOS-default PIC bases and after a VCPI DE0B moves the master off
/// base 8 (where vector 13 stops being IRQ5). Signals 0xA5 / 0xE1 / 0xE2, with
/// 0xD1/0xD2 for setup.
pub fn int0drfl_com() -> &'static [u8] {
    INT0DRFL_COM
}

/// Routing row 2: with the master remapped to 0x88 and IRQ0 genuinely in
/// service, a guest `INT 88h` reaches IVT[0x88] as a software interrupt and
/// nothing EOIs the in-service line for it. Signals 0xA5 / 0xE1 / 0xE2, with
/// 0xD1/0xD2 for setup.
pub fn int88rmp_com() -> &'static [u8] {
    INT88RMP_COM
}

/// Routing row 3, the stale-bookkeeping construction: DE0B the master to 0x88,
/// then put the CHIP back to base 8 through a direct (untrapped) ICW sequence,
/// then let a real IRQ0 fire. The arriving vector must decide, so IVT[8] runs.
/// Signals 0xA5 / 0xE1 (IVT[0x88] ran through the stale cache), with 0xD1/0xD2
/// for setup.
pub fn picstale_com() -> &'static [u8] {
    PICSTALE_COM
}

/// GSWMODE.COM: a guest tool that retargets the GSW-586's live CPU speed at
/// runtime by writing the Lotura mode register (port 0xE1). `GSWMODE
/// 386-slow|386|486|586` switches without changing the BIOS default. The
/// removed `286` name reports how to migrate. Ships on the Toka-DOS image
/// (see build-freedos-hdd-image.py).
pub fn gswmode_com() -> &'static [u8] {
    GSWMODE_COM
}

/// UNHALT.COM: makes the BIOS INT 16h keyboard wait spin instead of halting.
///
/// The wait halts by default (kbd-bios-core.inc), because a spinning wait is
/// interpreted guest code and DOS programs block there constantly. A halt is
/// not identical to a spin, though, and this is the escape for the difference:
/// a program that masks IRQ0 and IRQ1 before blocking, or one that expects
/// guest time to advance smoothly across the wait rather than in 18.2 Hz steps.
///
/// Not a TSR. The flag is a BDA byte (0040:00B4) that the ROM re-reads on every
/// wait, so the tool sets it and exits; `UNHALT /H` puts it back. Ships on the
/// Toka-DOS image (see build-freedos-hdd-image.py).
pub fn unhalt_com() -> &'static [u8] {
    UNHALT_COM
}

/// SNDCTRL.COM: the ReSonique II sound-card setup tool, run from inside the
/// guest.
///
/// Moves the SB16 and AD1848 IRQ/DMA assignment with a text-mode interface (or
/// from the command line), writes it to the live hardware, persists it in the
/// CMOS block at 0x1B-0x21 with a refreshed NVRAM checksum, and rewrites the
/// `BLASTER` line in both the master environment and `C:\AUTOEXEC.BAT`.
///
/// This exists because DOS titles split into two populations that cannot both
/// be satisfied by one default: the ones that hardwire an IRQ (usually 7) and
/// the ones that read `BLASTER`. On real hardware you moved a jumper or ran the
/// card's own setup utility; this is that utility. Ships on the Toka-DOS image
/// in `C:\DOS` (see build-freedos-hdd-image.py).
pub fn sndctrl_com() -> &'static [u8] {
    SNDCTRL_COM
}

/// SNDMIXER.COM, the card's volume mixer: six vertical faders over the CT1745
/// mixer (master, FM, wave, CD, wavetable MIDI and the PC speaker), a config
/// file so the levels survive a power cycle, and a `/CFG path /S` boot line
/// that restores them without printing anything.
///
/// SNDCTRL.COM decides where the card lives; this decides how loud it is. The
/// two are one setup screen split in half, and share a palette and key map.
/// Ships on the Toka-DOS image in `C:\DOS` (see build-freedos-hdd-image.py).
pub fn sndmixer_com() -> &'static [u8] {
    SNDMIXER_COM
}

/// The direct UMB mechanism fixture: drives XMS 10h/11h/12h without
/// DOS=UMB) to exercise the allocator paths the DOS=UMB e2e doesn't reach — the
/// too-big probe (B0h + largest), alloc, grow, release, and reuse-after-free —
/// plus a write/read of the paged RAM. Signals 0xA5 (or a 0xEn step code).
pub fn umbmech_com() -> &'static [u8] {
    UMBMECH_COM
}

/// TOKAEMM.SYS is a bespoke memory-manager character device. Its INIT runs
/// at SYSINIT, builds a load-relative protected-mode + paging + ring-0 monitor
/// environment in its own resident memory, then IRETDs the running kernel into
/// virtual-8086 mode — so the rest of DOS boots and runs virtualized under the
/// monitor. It stays resident permanently. Overlaid into C:\DOS and loaded via
/// `DEVICE=C:\DOS\TOKAEMM.SYS`.
pub fn tokaemm_sys() -> &'static [u8] {
    TOKAEMM_SYS
}

/// TOKACD.SYS is the Toka-DOS MSCDEX hardware driver for Izarra's fixed
/// secondary-master ATAPI CD-ROM. It uses bounded polling PIO and exposes one
/// unit named TOKACD01.
pub fn tokacd_sys() -> &'static [u8] {
    TOKACD_SYS
}

/// Guest fixture for the IzarraCD ROM extension's DOS surface. It checks the
/// INT 2Fh install and device list, verifies the ROM device header by name,
/// then reads a known file from D: through DOS.
pub fn cdtest_com() -> &'static [u8] {
    CDTEST_COM
}

/// TOKAMOUS.COM: the Toka-DOS INT 33h PS/2 mouse driver TSR, built from
/// `toka-dos/tools/tokamous.asm` by `toka-dos/build-freedos.ps1`. Ships on the
/// Toka-DOS image (see build-freedos-hdd-image.py, which takes this committed
/// binary on both of its paths) and is mounted directly by the driver tests.
pub fn tokamous_com() -> &'static [u8] {
    TOKAMOUS_COM
}

/// Guest fixture for TOKAMOUS's mode 13h graphics cursor: draw, hide, move,
/// a vertical range past the mode's height, the INT 10h mode-set hook, and a
/// fn 09h user shape. Exits with 0 or the first failing step.
pub fn mousegfx_com() -> &'static [u8] {
    MOUSEGFX_COM
}

/// Guest fixture that calls the CD driver's request-packet entry points
/// directly (through the IzarraCD ROM header's strategy/interrupt stubs).
pub fn cdprot_com() -> &'static [u8] {
    CDPROT_COM
}

/// Guest fixture for CD play, pause, resume, seek, and read ordering through
/// the same request-packet entries.
pub fn cdaudio_com() -> &'static [u8] {
    CDAUDIO_COM
}

pub fn exehello_exe() -> &'static [u8] {
    EXEHELLO_EXE
}

/// Guest fixture for the kernel's .EXE relocation loop: 130 self-checking
/// fixups (four full 32-entry spans plus a 2-entry remainder) and an
/// unrelocated canary. Exits 42 when every fixup landed exactly once.
pub fn relocchk_exe() -> &'static [u8] {
    RELOCCHK_EXE
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuiteRecordStatus {
    Begin,
    Pass,
    Fail,
    Measure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuiteRecord {
    pub status: SuiteRecordStatus,
    pub name: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuiteResults {
    pub version: u16,
    pub declared_record_count: u16,
    pub payload_len: u16,
    pub checksum: u16,
    pub records: Vec<SuiteRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuiteParseError {
    MissingMagic,
    TruncatedHeader,
    TruncatedPayload,
    InvalidUtf8,
    ChecksumMismatch { expected: u16, actual: u16 },
    UnknownRecordStatus(String),
}

impl std::fmt::Display for SuiteParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingMagic => formatter.write_str("missing boot-suite result magic"),
            Self::TruncatedHeader => formatter.write_str("truncated boot-suite result header"),
            Self::TruncatedPayload => formatter.write_str("truncated boot-suite result payload"),
            Self::InvalidUtf8 => formatter.write_str("boot-suite result payload is not UTF-8"),
            Self::ChecksumMismatch { expected, actual } => write!(
                formatter,
                "boot-suite result checksum mismatch: expected {expected:#06x}, got {actual:#06x}"
            ),
            Self::UnknownRecordStatus(status) => {
                write!(formatter, "unknown boot-suite record status '{status}'")
            }
        }
    }
}

impl std::error::Error for SuiteParseError {}

pub fn parse_result_block(memory: &[u8]) -> Result<SuiteResults, SuiteParseError> {
    if memory.len() < X86_BOOT_RESULT_BLOCK_ADDRESS + 12 {
        return Err(SuiteParseError::TruncatedHeader);
    }

    let block = &memory[X86_BOOT_RESULT_BLOCK_ADDRESS..];
    if &block[0..4] != X86_BOOT_RESULT_MAGIC {
        return Err(SuiteParseError::MissingMagic);
    }

    let version = read_u16(&block[4..6])?;
    let declared_record_count = read_u16(&block[6..8])?;
    let payload_len = read_u16(&block[8..10])?;
    let checksum = read_u16(&block[10..12])?;
    let payload_start = 12;
    let payload_end = payload_start + usize::from(payload_len);
    if block.len() < payload_end {
        return Err(SuiteParseError::TruncatedPayload);
    }

    let payload = &block[payload_start..payload_end];
    let actual = additive_checksum(payload);
    if actual != checksum {
        return Err(SuiteParseError::ChecksumMismatch {
            expected: checksum,
            actual,
        });
    }

    let text = std::str::from_utf8(payload).map_err(|_| SuiteParseError::InvalidUtf8)?;
    Ok(SuiteResults {
        version,
        declared_record_count,
        payload_len,
        checksum,
        records: parse_records(text)?,
    })
}

pub fn parse_serial_records(text: &str) -> Result<Vec<SuiteRecord>, SuiteParseError> {
    parse_records(text)
}

fn parse_records(text: &str) -> Result<Vec<SuiteRecord>, SuiteParseError> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_record)
        .collect()
}

fn parse_record(line: &str) -> Result<SuiteRecord, SuiteParseError> {
    let mut parts = line.splitn(3, ' ');
    let status = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default().to_owned();
    let value = parts.next().map(str::to_owned);
    let status = match status {
        "BEGIN" => SuiteRecordStatus::Begin,
        "PASS" => SuiteRecordStatus::Pass,
        "FAIL" => SuiteRecordStatus::Fail,
        "MEASURE" => SuiteRecordStatus::Measure,
        other => return Err(SuiteParseError::UnknownRecordStatus(other.to_owned())),
    };

    Ok(SuiteRecord {
        status,
        name,
        value,
    })
}

fn read_u16(bytes: &[u8]) -> Result<u16, SuiteParseError> {
    let bytes: [u8; 2] = bytes
        .try_into()
        .map_err(|_| SuiteParseError::TruncatedHeader)?;
    Ok(u16::from_le_bytes(bytes))
}

fn additive_checksum(bytes: &[u8]) -> u16 {
    bytes
        .iter()
        .fold(0u16, |sum, byte| sum.wrapping_add(u16::from(*byte)))
}

#[cfg(test)]
#[path = "firmware_test.rs"]
mod tests;
