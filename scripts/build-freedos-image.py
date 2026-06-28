#!/usr/bin/env python3
"""Build a bootable 1.44MB FAT12 Toka-DOS floppy image from built components.
Authoring-only. Inputs: the built boot sector, KERNEL.SYS, command.com, and the
in-repo CONFIG.SYS/AUTOEXEC.BAT text. Output: crates/izarravm-firmware/roms/tokados.img.
Shares only the FAT12 nibble helper with the (retired) SP-1 patcher; this is a
from-scratch builder (format + BPB + multi-cluster files + fresh root entries)."""
import os, struct

# 1.44MB FAT12 geometry.
BPS, SPC, RSVD, NFAT, ROOTENT, TOTAL, SPF = 512, 1, 1, 2, 224, 2880, 9
MEDIA, SECT_TRK, HEADS = 0xF0, 18, 2
IMG_SIZE = TOTAL * BPS  # 1_474_560
FAT_START   = RSVD * BPS
ROOT_START  = (RSVD + NFAT * SPF) * BPS
ROOT_SECTS  = (ROOTENT * 32 + BPS - 1) // BPS
DATA_START  = ROOT_START + ROOT_SECTS * BPS
CLUSTER     = SPC * BPS

def fat_set(fat, cluster, value):  # FAT12 nibble-pack helper
    o = (cluster * 3) // 2
    v = fat[o] | (fat[o + 1] << 8)
    if cluster & 1:
        v = (v & 0x000F) | ((value & 0xFFF) << 4)
    else:
        v = (v & 0xF000) | (value & 0xFFF)
    fat[o] = v & 0xFF
    fat[o + 1] = (v >> 8) & 0xFF

def name11(fn):
    base, _, ext = fn.partition(".")
    assert len(base) <= 8 and len(ext) <= 3, f"name not 8.3: {fn}"
    return (base.upper().ljust(8) + ext.upper().ljust(3)).encode("ascii")

def stamp_bpb(img, oem=b"TOKADOS ", label=b"TOKA-DOS   "):
    # Keep boot code + 0xAA55 from fat12com.bin; author the BPB ourselves.
    img[0x03:0x0B] = oem
    struct.pack_into("<H", img, 0x0B, BPS)
    img[0x0D] = SPC
    struct.pack_into("<H", img, 0x0E, RSVD)
    img[0x10] = NFAT
    struct.pack_into("<H", img, 0x11, ROOTENT)
    struct.pack_into("<H", img, 0x13, TOTAL)
    img[0x15] = MEDIA
    struct.pack_into("<H", img, 0x16, SPF)
    struct.pack_into("<H", img, 0x18, SECT_TRK)
    struct.pack_into("<H", img, 0x1A, HEADS)
    struct.pack_into("<I", img, 0x1C, 0)          # hidden sectors
    struct.pack_into("<I", img, 0x20, 0)          # huge total
    img[0x24] = 0x00                              # drive (floppy)
    img[0x26] = 0x29                              # ext boot sig
    struct.pack_into("<I", img, 0x27, 0x32303236) # volume id (arbitrary)
    img[0x2B:0x36] = label
    img[0x36:0x3E] = b"FAT12   "

def main():
    here   = os.path.dirname(os.path.abspath(__file__))
    repo   = os.path.dirname(here)
    kdir   = os.path.join(repo, "toka-dos", "freedos", "kernel")
    fcdir  = os.path.join(repo, "toka-dos", "freedos", "freecom")
    boot   = open(os.path.join(kdir, "boot", "fat12com.bin"), "rb").read()
    assert len(boot) == 512, "boot sector must be 512 bytes"
    kernel = open(os.path.join(kdir, "bin", "kernel.sys"), "rb").read()
    shell  = open(os.path.join(fcdir, "command.com"), "rb").read()

    tokamous = open(os.path.join(repo, "toka-dos", "build-freedos-tokamous.com"), "rb").read()
    move = open(os.path.join(repo, "toka-dos", "freedos", "move", "src", "move.exe"), "rb").read()
    sort = open(os.path.join(repo, "toka-dos", "freedos", "sort", "src", "sort.exe"), "rb").read()

    config_sys = (b"FILES=40\r\nLASTDRIVE=Z\r\n"
                  b"SHELL=A:\\TOKACMD.COM A:\\ /E:2048 /P=A:\\AUTOEXEC.BAT\r\n")
    autoexec   = b"@ECHO OFF\r\nTOKAMOUS\r\n"

    img = bytearray(IMG_SIZE)
    img[0:512] = boot
    stamp_bpb(img)
    fat = bytearray(SPF * BPS)
    fat[0], fat[1], fat[2] = MEDIA, 0xFF, 0xFF  # reserved FAT[0]/[1]

    # KERNEL.SYS must be in the root dir before the first free slot; add it first.
    files = [("KERNEL.SYS", kernel), ("TOKACMD.COM", shell),
             ("COMMAND.COM", shell),          # byte-copy alias
             ("TOKAMOUS.COM", tokamous),
             ("MOUSE.COM", tokamous),         # byte-copy alias
             ("MOVE.EXE", move),
             ("SORT.EXE", sort),
             ("CONFIG.SYS", config_sys), ("AUTOEXEC.BAT", autoexec)]

    next_free = 2
    for slot, (fn, data) in enumerate(files):
        nclu = max(1, (len(data) + CLUSTER - 1) // CLUSTER)
        usable = (IMG_SIZE - DATA_START) // CLUSTER
        assert next_free - 2 + nclu <= usable, f"out of clusters writing {fn}"
        first = next_free
        for i in range(nclu):
            c = first + i
            off = DATA_START + (c - 2) * CLUSTER
            chunk = data[i * CLUSTER:(i + 1) * CLUSTER]
            img[off:off + len(chunk)] = chunk
            fat_set(fat, c, 0xFFF if i == nclu - 1 else c + 1)
        next_free += nclu
        de = ROOT_START + slot * 32
        img[de:de + 11] = name11(fn)
        img[de + 0x0B] = 0x20  # archive
        struct.pack_into("<H", img, de + 0x1A, first)
        struct.pack_into("<I", img, de + 0x1C, len(data))

    # Write both FAT copies.
    img[FAT_START:FAT_START + SPF * BPS] = fat
    img[FAT_START + SPF * BPS:FAT_START + 2 * SPF * BPS] = fat

    # BPB<->geometry cross-check: the boot sector reads the BPB we just stamped.
    assert struct.unpack_from("<H", img, 0x0B)[0] == BPS
    assert struct.unpack_from("<H", img, 0x16)[0] == SPF
    assert struct.unpack_from("<H", img, 0x13)[0] == TOTAL
    assert img[0x1FE] == 0x55 and img[0x1FF] == 0xAA, "boot signature missing"
    assert len(img) == IMG_SIZE

    out = os.path.join(repo, "crates", "izarravm-firmware", "roms", "tokados.img")
    with open(out, "wb") as f:
        f.write(img)
    print(f"tokados.img: {len(img)} bytes (kernel={len(kernel)}, shell={len(shell)}, tokamous={len(tokamous)}, move={len(move)}, sort={len(sort)})")

if __name__ == "__main__":
    main()
