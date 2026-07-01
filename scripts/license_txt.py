"""Assemble C:\\LICENSE.TXT for the Toka-DOS disk images.

The boot banner points at "See C:\\LICENSE.TXT for more.", so the full FreeDOS /
Toka-DOS licensing lives in that one file on the C: drive instead of scrolling
past on every boot. It is built from the project's NOTICE (attributions) and the
FreeDOS kernel COPYING (the full GNU GPL v2) — single source, no duplicated blob;
edit NOTICE / kernel/COPYING and rebuild the images, not a checked-in copy.

build-freedos-hdd-image.py imports this.
"""
import os

# The Villani/FreeDOS copyright + GPL summary that used to scroll past in the
# kernel signon banner (kernel/main.c). Moved here verbatim in intent.
_HEADER = (
    "Toka-DOS 3.0 - licensing\r\n"
    "========================\r\n"
    "\r\n"
    "Toka-DOS is built on the FreeDOS kernel and FreeCOM, with MOVE, SORT and\r\n"
    "kitten from the FreeDOS project. It is free software under the GNU General\r\n"
    "Public License v2 (or, at your option, any later version) and comes with\r\n"
    "ABSOLUTELY NO WARRANTY.\r\n"
    "\r\n"
    "(C) Copyright 1995-2012 Pasquale J. Villani and The FreeDOS Project.\r\n"
    "All Rights Reserved.\r\n"
    "\r\n"
)


def _crlf(data: bytes) -> bytes:
    # Normalize to DOS line endings so TYPE / EDIT render the file correctly.
    return data.replace(b"\r\n", b"\n").replace(b"\n", b"\r\n")


def build_license_txt(repo: str) -> bytes:
    """Return the bytes of C:\\LICENSE.TXT: header + NOTICE + the full GPL v2."""
    notice = open(os.path.join(repo, "NOTICE"), "rb").read()
    copying = open(
        os.path.join(repo, "toka-dos", "freedos", "kernel", "COPYING"), "rb"
    ).read()
    return (
        _HEADER.encode("ascii")
        + b"--- NOTICE ----------------------------------------------------------\r\n\r\n"
        + _crlf(notice)
        + b"\r\n--- GNU GENERAL PUBLIC LICENSE, VERSION 2 ----------------------------\r\n\r\n"
        + _crlf(copying)
    )
