#!/usr/bin/env python3
# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only

"""Build a bootable, partitioned FAT32 Toka-DOS hard-disk image from the built
components. Authoring-only; emits crates/izarravm-firmware/roms/tokados-hdd.img.

Layout:
  LBA 0                         MBR (mbr.bin) + a 4-entry partition table with one
                                primary FAT32-LBA (type 0x0C) bootable partition.
  PART_START (2048, 1 MiB)      FAT32 VBR (fat32lba.bin) with the FAT32 BPB stamped
                                over it; HiddSec = PART_START so the VBR's LBAs are
                                disk-absolute. FSInfo at +1, backup boot at +6.
  PART_START + reserved         FAT #1, then FAT #2.
  data region                   root directory (cluster 2) holds KERNEL.SYS FIRST
                                (hidden; FreeDOS requires it in the root), then
                                CONFIG.SYS, AUTOEXEC.BAT, LICENSE.TXT, and a DOS
                                subdirectory. C:\\DOS holds COMMAND.COM and every
                                command-line tool, so the root stays uncluttered.

The FAT32 BPB field values follow the same fatgen103 math as
crates/izarravm-machine/src/fat32.rs (the Rust reference): DskTableFAT32 cluster
size and the FATSz32 computation. AtaDisk derives 16 heads x 63 sectors/track, so
the MBR partition CHS start/end are filled to match that geometry."""
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from license_txt import build_license_txt

# --- disk + partition geometry -------------------------------------------------
BPS = 512
HEADS = 16          # AtaDisk fixed geometry
SPT = 63            # AtaDisk fixed geometry
PART_START = 2048   # 1 MiB-aligned partition start LBA (and HiddSec)
# Total disk size. 48 MiB gives a comfortably-valid FAT32 (>= 65525 clusters with
# 1-sector clusters) while staying tiny enough to commit. The partition fills the
# rest of the disk after the 1 MiB gap.
DISK_SECTORS = 48 * 1024 * 1024 // BPS  # 98304 sectors
PART_SECTORS = DISK_SECTORS - PART_START

# --- FAT32 BPB constants (mirror fat32.rs) ------------------------------------
RESERVED_SECTORS = 32
NUM_FATS = 2
ROOT_CLUSTER = 2
FSINFO_SECTOR = 1
BACKUP_BOOT_SECTOR = 6
FAT32_EOC = 0x0FFFFFFF
MIN_FAT32_CLUSTERS = 65525
PART_TYPE_FAT32_LBA = 0x0C


def sectors_per_cluster(total_sectors):
    # fatgen103 DskTableFAT32 (same cutoffs as fat32.rs::sectors_per_cluster).
    if total_sectors <= 66_600:
        raise ValueError("partition too small for FAT32")
    if total_sectors <= 532_480:
        return 1
    if total_sectors <= 16_777_216:
        return 8
    if total_sectors <= 33_554_432:
        return 16
    if total_sectors <= 67_108_864:
        return 32
    return 64


def fat_size_sectors(total_sectors, spc):
    # Match the FreeDOS kernel's CalculateFATData (initdisk.c) EXACTLY, not the MS
    # fatgen103 formula. The kernel computes a *default* BPB from the partition size
    # and uses it until bldbpb reads our VBR; if our on-disk FAT geometry disagrees
    # with that default, the two views of the data region differ. divisor here is
    # (bytes/sector / 4) * spc + nfats; fatgen103's (256*spc+nfats)/2 gives a
    # different (larger) value (746 vs 741 for this volume).
    fatdata = total_sectors - RESERVED_SECTORS
    fatentpersec = BPS // 4
    divisor = fatentpersec * spc + NUM_FATS
    return (fatdata + (2 * spc + divisor - 1)) // divisor


def lba_to_chs(lba):
    """Pack an LBA into the 3-byte CHS field of a partition entry, using the
    16x63 geometry AtaDisk derives. Cylinders above 1023 are clamped to the
    all-ones (0xFE 0xFF 0xFF) "use LBA" marker, the standard MBR convention."""
    cyl = lba // (HEADS * SPT)
    rem = lba % (HEADS * SPT)
    head = rem // SPT
    sect = rem % SPT + 1
    if cyl > 1023:
        return bytes([0xFE, 0xFF, 0xFF])
    c_hi = (cyl >> 8) & 0x03
    return bytes([head & 0xFF, ((c_hi << 6) | (sect & 0x3F)) & 0xFF, cyl & 0xFF])


def name11(fn):
    base, _, ext = fn.partition(".")
    assert len(base) <= 8 and len(ext) <= 3, f"name not 8.3: {fn}"
    return (base.upper().ljust(8) + ext.upper().ljust(3)).encode("ascii")


def stamp_fat32_bpb(vbr, geo, part_start):
    """Stamp the FAT32 BPB over the fat32lba.bin VBR, keeping its boot code.
    Field offsets/values mirror fat32.rs::fat32_boot_sector, except HiddSec is the
    partition start (this is a partition, not a superfloppy) and BS_DrvNum=0x80."""
    vbr[0x03:0x0B] = b"MSWIN4.1"                  # OEM (fatgen103 recommendation)
    struct.pack_into("<H", vbr, 0x0B, BPS)        # bytes/sector
    vbr[0x0D] = geo["spc"]                         # sectors/cluster
    struct.pack_into("<H", vbr, 0x0E, RESERVED_SECTORS)
    vbr[0x10] = NUM_FATS
    struct.pack_into("<H", vbr, 0x11, 0)          # RootEntCnt: 0 on FAT32
    struct.pack_into("<H", vbr, 0x13, 0)          # TotSec16: 0 on FAT32
    vbr[0x15] = 0xF8                               # media: fixed disk
    struct.pack_into("<H", vbr, 0x16, 0)          # FATSz16: 0 on FAT32
    struct.pack_into("<H", vbr, 0x18, SPT)        # sectors/track (CHS, cosmetic)
    struct.pack_into("<H", vbr, 0x1A, HEADS)      # heads (CHS, cosmetic)
    struct.pack_into("<I", vbr, 0x1C, part_start)  # HiddSec = partition start LBA
    struct.pack_into("<I", vbr, 0x20, geo["total_sectors"])  # TotSec32
    # FAT32 extended BPB.
    struct.pack_into("<I", vbr, 0x24, geo["fatsz"])  # BPB_FATSz32
    struct.pack_into("<H", vbr, 0x28, 0)          # ExtFlags: FAT mirroring active
    struct.pack_into("<H", vbr, 0x2A, 0)          # FSVer 0.0
    struct.pack_into("<I", vbr, 0x2C, ROOT_CLUSTER)
    struct.pack_into("<H", vbr, 0x30, FSINFO_SECTOR)
    struct.pack_into("<H", vbr, 0x32, BACKUP_BOOT_SECTOR)
    vbr[0x40] = 0x80                              # BS_DrvNum (boot32lb stores DL here too)
    vbr[0x42] = 0x29                              # BS_BootSig
    struct.pack_into("<I", vbr, 0x43, 0x32303236)  # BS_VolID
    vbr[0x47:0x52] = b"TOKA-DOS   "               # BS_VolLab (11)
    vbr[0x52:0x5A] = b"FAT32   "                  # BS_FilSysType (8)
    assert vbr[0x1FE] == 0x55 and vbr[0x1FF] == 0xAA, "VBR boot signature missing"


def fsinfo_sector(free_count, next_free):
    s = bytearray(BPS)
    struct.pack_into("<I", s, 0, 0x41615252)      # FSI_LeadSig
    struct.pack_into("<I", s, 484, 0x61417272)    # FSI_StrucSig
    struct.pack_into("<I", s, 488, free_count)
    struct.pack_into("<I", s, 492, next_free)
    struct.pack_into("<I", s, 508, 0xAA550000)    # FSI_TrailSig
    return s


def extract_from_image(img):
    """Pull the MBR, VBR, and root files back out of a built image — the python
    mirror of katea_volume::extract_system_payload. Lets the image be rebuilt
    (config/payload changes) without the gitignored Open Watcom build artifacts:
    the binaries round-trip byte-identical from the previous committed image."""
    part_off = PART_START * BPS
    vbr = img[part_off:part_off + 512]
    spc = vbr[0x0D]
    reserved = struct.unpack_from("<H", vbr, 0x0E)[0]
    nfats = vbr[0x10]
    fatsz = struct.unpack_from("<I", vbr, 0x24)[0]
    root_clu = struct.unpack_from("<I", vbr, 0x2C)[0]
    fat_off = part_off + reserved * BPS
    data_off = part_off + (reserved + nfats * fatsz) * BPS
    cluster_bytes = spc * BPS

    def fat_entry(c):
        return struct.unpack_from("<I", img, fat_off + c * 4)[0] & 0x0FFFFFFF

    def chain_bytes(first, size):
        out = bytearray()
        c = first
        # Bound the walk like the Rust extractor: a corrupt/cyclic FAT must
        # fail loudly, never under-read or spin.
        for _ in range(len(img) // BPS):
            if len(out) >= size or not 2 <= c < 0x0FFFFFF8:
                break
            off = data_off + (c - 2) * cluster_bytes
            out += img[off:off + cluster_bytes]
            c = fat_entry(c)
        assert len(out) >= size, f"FAT chain shorter than the directory size ({len(out)} < {size})"
        return bytes(out[:size])

    # Walk a directory's cluster chain, flattening files by bare 8.3 name and
    # recursing one level into subdirectories (the C:\DOS system folder). Files in
    # different directories have distinct names here, so flattening is unambiguous.
    files = []

    def walk_dir(dir_clu):
        c = dir_clu
        while 2 <= c < 0x0FFFFFF8:
            off = data_off + (c - 2) * cluster_bytes
            for e in range(0, cluster_bytes, 32):
                de = img[off + e:off + e + 32]
                if de[0] == 0x00:
                    return  # no further entries in this directory
                if de[0] == 0xE5 or de[11] == 0x0F or de[11] & 0x08:
                    continue  # deleted, LFN fragment, or volume label
                base = de[0:8].decode("ascii").rstrip()
                ext = de[8:11].decode("ascii").rstrip()
                name = f"{base}.{ext}" if ext else base
                first = (struct.unpack_from("<H", de, 0x14)[0] << 16) | \
                    struct.unpack_from("<H", de, 0x1A)[0]
                if de[11] & 0x10:  # subdirectory
                    if base not in (".", ".."):
                        walk_dir(first)
                    continue
                size = struct.unpack_from("<I", de, 0x1C)[0]
                files.append((name, chain_bytes(first, size)))
            c = fat_entry(c)

    walk_dir(root_clu)
    return bytearray(img[0:512]), bytearray(vbr), dict(files)


def main(check: bool = False) -> int:
    here = os.path.dirname(os.path.abspath(__file__))
    repo = os.path.dirname(here)
    kdir = os.path.join(repo, "toka-dos", "freedos", "kernel")
    fcdir = os.path.join(repo, "toka-dos", "freedos", "freecom")
    movedir = os.path.join(repo, "toka-dos", "freedos", "move", "src")
    sortdir = os.path.join(repo, "toka-dos", "freedos", "sort", "src")
    memdir = os.path.join(repo, "toka-dos", "freedos", "mem", "source")
    attribdir = os.path.join(repo, "toka-dos", "freedos", "attrib")
    choicedir = os.path.join(repo, "toka-dos", "freedos", "choice", "src")
    moredir = os.path.join(repo, "toka-dos", "freedos", "more", "src")
    finddir = os.path.join(repo, "toka-dos", "freedos", "find", "src")
    labeldir = os.path.join(repo, "toka-dos", "freedos", "label", "src")
    deltreedir = os.path.join(repo, "toka-dos", "freedos", "deltree")
    xcopydir = os.path.join(repo, "toka-dos", "tools-src", "xcopy")
    editdir = os.path.join(repo, "toka-dos", "tools-src", "edit")
    out = os.path.join(repo, "crates", "izarravm-firmware", "roms", "tokados-hdd.img")

    if os.path.exists(os.path.join(kdir, "bin", "kernel.sys")):
        # Full-build path: fresh artifacts from toka-dos/build-freedos.ps1.
        mbr = bytearray(open(os.path.join(kdir, "boot", "mbr.bin"), "rb").read())
        vbr = bytearray(open(os.path.join(kdir, "boot", "fat32lba.bin"), "rb").read())
        kernel = open(os.path.join(kdir, "bin", "kernel.sys"), "rb").read()
        shell = open(os.path.join(fcdir, "command.com"), "rb").read()
        tokamous = open(
            os.path.join(repo, "toka-dos", "build-freedos-tokamous.com"), "rb"
        ).read()
        izcdex = open(
            os.path.join(repo, "toka-dos", "build-freedos-izcdex.com"), "rb"
        ).read()
        move = open(os.path.join(movedir, "move.exe"), "rb").read()
        sort = open(os.path.join(sortdir, "sort.exe"), "rb").read()
        mem = open(os.path.join(memdir, "mem.exe"), "rb").read()
        attrib = open(os.path.join(attribdir, "attrib.exe"), "rb").read()
        choice = open(os.path.join(choicedir, "choice.exe"), "rb").read()
        more = open(os.path.join(moredir, "more.exe"), "rb").read()
        find = open(os.path.join(finddir, "find.exe"), "rb").read()
        label = open(os.path.join(labeldir, "label.exe"), "rb").read()
        deltree = open(os.path.join(deltreedir, "deltree.com"), "rb").read()
        xcopy = open(os.path.join(xcopydir, "xcopy.exe"), "rb").read()
        edit = open(os.path.join(editdir, "edit.exe"), "rb").read()
    else:
        # From-image path: source the binaries from the current committed image.
        prev = open(out, "rb").read()
        mbr, vbr, prev_files = extract_from_image(prev)
        kernel = prev_files["KERNEL.SYS"]
        shell = prev_files["COMMAND.COM"]
        tokamous = prev_files["TOKAMOUS.COM"]
        izcdex = prev_files["IZCDEX.COM"]
        move = prev_files["MOVE.EXE"]
        sort = prev_files["SORT.EXE"]
        mem = prev_files["MEM.EXE"]
        attrib = prev_files["ATTRIB.EXE"]
        choice = prev_files["CHOICE.EXE"]
        more = prev_files["MORE.EXE"]
        find = prev_files["FIND.EXE"]
        label = prev_files["LABEL.EXE"]
        deltree = prev_files["DELTREE.COM"]
        # XCOPY.EXE is a standalone tool-src build (like TOKAEMM.SYS/GSWMODE.COM
        # below); prefer a freshly-built binary if present, falling back to the
        # previous image's copy (absent on the first image that ships XCOPY).
        xcopy_fresh = os.path.join(xcopydir, "xcopy.exe")
        if os.path.exists(xcopy_fresh):
            xcopy = open(xcopy_fresh, "rb").read()
        else:
            xcopy = prev_files["XCOPY.EXE"]
        # EDIT.COM is a standalone tool-src build (like XCOPY.EXE above); prefer a
        # freshly-built binary if present, falling back to the previous image's copy
        # (absent on the first image that ships EDIT.COM).
        edit_fresh = os.path.join(editdir, "edit.exe")
        if os.path.exists(edit_fresh):
            edit = open(edit_fresh, "rb").read()
        else:
            edit = prev_files["EDIT.COM"]
        print("sourcing binaries from the committed image (build artifacts absent)")
    assert len(mbr) == 512, "MBR must be 512 bytes"
    assert len(vbr) == 512, "FAT32 VBR must be 512 bytes"
    # The small Toka-DOS drivers and GSWMODE.COM are committed binaries (built
    # straight from NASM source by toka-dos/build-freedos.ps1 into the firmware
    # crate), never extracted from the previous image.
    tokaemm = open(os.path.join(
        repo, "crates", "izarravm-firmware", "roms", "dos", "tokaemm.sys"), "rb").read()
    tokacd = open(os.path.join(
        repo, "crates", "izarravm-firmware", "roms", "dos", "tokacd.sys"), "rb").read()
    gswmode = open(os.path.join(
        repo, "crates", "izarravm-firmware", "roms", "dos", "gswmode.com"), "rb").read()
    unhalt = open(os.path.join(
        repo, "crates", "izarravm-firmware", "roms", "dos", "unhalt.com"), "rb").read()
    sndctrl = open(os.path.join(
        repo, "crates", "izarravm-firmware", "roms", "dos", "sndctrl.com"), "rb").read()

    # CONFIG.SYS / AUTOEXEC point at C: (the HDD). TOKAEMM
    # loads as the memory manager, drawing EMS pages on demand from the same
    # shared XMS/VCPI arena rather than a fixed-size pool, and the system
    # runs in V86 under its monitor. DOS=HIGH,UMB uses the HMA + TOKAEMM's UMBs,
    # while LASTDRIVE=D
    # covers A: floppy / C: HDD / D: CD-ROM without wasting CDS entries. The system
    # binaries live in C:\DOS (see the file layout below), so DEVICE= and SHELL=
    # name that subdirectory; only CONFIG.SYS/AUTOEXEC.BAT stay in the root. The
    # SHELL= dir argument (C:\DOS) is where FreeCOM builds COMSPEC from.
    config_sys = (b"FILES=40\r\nLASTDRIVE=D\r\n"
                  b"DEVICE=C:\\DOS\\TOKAEMM.SYS RAM\r\n"
                  b"DOS=HIGH,UMB\r\n"
                  b"DEVICEHIGH=C:\\DOS\\TOKACD.SYS\r\n"
                  b"SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT\r\n")
    # Defaults the user owns (mount_hdd_folder seeds these if missing). PATH C:\DOS
    # lets the command-line tools (MOVE/SORT/MEM/...) and TOKAMOUS resolve from any
    # current directory. SET BLASTER advertises the emulated SB16 (base 0x220, IRQ7,
    # DMA1, high DMA5, wavetable MPU at 0x300, type 6 SB16). LH
    # loads the INT 33h mouse into a TOKAEMM UMB (LOADHIGH falls back to a low load).
    autoexec = (b"@ECHO OFF\r\nPROMPT $P$G\r\nPATH C:\\DOS\r\n"
                b"SET BLASTER=A220 I7 D1 H5 P300 T6\r\n"
                b"IZCDEX /I /D:TOKACD01 /L:D /Q\r\n"
                b"LH TOKAMOUS\r\n")
    hello_txt = b"Katea M0 OK\r\n"
    # The kernel signon points at "See C:\\LICENSE.TXT for more."; ship it on C:.
    license_txt = build_license_txt(repo)

    # --- FAT32 geometry for the partition -------------------------------------
    spc = sectors_per_cluster(PART_SECTORS)
    fatsz = fat_size_sectors(PART_SECTORS, spc)
    used = RESERVED_SECTORS + NUM_FATS * fatsz
    data_sectors = PART_SECTORS - used
    count_of_clusters = data_sectors // spc
    assert count_of_clusters >= MIN_FAT32_CLUSTERS, \
        f"only {count_of_clusters} clusters; not a valid FAT32"
    geo = {"spc": spc, "fatsz": fatsz, "total_sectors": PART_SECTORS}
    cluster_bytes = spc * BPS

    # --- whole-disk buffer ----------------------------------------------------
    img = bytearray(DISK_SECTORS * BPS)

    # --- MBR + partition table ------------------------------------------------
    img[0:512] = mbr
    pe = 0x1BE  # first partition entry
    img[pe + 0] = 0x80  # active/bootable
    img[pe + 1:pe + 4] = lba_to_chs(PART_START)            # CHS start
    img[pe + 4] = PART_TYPE_FAT32_LBA                      # type 0x0C
    img[pe + 5:pe + 8] = lba_to_chs(PART_START + PART_SECTORS - 1)  # CHS end
    struct.pack_into("<I", img, pe + 8, PART_START)        # RelSect (start LBA)
    struct.pack_into("<I", img, pe + 12, PART_SECTORS)     # NumSect
    img[0x1FE] = 0x55
    img[0x1FF] = 0xAA

    # --- partition region offsets (disk-absolute, in bytes) -------------------
    part_off = PART_START * BPS
    fat1_off = part_off + RESERVED_SECTORS * BPS
    fat2_off = fat1_off + fatsz * BPS
    data_off = part_off + used * BPS  # cluster 2 starts here

    # --- VBR (FAT32 BPB) + FSInfo + backup boot -------------------------------
    stamp_fat32_bpb(vbr, geo, PART_START)
    img[part_off:part_off + 512] = vbr
    # Backup boot sector copy at +6.
    bk_off = part_off + BACKUP_BOOT_SECTOR * BPS
    img[bk_off:bk_off + 512] = vbr

    # --- files -----------------------------------------------------------------
    # Only KERNEL.SYS, CONFIG.SYS and AUTOEXEC.BAT are truly forced to the root:
    # the boot sector loads KERNEL.SYS by root-directory name, and the kernel opens
    # CONFIG.SYS (then, via SHELL=, AUTOEXEC.BAT) from the boot-drive root. LICENSE.TXT
    # stays at root too, because the kernel signon points at "See C:\LICENSE.TXT".
    # Everything else -- COMMAND.COM and the command-line tools -- lives in C:\DOS so
    # a plain DIR of the root isn't buried under system files. KERNEL.SYS carries the
    # hidden+system+read-only attributes (0x27, the DOS convention for IO.SYS/
    # MSDOS.SYS) so it doesn't show in DIR either; the boot sector matches it by name
    # and never reads the attribute byte, so hiding it is safe.
    ATTR_ARCHIVE = 0x20
    ATTR_HIDDEN_SYS = 0x27  # archive | read-only | hidden | system
    ATTR_SUBDIR = 0x10
    root_files = [
        ("KERNEL.SYS", kernel, ATTR_HIDDEN_SYS),  # first, per FreeDOS SYS convention
        ("CONFIG.SYS", config_sys, ATTR_ARCHIVE),
        ("AUTOEXEC.BAT", autoexec, ATTR_ARCHIVE),
        ("LICENSE.TXT", license_txt, ATTR_ARCHIVE),
    ]
    dos_files = [
        ("COMMAND.COM", shell, ATTR_ARCHIVE),
        ("TOKAMOUS.COM", tokamous, ATTR_ARCHIVE),
        ("TOKAEMM.SYS", tokaemm, ATTR_ARCHIVE),
        ("TOKACD.SYS", tokacd, ATTR_ARCHIVE),
        ("IZCDEX.COM", izcdex, ATTR_ARCHIVE),
        ("GSWMODE.COM", gswmode, ATTR_ARCHIVE),
        ("UNHALT.COM", unhalt, ATTR_ARCHIVE),
        ("SNDCTRL.COM", sndctrl, ATTR_ARCHIVE),
        ("MOVE.EXE", move, ATTR_ARCHIVE),
        ("SORT.EXE", sort, ATTR_ARCHIVE),
        ("MEM.EXE", mem, ATTR_ARCHIVE),
        # Audit items 3+10 external tool batch (see VENDOR.md).
        ("ATTRIB.EXE", attrib, ATTR_ARCHIVE),
        ("CHOICE.EXE", choice, ATTR_ARCHIVE),
        ("MORE.EXE", more, ATTR_ARCHIVE),
        ("FIND.EXE", find, ATTR_ARCHIVE),
        ("LABEL.EXE", label, ATTR_ARCHIVE),
        ("DELTREE.COM", deltree, ATTR_ARCHIVE),
        # Original Toka-DOS project tool (GPL-3, not vendored) -- see
        # toka-dos/tools-src/README.md and toka-dos/msdos4/VENDOR.md.
        ("XCOPY.EXE", xcopy, ATTR_ARCHIVE),
        # Original Toka-DOS project tool (GPL-3, not vendored): TokaEdit, the
        # full-screen editor. A large-model MZ exe shipped as EDIT.COM --
        # faithful to real MS-DOS, whose EDIT.COM is itself an MZ executable.
        ("EDIT.COM", edit, ATTR_ARCHIVE),
        ("HELLO.TXT", hello_txt, ATTR_ARCHIVE),
    ]

    # Allocate cluster chains, write file data into the data region, and build the
    # two directories: the root (a cluster chain starting at ROOT_CLUSTER=2) and the
    # DOS subdirectory (its own chain, allocated right after the root).
    fat = {0: 0x0FFFFFF8, 1: FAT32_EOC}  # FAT[0] media + EOC bits, FAT[1] EOC

    def alloc_chain(start, nclu):
        for i in range(nclu):
            c = start + i
            fat[c] = FAT32_EOC if i == nclu - 1 else c + 1

    def write_file_data(first, data):
        """Write `data` into the contiguous cluster run starting at `first`, set its
        FAT chain, and return the cluster count consumed."""
        nclu = max(1, (len(data) + cluster_bytes - 1) // cluster_bytes)
        assert first + nclu - 1 <= count_of_clusters + 1, "out of clusters"
        alloc_chain(first, nclu)
        for i in range(nclu):
            c = first + i
            off = data_off + (c - ROOT_CLUSTER) * cluster_bytes
            chunk = data[i * cluster_bytes:(i + 1) * cluster_bytes]
            img[off:off + len(chunk)] = chunk
        return nclu

    def dir_entry(name11_field, first, size, attr):
        """A 32-byte directory entry from a pre-folded 11-byte name field."""
        de = bytearray(32)
        de[0:11] = name11_field
        de[11] = attr
        struct.pack_into("<H", de, 0x14, (first >> 16) & 0xFFFF)  # FstClusHI
        struct.pack_into("<H", de, 0x1A, first & 0xFFFF)          # FstClusLO
        struct.pack_into("<I", de, 0x1C, size)                    # file size
        return de

    entries_per_cluster = cluster_bytes // 32

    # Reserve both directory chains up front (root at 2, then DOS), THEN allocate
    # file-data clusters after them, so a directory chain's length never shifts under
    # file data placed earlier. This image's cluster size is 1 sector = 512 bytes =
    # 16 dir entries, so a directory past 16 entries is a real multi-cluster chain.
    root_entries = len(root_files) + 1  # + the DOS subdir entry
    root_clusters = max(1, -(-root_entries // entries_per_cluster))
    alloc_chain(ROOT_CLUSTER, root_clusters)
    next_free = ROOT_CLUSTER + root_clusters

    dos_first = next_free
    dos_entries = 2 + len(dos_files)  # '.' + '..' + the files
    dos_clusters = max(1, -(-dos_entries // entries_per_cluster))
    alloc_chain(dos_first, dos_clusters)
    next_free += dos_clusters

    # Root directory: each root file, then the DOS subdir entry.
    root = bytearray()
    for name, data, attr in root_files:
        first = next_free
        next_free += write_file_data(first, data)
        root += dir_entry(name11(name), first, len(data), attr)
    root += dir_entry(name11("DOS"), dos_first, 0, ATTR_SUBDIR)

    # DOS directory: '.' (own cluster) + '..' (0, since the parent is the root, per
    # fatgen103 6.5) + each tool. Its clusters were reserved above, so writing one
    # span across them lands correctly.
    dos = bytearray()
    dos += dir_entry(b".          ", dos_first, 0, ATTR_SUBDIR)
    dos += dir_entry(b"..         ", 0, 0, ATTR_SUBDIR)
    for name, data, attr in dos_files:
        first = next_free
        next_free += write_file_data(first, data)
        dos += dir_entry(name11(name), first, len(data), attr)

    assert len(root) <= root_clusters * cluster_bytes, \
        f"root directory ({len(root)} bytes) overflows its {root_clusters}-cluster chain"
    assert len(dos) <= dos_clusters * cluster_bytes, \
        f"DOS directory ({len(dos)} bytes) overflows its {dos_clusters}-cluster chain"

    # Both directory chains are contiguous (allocated first, in order), so each is one
    # write spanning its full byte length.
    root_off = data_off + (ROOT_CLUSTER - ROOT_CLUSTER) * cluster_bytes
    img[root_off:root_off + len(root)] = root
    dos_off = data_off + (dos_first - ROOT_CLUSTER) * cluster_bytes
    img[dos_off:dos_off + len(dos)] = dos

    # --- serialize both FATs --------------------------------------------------
    fat_bytes = bytearray(fatsz * BPS)
    for c, v in fat.items():
        struct.pack_into("<I", fat_bytes, c * 4, v & 0x0FFFFFFF)
    img[fat1_off:fat1_off + len(fat_bytes)] = fat_bytes
    img[fat2_off:fat2_off + len(fat_bytes)] = fat_bytes

    # --- FSInfo (after we know how many clusters we used) ---------------------
    used_clusters = next_free - ROOT_CLUSTER  # clusters 2..next_free-1
    free_count = count_of_clusters - used_clusters
    fsi = fsinfo_sector(free_count, next_free)
    img[part_off + FSINFO_SECTOR * BPS:part_off + FSINFO_SECTOR * BPS + 512] = fsi
    # Backup FSInfo (+7, right after the backup boot sector) for good measure.
    img[part_off + 7 * BPS:part_off + 7 * BPS + 512] = fsi

    # --- cross-checks ---------------------------------------------------------
    assert img[0x1FE] == 0x55 and img[0x1FF] == 0xAA, "MBR signature missing"
    assert struct.unpack_from("<I", img, part_off + 0x1C)[0] == PART_START, "HiddSec mismatch"
    assert img[part_off + 0x1FE] == 0x55 and img[part_off + 0x1FF] == 0xAA, "VBR signature missing"
    assert len(img) == DISK_SECTORS * BPS

    summary = (f"tokados-hdd.img: {len(img)} bytes "
               f"(part_start={PART_START}, part_sectors={PART_SECTORS}, "
               f"spc={spc}, fatsz={fatsz}, clusters={count_of_clusters}, "
               f"kernel={len(kernel)}, shell={len(shell)}, tokamous={len(tokamous)})")

    # --check: prove the committed image still matches its inputs, without
    # writing. The image is a build product of ~30 committed binaries plus
    # LICENSE.TXT, which is generated from NOTICE and the kernel COPYING -- so
    # editing NOTICE silently staled the shipped attribution file once already
    # (the Cranelift credit outlived the dependency by 465 commits), and every
    # file after it moved a cluster, which reads as 323 KB of mystery drift to
    # anyone who rebuilds. Cheap to check, invisible when it rots.
    if check:
        if not os.path.exists(out):
            print(f"FAIL: {out} does not exist", file=sys.stderr)
            return 1
        committed = open(out, "rb").read()
        if committed == bytes(img):
            print(f"tokados-hdd.img is reproducible from the tree ({len(img)} bytes)")
            return 0
        print("FAIL: the committed tokados-hdd.img does not match a build from "
              "this tree.", file=sys.stderr)
        if len(committed) != len(img):
            print(f"  size {len(committed)} committed vs {len(img)} rebuilt",
                  file=sys.stderr)
        else:
            differing = sum(1 for a, b in zip(committed, img) if a != b)
            first = next(i for i, (a, b) in enumerate(zip(committed, img)) if a != b)
            print(f"  {differing} bytes differ, first at {first:#x} "
                  f"(sector {first // BPS})", file=sys.stderr)
        print("  Rebuild it: python scripts/build-freedos-hdd-image.py",
              file=sys.stderr)
        return 1

    with open(out, "wb") as f:
        f.write(img)
    print(summary)
    return 0


if __name__ == "__main__":
    sys.exit(main(check="--check" in sys.argv[1:]))
