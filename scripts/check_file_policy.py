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

# Line ceilings, counted on CODE lines only: comment-only lines are free, so a
# well-documented module is never pushed over the edge by its documentation.
# Large focused files are fine here; the limits exist to catch runaway growth,
# not to force splitting a hot module. Keep code free of duplication and do not
# shred logic into tiny indirection layers to fit: this codebase optimizes for
# performance and locality, not layer count. Dense, long systems still deserve
# their own files along with their tests.
SOURCE_LINE_LIMIT = 5000
TEST_LINE_LIMIT = 7000

# Comment syntax per suffix for the code-line count: (line prefixes, block pairs).
HASH_COMMENTS = ((("#",), ()),)
C_COMMENTS = ((("//",), (("/*", "*/"),)),)
ASM_COMMENTS = (((";",), ()),)
COMMENT_SYNTAX = {
    ".rs": C_COMMENTS[0],
    ".js": C_COMMENTS[0],
    ".jsx": C_COMMENTS[0],
    ".ts": C_COMMENTS[0],
    ".tsx": C_COMMENTS[0],
    ".c": C_COMMENTS[0],
    ".cc": C_COMMENTS[0],
    ".cpp": C_COMMENTS[0],
    ".cxx": C_COMMENTS[0],
    ".h": C_COMMENTS[0],
    ".hh": C_COMMENTS[0],
    ".hpp": C_COMMENTS[0],
    ".hxx": C_COMMENTS[0],
    ".m": C_COMMENTS[0],
    ".ld": ((), (("/*", "*/"),)),
    ".rc": C_COMMENTS[0],
    ".asm": ASM_COMMENTS[0],
    ".inc": ASM_COMMENTS[0],
    ".nas": ASM_COMMENTS[0],
    ".s": ((";", "#"), (("/*", "*/"),)),
    ".py": HASH_COMMENTS[0],
    ".pl": HASH_COMMENTS[0],
    ".pm": HASH_COMMENTS[0],
    ".sh": HASH_COMMENTS[0],
    ".mk": HASH_COMMENTS[0],
    ".mak": HASH_COMMENTS[0],
    ".ps1": (("#",), (("<#", "#>"),)),
    ".bat": (("rem ", "rem\t", "::"), ()),
    ".cmd": (("rem ", "rem\t", "::"), ()),
    ".btm": (("rem ", "rem\t", "::"), ()),
}


def code_line_count(lines: list[str], path: str) -> int:
    """Count lines that carry code: comment-only lines are excluded.

    A line is a comment line when it is inside a block comment, or its stripped
    text starts with a line-comment marker, or it starts a block comment and
    nothing follows the closer on the closing line. Markers inside string
    literals mid-line never start block state because only lines BEGINNING with
    the opener enter it; the rare miss counts the line as code, which errs
    toward the stricter side of the gate.
    """
    item = PurePosixPath(path)
    syntax = COMMENT_SYNTAX.get(item.suffix.lower())
    if syntax is None and item.name.lower() in SOURCE_CODE_NAMES:
        syntax = HASH_COMMENTS[0]
    if syntax is None:
        return len(lines)
    line_prefixes, block_pairs = syntax
    count = 0
    closer: str | None = None
    for line in lines:
        stripped = line.strip()
        lowered = stripped.lower()
        if closer is not None:
            end = stripped.find(closer)
            if end < 0:
                continue
            rest = stripped[end + len(closer) :].strip()
            closer = None
            if rest:
                count += 1
            continue
        if line_prefixes and lowered.startswith(tuple(line_prefixes)):
            continue
        opened = False
        for opener, block_closer in block_pairs:
            if stripped.startswith(opener):
                tail = stripped[len(opener) :]
                end = tail.find(block_closer)
                while end >= 0:
                    rest = tail[end + len(block_closer) :].strip()
                    if not rest.startswith(opener):
                        break
                    tail = rest[len(opener) :]
                    end = tail.find(block_closer)
                if end < 0:
                    closer = block_closer
                    opened = True
                else:
                    rest = tail[end + len(block_closer) :].strip()
                    if rest:
                        count += 1
                    opened = True
                break
        if opened:
            continue
        count += 1
    return count


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
            limit = TEST_LINE_LIMIT if is_test_file(path) else SOURCE_LINE_LIMIT
            code_lines = code_line_count(lines, path)
            if code_lines > limit:
                errors.append(
                    f"{path}: {code_lines} code lines exceeds the {limit}-line limit"
                )
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
