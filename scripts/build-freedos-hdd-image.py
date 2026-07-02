#!/usr/bin/env python3
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
                                (FreeDOS requires it), then the shell, CONFIG.SYS,
                                AUTOEXEC.BAT, and HELLO.TXT.

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

    files = []
    c = root_clu
    while 2 <= c < 0x0FFFFFF8:
        off = data_off + (c - 2) * cluster_bytes
        for e in range(0, cluster_bytes, 32):
            de = img[off + e:off + e + 32]
            if de[0] == 0x00:
                break
            if de[0] == 0xE5 or de[11] == 0x0F or de[11] & 0x08:
                continue
            base = de[0:8].decode("ascii").rstrip()
            ext = de[8:11].decode("ascii").rstrip()
            name = f"{base}.{ext}" if ext else base
            first = (struct.unpack_from("<H", de, 0x14)[0] << 16) | \
                struct.unpack_from("<H", de, 0x1A)[0]
            size = struct.unpack_from("<I", de, 0x1C)[0]
            files.append((name, chain_bytes(first, size)))
        c = fat_entry(c)
    return bytearray(img[0:512]), bytearray(vbr), dict(files)


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    repo = os.path.dirname(here)
    kdir = os.path.join(repo, "toka-dos", "freedos", "kernel")
    fcdir = os.path.join(repo, "toka-dos", "freedos", "freecom")
    movedir = os.path.join(repo, "toka-dos", "freedos", "move", "src")
    sortdir = os.path.join(repo, "toka-dos", "freedos", "sort", "src")
    memdir = os.path.join(repo, "toka-dos", "freedos", "mem", "source")
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
        move = open(os.path.join(movedir, "move.exe"), "rb").read()
        sort = open(os.path.join(sortdir, "sort.exe"), "rb").read()
        mem = open(os.path.join(memdir, "mem.exe"), "rb").read()
    else:
        # From-image path: source the binaries from the current committed image.
        prev = open(out, "rb").read()
        mbr, vbr, prev_files = extract_from_image(prev)
        kernel = prev_files["KERNEL.SYS"]
        shell = prev_files["COMMAND.COM"]
        tokamous = prev_files["TOKAMOUS.COM"]
        move = prev_files["MOVE.EXE"]
        sort = prev_files["SORT.EXE"]
        mem = prev_files["MEM.EXE"]
        print("sourcing binaries from the committed image (build artifacts absent)")
    assert len(mbr) == 512, "MBR must be 512 bytes"
    assert len(vbr) == 512, "FAT32 VBR must be 512 bytes"
    # TOKAEMM.SYS is a committed binary (built from tokaemm.asm), never extracted.
    tokaemm = open(os.path.join(
        repo, "crates", "izarravm-firmware", "roms", "dos", "tokaemm.sys"), "rb").read()

    # CONFIG.SYS / AUTOEXEC point at C: (the HDD). SP-4b M4 defaults: TOKAEMM
    # loads as the memory manager (frameless NOEMS; the system runs in V86 under
    # its monitor), DOS=HIGH,UMB uses the HMA + TOKAEMM's UMBs, LASTDRIVE=D
    # covers A: floppy / C: HDD / D: CD-ROM without wasting CDS entries.
    config_sys = (b"FILES=40\r\nLASTDRIVE=D\r\n"
                  b"DEVICE=C:\\TOKAEMM.SYS NOEMS\r\n"
                  b"DOS=HIGH,UMB\r\n"
                  b"SHELL=C:\\COMMAND.COM C:\\ /E:2048 /P=C:\\AUTOEXEC.BAT\r\n")
    # Defaults the user owns (mount_hdd_folder seeds these if missing). PATH lets
    # the external commands (MOVE/SORT/MEM) resolve from any current directory.
    # SET BLASTER advertises the emulated SB16 (base 0x220, IRQ5, DMA1, high DMA5,
    # type 6 SB16); no P (MPU-401) param -- no MPU is emulated. LH loads the INT
    # 33h mouse into a TOKAEMM UMB (LOADHIGH falls back to a low load if none fits).
    autoexec = (b"@ECHO OFF\r\nPROMPT $P$G\r\nPATH C:\\\r\n"
                b"SET BLASTER=A220 I5 D1 H5 T6\r\nLH TOKAMOUS\r\n")
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

    # --- files (KERNEL.SYS first) ---------------------------------------------
    files = [
        ("KERNEL.SYS", kernel),
        ("COMMAND.COM", shell),
        ("CONFIG.SYS", config_sys),
        ("AUTOEXEC.BAT", autoexec),
        ("TOKAMOUS.COM", tokamous),
        ("TOKAEMM.SYS", tokaemm),
        ("MOVE.EXE", move),
        ("SORT.EXE", sort),
        ("MEM.EXE", mem),
        ("HELLO.TXT", hello_txt),
        ("LICENSE.TXT", license_txt),
    ]

    # Allocate cluster chains, write file data into the data region, and build the
    # root directory entries (the root is itself a cluster chain starting at 2).
    fat = {0: 0x0FFFFFF8, 1: FAT32_EOC}  # FAT[0] media + EOC bits, FAT[1] EOC

    def alloc_chain(start, nclu):
        for i in range(nclu):
            c = start + i
            fat[c] = FAT32_EOC if i == nclu - 1 else c + 1

    # Root directory occupies cluster 2 (one cluster is plenty for 7 entries).
    fat[ROOT_CLUSTER] = FAT32_EOC
    next_free = ROOT_CLUSTER + 1
    root = bytearray()

    for fn, data in files:
        nclu = max(1, (len(data) + cluster_bytes - 1) // cluster_bytes)
        first = next_free
        assert first + nclu - 1 <= count_of_clusters + 1, f"out of clusters writing {fn}"
        alloc_chain(first, nclu)
        for i in range(nclu):
            c = first + i
            off = data_off + (c - ROOT_CLUSTER) * cluster_bytes
            chunk = data[i * cluster_bytes:(i + 1) * cluster_bytes]
            img[off:off + len(chunk)] = chunk
        next_free += nclu
        # 32-byte directory entry: 8.3 name, archive attr, cluster split hi/lo, size.
        de = bytearray(32)
        de[0:11] = name11(fn)
        de[11] = 0x20  # archive
        struct.pack_into("<H", de, 0x14, (first >> 16) & 0xFFFF)  # FstClusHI
        struct.pack_into("<H", de, 0x1A, first & 0xFFFF)          # FstClusLO
        struct.pack_into("<I", de, 0x1C, len(data))               # file size
        root += de

    # Write the root directory into cluster 2.
    root_off = data_off + (ROOT_CLUSTER - ROOT_CLUSTER) * cluster_bytes
    img[root_off:root_off + len(root)] = root

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

    with open(out, "wb") as f:
        f.write(img)
    print(f"tokados-hdd.img: {len(img)} bytes "
          f"(part_start={PART_START}, part_sectors={PART_SECTORS}, "
          f"spc={spc}, fatsz={fatsz}, clusters={count_of_clusters}, "
          f"kernel={len(kernel)}, shell={len(shell)}, tokamous={len(tokamous)})")


if __name__ == "__main__":
    main()
