#!/usr/bin/env python3
# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only

"""Check that committed guest binaries match their NASM sources.

The script rebuilds each guest binary that has a NASM recipe in a temporary
directory. It then compares the result with the committed file, byte for byte.
It needs NASM 3.01. Other NASM versions can emit other bytes.

The table RECIPES below is the only recipe set that this script checks. The
script does not read the header comments of the .asm files.

Which manifest rows are in scope: every LICENSE_MANIFEST.tsv row of class
generated-guest-binary or generated-firmware, and every generated-test-fixture
row whose path ends in .com. Each of these rows must have an entry in RECIPES
or in OUT_OF_SCOPE. A new row without an entry fails the check. The other
generated-test-fixture rows are audio tone files. They have no assembler
source, so the rule FIXTURE_NON_ASM_SUFFIXES skips them by extension.

What this check does NOT prove:
  * The Toka-DOS kernel and boot sectors exist only inside tokados-hdd.img.
    "build-freedos-hdd-image.py --check" compares that image with its input
    binaries. It does not compare them with their sources.
  * exehello.exe and tokadesk.exe need Open Watcom. They are in OUT_OF_SCOPE.

Exit codes: 0 when every binary matches, 1 for any mismatch, setup error, or
missing NASM. The flag --allow-missing-nasm turns a missing NASM into a skip
with exit code 0. The script ignores that flag when the environment variable
CI or GITHUB_ACTIONS is set. The script uses no network.
"""

from __future__ import annotations

import argparse
import csv
import os
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

NASM_VERSION = "3.01"
ROOT = Path(__file__).resolve().parent.parent
ROMS = "crates/izarravm-firmware/roms"
FIXTURES = "crates/izarravm-machine/tests/fixtures"
FLOPPY_SIZE = 1474560
SECTOR = 512

SCOPE_CLASSES = ("generated-guest-binary", "generated-firmware")
FIXTURE_CLASS = "generated-test-fixture"
FIXTURE_ASM_SUFFIX = ".com"
FIXTURE_NON_ASM_SUFFIXES = (".wav", ".mp3", ".flac", ".ogg")

OUT_OF_SCOPE = {
    f"{ROMS}/dos/exehello.exe": "needs NASM -f obj and the Open Watcom linker",
    f"{ROMS}/dos/tokadesk.exe": "needs Open Watcom; see toka-dos/tools-src/tokadesk",
}


@dataclass
class Step:
    """One NASM run. The source file is relative to the recipe directory."""

    source: str
    args: tuple = ()


@dataclass
class Recipe:
    out: str
    cwd: str  # relative to the repository root
    steps: list
    compose: str | None = None  # "floppy": boot sector, then stage 2 at 512


def simple(path: str) -> Recipe:
    """Recipe for a single-source binary that sits next to its .asm file."""
    directory, name = path.rsplit("/", 1)
    return Recipe(path, directory, [Step(name.rsplit(".", 1)[0].lower() + ".asm")])


def floppy(path: str, stage2: str) -> Recipe:
    directory = path.rsplit("/", 1)[0]
    return Recipe(
        path,
        directory,
        [Step("boot.asm", ("-I", ".")), Step(stage2, ("-I", "."))],
        "floppy",
    )


DOS_TOOLS = (
    "cdaudio cdprot cdtest echo emmprobe emsfrag emsnone emstest exit42 gpemul "
    "gpreflct gpstorm gswmode hello hlttest int0drfl int88rmp irq5ip0 isrset "
    "loadtest mark mousegfx mousetst multihlt nointa picstale runner sndctrl "
    "sndmixer sndtst type umbmech umbtest unhalt vcpidet vcpiif vcpilow "
    "vcpimem vcpisw xmsarena xmstest"
).split()

RECIPES = (
    [simple(f"{ROMS}/dos/{name}.com") for name in DOS_TOOLS]
    + [
        simple(f"{ROMS}/dos/relocchk.exe"),
        simple(f"{ROMS}/dos/v86spike.bin"),
        # The define fixes the include directory. build-freedos.ps1 passes it too.
        Recipe(
            f"{ROMS}/dos/tokaemm.sys",
            f"{ROMS}/dos",
            [Step("tokaemm.asm", ('-dTOKAEMM_SOURCE_DIR="{cwd}/"',))],
        ),
        Recipe(f"{ROMS}/dos/tokamous.com", "toka-dos/tools", [Step("tokamous.asm")]),
        simple(f"{ROMS}/disttri.bin"),
        simple(f"{ROMS}/i386dx25-test.bin"),
        simple(f"{ROMS}/izarra-bios.bin"),
        simple(f"{ROMS}/kbd-bios.bin"),
        simple(f"{ROMS}/kbd-resident.bin"),
        floppy(f"{ROMS}/boot-suite/izarravm-test.img", "stage2.asm"),
        floppy(f"{ROMS}/neurketa/neurketa.img", "neurketa-stage2.asm"),
    ]
    + [
        simple(f"{FIXTURES}/{name}")
        for name in (
            "CBLEAK.COM GFXCUR.COM MTEST.COM SEGKEEP.COM WHEELTEST.COM "
            "lotura_crc.com pmirq5.com vcpipic.com"
        ).split()
    ]
)


def manifest_scope(errors: list) -> set:
    """Return the manifest paths that this script must cover."""
    paths = set()
    with open(ROOT / "LICENSE_MANIFEST.tsv", newline="", encoding="utf-8") as handle:
        for row in list(csv.reader(handle, delimiter="\t"))[1:]:
            path, kind = row[0], row[1]
            lower = path.lower()
            if kind in SCOPE_CLASSES:
                paths.add(path)
            elif kind == FIXTURE_CLASS:
                if lower.endswith(FIXTURE_ASM_SUFFIX):
                    paths.add(path)
                elif not lower.endswith(FIXTURE_NON_ASM_SUFFIXES):
                    errors.append(f"{path}: unknown fixture type; add a recipe or a rule")
    return paths


def check_table(scope: set, errors: list) -> None:
    seen = set()
    for recipe in RECIPES:
        if recipe.out in seen:
            errors.append(f"{recipe.out}: two recipes write this path")
        seen.add(recipe.out)
    for path in sorted(seen & set(OUT_OF_SCOPE)):
        errors.append(f"{path}: has a recipe and an OUT_OF_SCOPE entry")
    for path in sorted(scope - seen - set(OUT_OF_SCOPE)):
        errors.append(f"{path}: manifest row has no recipe and no OUT_OF_SCOPE entry")
    for path in sorted((seen | set(OUT_OF_SCOPE)) - scope):
        errors.append(f"{path}: table entry has no matching manifest row in scope")


def nasm_version(nasm: str, env: dict) -> str | None:
    try:
        text = subprocess.run(
            [nasm, "-v"], capture_output=True, text=True, env=env, timeout=30
        ).stdout
    except (OSError, subprocess.TimeoutExpired):
        return None
    match = re.search(r"NASM version (\S+)", text)
    return match.group(1) if match else None


def command_text(recipe: Recipe) -> str:
    parts = []
    for step in recipe.steps:
        args = [a.replace("{cwd}", "<this directory>") for a in step.args]
        parts.append(" ".join(["nasm", "-f", "bin", *args, step.source, "-o", "<out>"]))
    text = f"(in {recipe.cwd}) " + " && ".join(parts)
    if recipe.compose:
        text += (
            "; then write boot at offset 0 and stage 2 at offset 512 "
            "of a zero-filled 1,474,560-byte file"
        )
    return text


def build(recipe: Recipe, nasm: str, env: dict, tmp: Path) -> bytes:
    """Run the NASM steps of one recipe and return the built bytes."""
    cwd = ROOT / recipe.cwd
    outputs = []
    for index, step in enumerate(recipe.steps):
        out = tmp / f"step{index}.bin"
        args = [a.replace("{cwd}", cwd.as_posix()) for a in step.args]
        cmd = [nasm, "-f", "bin", *args, step.source, "-o", str(out)]
        run = subprocess.run(
            cmd, cwd=cwd, capture_output=True, text=True, env=env, timeout=120
        )
        if run.returncode != 0:
            tail = (run.stderr or run.stdout).strip().splitlines()[-3:]
            raise RuntimeError(f"NASM exited with {run.returncode}: " + " | ".join(tail))
        outputs.append(out.read_bytes())
    if recipe.compose != "floppy":
        return outputs[0]
    boot, stage2 = outputs
    if len(boot) != SECTOR:
        raise RuntimeError(f"boot sector is {len(boot)} bytes, not {SECTOR}")
    if SECTOR + len(stage2) > FLOPPY_SIZE:
        raise RuntimeError("stage 2 does not fit in the floppy image")
    image = bytearray(FLOPPY_SIZE)
    image[:SECTOR] = boot
    image[SECTOR : SECTOR + len(stage2)] = stage2
    return bytes(image)


def main() -> int:
    parser = argparse.ArgumentParser(description="Check that guest binaries match their sources.")
    parser.add_argument("--allow-missing-nasm", action="store_true")
    parser.add_argument("--any-nasm", action="store_true", help="accept any NASM version")
    parser.add_argument("--nasm", help="path of the NASM program (default: search PATH)")
    args = parser.parse_args()

    env = dict(os.environ)
    env.pop("NASMENV", None)  # NASMENV adds hidden options to every run
    nasm = args.nasm or shutil.which("nasm")
    version = nasm_version(nasm, env) if nasm else None
    if version is None:
        in_ci = bool(env.get("CI") or env.get("GITHUB_ACTIONS"))
        if args.allow_missing_nasm and not in_ci:
            print(f"check_guest_binaries: SKIPPED, NASM {NASM_VERSION} not found (0 verified)")
            return 0
        print(
            f"check_guest_binaries: NASM {NASM_VERSION} is required and was not found. "
            "Install it, or use --allow-missing-nasm outside CI."
        )
        return 1
    if version != NASM_VERSION and not args.any_nasm:
        print(
            f"check_guest_binaries: NASM {version} found, NASM {NASM_VERSION} required. "
            "Other versions can emit other bytes. Use --any-nasm to try anyway."
        )
        return 1
    if version != NASM_VERSION:
        print(f"warning: NASM {version}; the pinned version is {NASM_VERSION}")

    errors: list = []
    check_table(manifest_scope(errors), errors)
    verified = 0
    with tempfile.TemporaryDirectory() as tmp_name:
        for number, recipe in enumerate(RECIPES):
            tmp = Path(tmp_name) / str(number)
            tmp.mkdir()
            try:
                built = build(recipe, nasm, env, tmp)
                committed = (ROOT / recipe.out).read_bytes()
            except (RuntimeError, OSError, subprocess.TimeoutExpired) as exc:
                errors.append(f"{recipe.out}: {exc}")
                continue
            if built == committed:
                verified += 1
                continue
            first = next(
                (i for i, (a, b) in enumerate(zip(built, committed)) if a != b),
                min(len(built), len(committed)),
            )
            errors.append(
                f"{recipe.out} does not match its source "
                f"(built {len(built)} bytes, committed {len(committed)}, "
                f"first difference at offset {first}); "
                f"rebuild with: {command_text(recipe)}"
            )
    if verified == 0 and not errors:
        errors.append("no binary was verified")
    for message in errors:
        print(f"check_guest_binaries: {message}")
    if errors:
        print(f"check_guest_binaries: FAILED, {verified} verified, {len(errors)} problem(s)")
        return 1
    print(
        f"check_guest_binaries: {verified} guest binaries match their sources "
        f"(NASM {version}); {len(OUT_OF_SCOPE)} out of scope"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
