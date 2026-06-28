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

Source resolution order (when no positional arg is given):
  1. FREEDOS_SPIKE_SRC env var (if set)
  2. <repo>/.local/freedos/freedos-raw.img  (produced by fetch-freedos-spike.ps1)
  3. <repo>/dev_docs/reference/FreeDOS disks/x86BOOT.img  (local disk cache)

Output: <repo>/.local/freedos/freedos-spike.img  (always rebuilt from source — idempotent)

After running, set:
  IZARRAVM_FREEDOS_SPIKE_IMG=<repo>/.local/freedos/freedos-spike.img
"""

import os
import struct
import sys

# ---------------------------------------------------------------------------
# Locate source image and output path
# ---------------------------------------------------------------------------
script_dir = os.path.dirname(os.path.abspath(__file__))
repo_root  = os.path.dirname(script_dir)

def _resolve_src() -> str:
    if len(sys.argv) > 1:
        return sys.argv[1]
    env = os.environ.get("FREEDOS_SPIKE_SRC")
    if env:
        return env
    raw = os.path.join(repo_root, ".local", "freedos", "freedos-raw.img")
    if os.path.exists(raw):
        return raw
    # Fall back to the local FreeDOS 1.4 disk cache (dev_docs/ is git-ignored)
    return os.path.join(repo_root, "dev_docs", "reference", "FreeDOS disks", "x86BOOT.img")

src_path = _resolve_src()

out_dir  = os.path.join(repo_root, ".local", "freedos")
out_path = os.path.join(out_dir, "freedos-spike.img")
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
    cluster with new_content (zero-padded), update the dir entry size, and fix
    the FAT chain in both copies:
      - first cluster -> EOC (0xFFF)
      - every subsequent original cluster -> FREE (0x000)
    Our replacement always fits in one cluster, so freeing the tail is sufficient.
    Returns the first cluster number."""
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
        if e[0x0B] == 0x0F:      # long-file-name (VFAT) entry — skip
            continue
        if (e[0:11]).upper() != name11.upper():
            continue
        fc  = struct.unpack_from("<H", e, 0x1A)[0]
        old = struct.unpack_from("<I", e, 0x1C)[0]
        print(f"  {label}: dir@{base:#x}  clus={fc}  old_size={old}  new_size={len(new_content)}")
        # Overwrite first cluster only (new content always fits in one cluster)
        off = cluster_offset(fc)
        img[off:off + cluster_size] = b"\x00" * cluster_size
        img[off:off + len(new_content)] = new_content
        # Update dir entry size
        struct.pack_into("<I", img, base + 0x1C, len(new_content))
        # Fix FAT chain in both copies: walk original chain, set first -> EOC,
        # free every subsequent cluster so CHKDSK reports no lost clusters.
        EOC  = 0xFFF
        FREE = 0x000
        for fn in range(nfat):
            fat_base  = fat_start + fn * spf * bps
            fat_slice = bytearray(img[fat_base:fat_base + spf * bps])
            # Walk the original chain before modifying anything
            chain = []
            c = fc
            while True:
                chain.append(c)
                nxt = fat_get(fat_slice, c)
                if nxt >= 0xFF8 or nxt == 0:
                    break
                c = nxt
            # First cluster -> EOC; tail clusters -> FREE
            fat_set(fat_slice, chain[0], EOC)
            for tail in chain[1:]:
                fat_set(fat_slice, tail, FREE)
                print(f"    FAT{fn+1}: clus {tail} -> FREE (orphan freed)")
            img[fat_base:fat_base + spf * bps] = fat_slice
            print(f"    FAT{fn+1}: clus {chain[0]} -> {fat_get(fat_slice, chain[0]):#05x} (EOC)")
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
    if attr & 0x08:        # volume label
        continue
    if attr == 0x0F:       # long-file-name (VFAT) entry — skip
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
