#!/usr/bin/env python3
"""
prep-freedos-spike.py
Derives a minimal-config FreeDOS boot floppy from x86BOOT.img (the FreeDOS
1.4 installer disk) by replacing only FDCONFIG.SYS and FDAUTO.BAT:
  - FDCONFIG.SYS: minimal config (SWITCHES, FILES, SHELL) — no MENU, no delay
  - FDAUTO.BAT: no-op (@ECHO OFF) so /P=\\FDAUTO.BAT skips installer + DATE prompt
  KERNEL.SYS and COMMAND.COM are byte-for-byte untouched.
  Result boots straight to an A:\\> prompt.

Usage:
    python scripts/prep-freedos-spike.py [src.img]
    FREEDOS_SPIKE_SRC=path python scripts/prep-freedos-spike.py

Output: .local/freedos/freedos-spike.img  (always rebuilt from source — idempotent)
"""

import os
import struct
import sys

# ---------------------------------------------------------------------------
# Locate source image and output path
# ---------------------------------------------------------------------------
default_src = r"D:\dev\IzarraVM\dev_docs\reference\FreeDOS disks\x86BOOT.img"
src_path = (
    sys.argv[1]
    if len(sys.argv) > 1
    else os.environ.get("FREEDOS_SPIKE_SRC", default_src)
)

script_dir = os.path.dirname(os.path.abspath(__file__))
repo_root  = os.path.dirname(script_dir)
out_dir    = os.path.join(repo_root, ".local", "freedos")
out_path   = os.path.join(out_dir, "freedos-spike.img")
os.makedirs(out_dir, exist_ok=True)

print(f"Source : {src_path}")
print(f"Output : {out_path}")

# ---------------------------------------------------------------------------
# Read source (always rebuild from scratch)
# ---------------------------------------------------------------------------
with open(src_path, "rb") as f:
    img = bytearray(f.read())

EXPECTED = 1_474_560
if len(img) != EXPECTED:
    raise RuntimeError(f"Source image is {len(img)} bytes, expected {EXPECTED}.")

# ---------------------------------------------------------------------------
# Parse BPB
# ---------------------------------------------------------------------------
bps     = struct.unpack_from("<H", img, 0x0B)[0]
spc     = img[0x0D]
rsvd    = struct.unpack_from("<H", img, 0x0E)[0]
nfat    = img[0x10]
rootent = struct.unpack_from("<H", img, 0x11)[0]
spf     = struct.unpack_from("<H", img, 0x16)[0]

fat_start  = rsvd * bps
root_start = (rsvd + nfat * spf) * bps
root_sects = (rootent * 32 + bps - 1) // bps
data_start = root_start + root_sects * bps
cluster_size = spc * bps

print(f"BPB: bps={bps} spc={spc} rsvd={rsvd} nfat={nfat} rootent={rootent} spf={spf}")
print(f"     fat_start={fat_start:#x}  root_start={root_start:#x}  data_start={data_start:#x}")

# ---------------------------------------------------------------------------
# FAT12 helpers
# ---------------------------------------------------------------------------
def fat_get(fat_bytes: bytearray, cluster: int) -> int:
    o = (cluster * 3) // 2
    v = fat_bytes[o] | (fat_bytes[o + 1] << 8)
    return (v >> 4) if (cluster & 1) else (v & 0xFFF)


def fat_set(fat_bytes: bytearray, cluster: int, value: int) -> None:
    o = (cluster * 3) // 2
    v = fat_bytes[o] | (fat_bytes[o + 1] << 8)
    if cluster & 1:
        v = (v & 0x000F) | ((value & 0xFFF) << 4)
    else:
        v = (v & 0xF000) | (value & 0xFFF)
    fat_bytes[o]     = v & 0xFF
    fat_bytes[o + 1] = (v >> 8) & 0xFF


def cluster_offset(cluster: int) -> int:
    return data_start + (cluster - 2) * spc * bps


# ---------------------------------------------------------------------------
# Replace a root-directory file with new content (single-cluster files only)
# ---------------------------------------------------------------------------
def replace_root_file(name11: bytes, new_content: bytes, label: str) -> int:
    """Locate name11 (11-byte 8.3, no dot) in the root dir, overwrite its first
    cluster with new_content (zero-padded), update the dir entry size, set FAT
    to EOC in both copies.  Returns the first cluster number."""
    assert len(name11) == 11, f"name11 must be 11 bytes, got {len(name11)}"
    assert len(new_content) <= cluster_size, (
        f"new {label} ({len(new_content)} B) exceeds one cluster ({cluster_size} B)"
    )
    for i in range(rootent):
        base = root_start + i * 32
        e = img[base:base + 32]
        if e[0] in (0, 0xE5):
            continue
        if e[0x0B] & 0x08:       # volume label
            continue
        if (e[0:11]).upper() != name11.upper():
            continue
        fc  = struct.unpack_from("<H", e, 0x1A)[0]
        old = struct.unpack_from("<I", e, 0x1C)[0]
        print(f"  {label}: dir@{base:#x}  clus={fc}  old_size={old}  new_size={len(new_content)}")
        # Overwrite cluster
        off = cluster_offset(fc)
        img[off:off + cluster_size] = b"\x00" * cluster_size
        img[off:off + len(new_content)] = new_content
        # Update dir entry size
        struct.pack_into("<I", img, base + 0x1C, len(new_content))
        # Set FAT to EOC in both copies
        EOC = 0xFFF
        for fn in range(nfat):
            fat_base  = fat_start + fn * spf * bps
            fat_slice = bytearray(img[fat_base:fat_base + spf * bps])
            fat_set(fat_slice, fc, EOC)
            img[fat_base:fat_base + spf * bps] = fat_slice
            print(f"    FAT{fn+1}: clus {fc} -> {fat_get(fat_slice, fc):#05x} (EOC)")
        return fc
    raise RuntimeError(f"{label} not found in root directory!")


# ---------------------------------------------------------------------------
# Files to replace
# ---------------------------------------------------------------------------
# FDCONFIG.SYS — minimal config; /P=\FDAUTO.BAT makes FreeCom run our no-op
# batch rather than prompting for DATE/TIME
NEW_FDCONFIG = (
    "SWITCHES=/N\r\n"
    "LASTDRIVE=Z\r\n"
    "FILES=40\r\n"
    "SHELL=\\FREEDOS\\BIN\\COMMAND.COM \\FREEDOS\\BIN /E:2048 /P=\\FDAUTO.BAT\r\n"
).encode("ascii")

# FDAUTO.BAT — no-op: just @ECHO OFF, no installer, no DATE/TIME
NEW_FDAUTO = b"@ECHO OFF\r\n"

print("\n--- Patching files ---")
replace_root_file(b"FDCONFIGSYS", NEW_FDCONFIG, "FDCONFIG.SYS")
replace_root_file(b"FDAUTO  BAT", NEW_FDAUTO,   "FDAUTO.BAT")

# ---------------------------------------------------------------------------
# Write output
# ---------------------------------------------------------------------------
with open(out_path, "wb") as f:
    f.write(img)

actual = os.path.getsize(out_path)
if actual != EXPECTED:
    raise RuntimeError(f"Output is {actual} bytes, expected {EXPECTED}!")
print(f"\nWrote {actual} bytes -> {out_path}")

# ---------------------------------------------------------------------------
# Verification — re-read and dump root dir + patched file contents
# ---------------------------------------------------------------------------
with open(out_path, "rb") as f:
    v = f.read()

print("\n=== Root directory ===")
for i in range(rootent):
    base = root_start + i * 32
    e = v[base:base + 32]
    if not e or e[0] in (0, 0xE5):
        continue
    attr = e[0x0B]
    if attr & 0x08:
        continue
    name = e[0:8].decode("latin1").rstrip()
    ext  = e[8:11].decode("latin1").rstrip()
    fn   = name + ("." + ext if ext else "")
    fc   = struct.unpack_from("<H", e, 0x1A)[0]
    sz   = struct.unpack_from("<I", e, 0x1C)[0]
    flag = "<DIR>" if (attr & 0x10) else ""
    print(f"  {fn:<13} {flag:<5} clus={fc}  size={sz}")

def show_file(name11: bytes, label: str) -> None:
    for i in range(rootent):
        base = root_start + i * 32
        e = v[base:base + 32]
        if e[0] in (0, 0xE5) or (e[0x0B] & 0x08):
            continue
        if (e[0:11]).upper() != name11.upper():
            continue
        fc = struct.unpack_from("<H", e, 0x1A)[0]
        sz = struct.unpack_from("<I", e, 0x1C)[0]
        off = cluster_offset(fc)
        content = v[off:off + sz].decode("ascii", errors="replace")
        print(f"\n=== {label} ({sz} bytes) ===")
        for line in content.splitlines():
            print(f"  {repr(line)}")
        return
    print(f"\n=== {label}: NOT FOUND ===")

show_file(b"FDCONFIGSYS", "FDCONFIG.SYS")
show_file(b"FDAUTO  BAT", "FDAUTO.BAT")

print("\nImage ready. Set:")
print(f"  IZARRAVM_FREEDOS_SPIKE_IMG={out_path}")
