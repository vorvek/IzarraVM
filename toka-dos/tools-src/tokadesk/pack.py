#!/usr/bin/env python3
# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only

"""Pack the 16-bit VCPI stub and the 32-bit payload into TOKADESK.EXE.

The MZ load image is the stub only. The payload sits past that image; the
stub seeks to it after DE0C lands on copy32. Header fields at stub offset 16
are patched here.
"""
import os
import struct
import sys

HDR_OFF = 16
STACK_BYTES = 16 * 1024
PAGE = 4096


def main():
    if len(sys.argv) != 4:
        sys.exit("usage: pack.py stub.bin payload.bin tokadesk.exe")
    stub_path, payload_path, out_path = sys.argv[1], sys.argv[2], sys.argv[3]
    stub = bytearray(open(stub_path, "rb").read())
    payload = bytearray(open(payload_path, "rb").read())
    while len(payload) % 4:
        payload.append(0)
    if len(stub) < HDR_OFF + 24:
        sys.exit("stub.bin is too short to hold the overlay header")

    payload_size = len(payload)
    n = (payload_size + STACK_BYTES + PAGE - 1) // PAGE
    if 0x200000 + n * PAGE > 0x400000:
        sys.exit("payload + stack does not fit in PT0 at 0x200000")

    overlay = 32 + ((len(stub) + 15) // 16) * 16
    stub += b"\x00" * (overlay - 32 - len(stub))

    struct.pack_into("<IIIII", stub, HDR_OFF, overlay, payload_size, 0, STACK_BYTES, n)
    struct.pack_into("<I", stub, HDR_OFF + 20, len(stub))

    load_size = 32 + len(stub)
    pages = (load_size + 511) // 512
    last = load_size % 512
    header = bytearray(32)
    header[0:2] = b"MZ"
    struct.pack_into("<HHHHHHHHHHH", header, 2,
                     last if last else 512,
                     pages,
                     0,          # relocs
                     2,          # header paragraphs
                     0x40,       # minalloc
                     0xFFFF,     # maxalloc
                     0,          # SS = CS
                     0xFFFE,     # SP, replaced at runtime
                     0,          # checksum
                     0,          # IP
                     0)          # CS
    struct.pack_into("<HH", header, 24, 0x1C, 0)

    with open(out_path, "wb") as out:
        out.write(header)
        out.write(stub)
        out.write(payload)
    print("TOKADESK.EXE: %d bytes (stub %d, payload %d, N=%d)" % (
        os.path.getsize(out_path), len(stub), payload_size, n))


if __name__ == "__main__":
    main()
