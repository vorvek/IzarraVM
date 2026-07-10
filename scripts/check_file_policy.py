#!/usr/bin/env python3
# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only

"""Check source headers, the license manifest, and source-file layout limits."""

from __future__ import annotations

import csv
import hashlib
import re
import subprocess
import sys
from pathlib import Path, PurePosixPath


ROOT = Path(__file__).resolve().parents[1]
MANIFEST_PATH = "LICENSE_MANIFEST.tsv"
MANIFEST_HEADER = ["path", "class", "origin", "license", "sha256"]
NOTICE = "This file is part of IzarraVM and is licensed under GNU GPL version 3 only."
SPDX = "SPDX-License-Identifier: GPL-3.0-only"
FORBIDDEN = b"pony" + b"tail:"

MODERN_DOS_FONTS = {
    "tools/fonts/ModernDOS8x8.ttf",
    "tools/fonts/ModernDOS8x14.ttf",
    "tools/fonts/ModernDOS8x16.ttf",
}
GENERATED_TEXT = {
    "crates/izarravm-firmware/roms/boot-suite/results.inc",
    "crates/izarravm-firmware/roms/izbios-art.inc",
    "crates/izarravm-firmware/roms/izbios-logo.inc",
    "crates/izarravm-firmware/roms/kbd-layout-meta.inc",
    "crates/izarravm-firmware/roms/kbd-layouts.inc",
}
BINARY_SUFFIXES = {
    ".bin",
    ".com",
    ".exe",
    ".gz",
    ".ico",
    ".img",
    ".jpg",
    ".json",
    ".lock",
    ".ovl",
    ".png",
    ".rgba",
    ".sf3",
    ".sys",
    ".ttf",
}
SOURCE_CODE_SUFFIXES = {
    ".asm",
    ".bat",
    ".btm",
    ".c",
    ".cc",
    ".cmd",
    ".cpp",
    ".cxx",
    ".h",
    ".hh",
    ".hpp",
    ".hxx",
    ".inc",
    ".js",
    ".jsx",
    ".ld",
    ".m",
    ".mak",
    ".mk",
    ".nas",
    ".pl",
    ".pm",
    ".ps1",
    ".py",
    ".rc",
    ".rs",
    ".s",
    ".sh",
    ".ts",
    ".tsx",
}
SOURCE_CODE_NAMES = {"makefile"}


def tracked_files() -> list[str]:
    result = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
    )
    return sorted(result.stdout.decode("utf-8").rstrip("\0").split("\0"))


def is_vendor(path: str) -> bool:
    if path.startswith("toka-dos/freedos/"):
        return path != "toka-dos/freedos/VENDOR.md"
    if path.startswith("crates/izarravm-native-synth/vendor/"):
        return path != "crates/izarravm-native-synth/vendor/SOURCES.md"
    if path.startswith("third_party/"):
        return not path.endswith("/SOURCE.md")
    if path.startswith("tools/msdos-keyboard/"):
        return path != "tools/msdos-keyboard/VENDOR.md"
    return path in MODERN_DOS_FONTS


def header_for(path: str) -> tuple[str, str] | None:
    item = PurePosixPath(path)
    suffix = item.suffix.lower()
    name = item.name.lower()
    if suffix in {".rs", ".js", ".ts"}:
        return f"// {NOTICE}", f"// {SPDX}"
    if suffix in {".c", ".cc", ".cpp", ".h", ".hpp", ".ld", ".rc"}:
        return f"/* {NOTICE} */", f"/* {SPDX} */"
    if suffix in {".asm", ".inc", ".nas"}:
        return f"; {NOTICE}", f"; {SPDX}"
    if suffix in {".bat", ".cmd"}:
        return f"REM {NOTICE}", f"REM {SPDX}"
    if (
        suffix in {".mk", ".mak", ".pl", ".ps1", ".py", ".sh", ".toml", ".yaml", ".yml"}
        or name in {".editorconfig", ".gitattributes", ".gitignore", "makefile"}
        or path == "docs/requirements.txt"
    ):
        return f"# {NOTICE}", f"# {SPDX}"
    if suffix in {".html", ".md", ".svg", ".xml"}:
        return f"<!-- {NOTICE} -->", f"<!-- {SPDX} -->"
    return None


def header_offset(lines: list[str], path: str) -> int:
    offset = 1 if lines and lines[0].startswith("#!/") else 0
    if PurePosixPath(path).suffix.lower() == ".py":
        if offset < len(lines) and re.search(r"coding[:=]", lines[offset]):
            offset += 1
    while offset < len(lines):
        line = lines[offset].lstrip().lower()
        if line.startswith("<?xml ") or line.startswith("<!doctype "):
            offset += 1
        else:
            break
    return offset


def is_plain_license(path: str) -> bool:
    item = PurePosixPath(path)
    name = item.name.lower()
    return (
        "/licenses/" in f"/{path}"
        or "license" in name
        or name.startswith(("copying", "notice"))
    )


def needs_manifest(path: str, data: bytes) -> bool:
    if path == MANIFEST_PATH:
        return False
    if path in GENERATED_TEXT:
        return True
    if not is_vendor(path):
        return header_for(path) is None
    suffix = PurePosixPath(path).suffix.lower()
    return suffix in BINARY_SUFFIXES or b"\0" in data or is_plain_license(path)


def is_test_file(path: str) -> bool:
    item = PurePosixPath(path)
    return "tests" in item.parts or item.stem.endswith("_test") or item.name.startswith("test_")


def is_source_code(path: str) -> bool:
    item = PurePosixPath(path)
    return item.suffix.lower() in SOURCE_CODE_SUFFIXES or item.name.lower() in SOURCE_CODE_NAMES


def read_manifest(errors: list[str]) -> list[list[str]]:
    path = ROOT / MANIFEST_PATH
    if not path.is_file():
        errors.append(f"missing {MANIFEST_PATH}")
        return []
    with path.open("r", encoding="utf-8", newline="") as handle:
        rows = list(csv.reader(handle, delimiter="\t"))
    if not rows or rows[0] != MANIFEST_HEADER:
        errors.append(f"{MANIFEST_PATH}: expected tab-separated header {MANIFEST_HEADER}")
        return []
    for number, row in enumerate(rows[1:], 2):
        if len(row) != len(MANIFEST_HEADER):
            errors.append(f"{MANIFEST_PATH}:{number}: expected five tab-separated fields")
    return [row for row in rows[1:] if len(row) == len(MANIFEST_HEADER)]


def check_manifest(files: list[str], data: dict[str, bytes], errors: list[str]) -> int:
    expected = {path for path in files if needs_manifest(path, data[path])}
    rows = read_manifest(errors)
    paths = [row[0] for row in rows]
    if paths != sorted(paths):
        errors.append(f"{MANIFEST_PATH}: paths are not sorted")
    if len(paths) != len(set(paths)):
        errors.append(f"{MANIFEST_PATH}: duplicate path")
    missing = sorted(expected - set(paths))
    extra = sorted(set(paths) - expected)
    for path in missing:
        errors.append(f"{MANIFEST_PATH}: missing {path}")
    for path in extra:
        errors.append(f"{MANIFEST_PATH}: unexpected {path}")
    for path, kind, origin, license_name, digest in rows:
        if not kind or not origin or not license_name:
            errors.append(f"{MANIFEST_PATH}: incomplete metadata for {path}")
        if path in data:
            actual = hashlib.sha256(data[path]).hexdigest()
            if digest != actual:
                errors.append(f"{MANIFEST_PATH}: SHA-256 mismatch for {path}")
    return len(expected)


def main() -> int:
    errors: list[str] = []
    files = tracked_files()
    data = {path: (ROOT / path).read_bytes() for path in files}
    project_headers = 0
    vendor_exemptions = 0
    test_attribute = re.compile(
        r"(?m)^\s*#\s*\[\s*(?:[A-Za-z0-9_:]+::)?test(?:\s*\([^]]*\))?\s*\]"
    )

    for path in files:
        raw = data[path]
        if FORBIDDEN in raw.lower():
            errors.append(f"{path}: forbidden marker")
        if is_vendor(path):
            if not needs_manifest(path, raw):
                vendor_exemptions += 1
            continue
        header = header_for(path)
        if header is None:
            continue
        project_headers += 1
        try:
            text = raw.decode("utf-8-sig")
        except UnicodeDecodeError:
            errors.append(f"{path}: project source is not UTF-8")
            continue
        lines = text.splitlines()
        offset = header_offset(lines, path)
        if tuple(lines[offset : offset + 2]) != header:
            errors.append(f"{path}: missing exact GPL-3.0-only header")
        if path not in GENERATED_TEXT and is_source_code(path):
            limit = 2500 if is_test_file(path) else 3000
            if len(lines) > limit:
                errors.append(f"{path}: {len(lines)} lines exceeds the {limit}-line limit")
        if PurePosixPath(path).suffix.lower() == ".rs" and not is_test_file(path):
            if test_attribute.search(text):
                errors.append(f"{path}: inline test body belongs in a *_test.rs file")

    manifest_entries = check_manifest(files, data, errors)
    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1
    print(
        f"file policy ok: {project_headers} project headers, "
        f"{vendor_exemptions} vendor text exemptions, {manifest_entries} manifest entries"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
