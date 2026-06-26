#!/usr/bin/env python3
"""Convert the vendored MS-DOS 4.0 keyboard source into IzarraVM layout tables.

Input: the KDF<CC>.ASM country files in tools/msdos-keyboard (MIT-licensed
MS-DOS 4.0 source) plus KEYBSHAR.INC / KEYBMAC.INC for structure context.

Output:
  crates/izarravm-firmware/roms/kbd-layouts.inc      17 x 384-byte blocks +
                                                     dead-key tables
  crates/izarravm-firmware/roms/kbd-layout-meta.inc  per-layout code page byte

Each KDF file is a MASM state machine. We do not assemble it; we read the
DB/DW directives textually. A translate section is

    DW len ; DW code_page ; { state } ... ; DW 0

and a state is

    DW state_len ; DB state_id ; DW kbd_type ; DB err_lo,err_hi ; { xlat } ...

where each xlat table is

    DW tab_size ; DB options ; DB num ; <entries> ... ; DW 0

For the layouts here every populated table is STANDARD_TABLE (TYPE_2_TAB +
ASCII_ONLY): the entries are scan,char byte pairs. We select the state whose
keyboard-type mask includes G_KB (the enhanced 102-key ISO keyboard, which is
what our ES/UK/etc tables already model) or ANY_KB.

Characters in the source are raw bytes in the file's own code page. A char in a
*_437_XLAT table is a CP437 byte, in a *_850_XLAT table a CP850 byte, and so on;
COMMON tables hold low-ASCII plus a few high bytes that are valid in every code
page. We decode each char byte through the table's source code page to Unicode,
then re-encode into the target layout's code page for the emitted byte.
"""

import sys
import os

# The Windows console defaults to cp1252; force UTF-8 so log lines with accented
# characters do not raise UnicodeEncodeError.
try:
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
except Exception:
    pass

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.join(HERE, "msdos-keyboard")
ROMS = os.path.normpath(os.path.join(HERE, "..", "crates", "izarravm-firmware", "roms"))

# (index, KEYB code, KDF basename or None for US, code-page index)
LAYOUTS = [
    (0, "US", None, 0), (1, "UK", "KDFUK", 0), (2, "SP", "KDFSP", 1),
    (3, "FR", "KDFFR", 1), (4, "GR", "KDFGE", 1), (5, "IT", "KDFIT", 1),
    (6, "BE", "KDFBE", 1), (7, "CF", "KDFCF", 3), (8, "DK", "KDFDK", 4),
    (9, "NL", "KDFNL", 1), (10, "NO", "KDFNO", 4), (11, "PO", "KDFPO", 2),
    (12, "SF", "KDFSF", 1), (13, "SG", "KDFSG", 1), (14, "SU", "KDFSU", 1),
    (15, "SV", "KDFSV", 1), (16, "LA", "KDFLA", 1),
]
LAYOUT_CP = [0, 0, 1, 1, 1, 1, 1, 3, 4, 1, 4, 2, 1, 1, 1, 1, 1]
CP_NAME = {0: "cp437", 1: "cp850", 2: "cp860", 3: "cp863", 4: "cp865"}

# Keyboard type bits (KEYBSHAR.INC).
JR_KB, XT_KB, AT_KB, G_KB, P_KB, P12_KB = 0x8000, 0x4000, 0x2000, 0x1000, 0x800, 0x400
ANY_KB = 0xFFFF

# State IDs (KEYBMAC.INC).
ALPHA_LOWER, ALPHA_UPPER = 3, 4
NON_ALPHA_LOWER, NON_ALPHA_UPPER = 5, 6
THIRD_SHIFT = 7
ACUTE_LOWER, ACUTE_UPPER = 8, 9
GRAVE_LOWER, GRAVE_UPPER = 11, 12
DIARESIS_LOWER, DIARESIS_UPPER = 14, 15
CIRCUMFLEX_LOWER, CIRCUMFLEX_UPPER = 17, 18
TILDE_LOWER, TILDE_UPPER = 31, 32

# Full state-id symbol table (KEYBMAC.INC) for resolving DB <state_id> lines.
STATE_ID = {
    "DEAD_LOWER": 1, "DEAD_UPPER": 2, "ALPHA_LOWER": 3, "ALPHA_UPPER": 4,
    "NON_ALPHA_LOWER": 5, "NON_ALPHA_UPPER": 6, "THIRD_SHIFT": 7,
    "ACUTE_LOWER": 8, "ACUTE_UPPER": 9, "ACUTE_SPACE": 10,
    "GRAVE_LOWER": 11, "GRAVE_UPPER": 12, "GRAVE_SPACE": 13,
    "DIARESIS_LOWER": 14, "DIARESIS_UPPER": 15, "DIARESIS_SPACE": 16,
    "CIRCUMFLEX_LOWER": 17, "CIRCUMFLEX_UPPER": 18, "CIRCUMFLEX_SPACE": 19,
    "CEDILLA_LOWER": 20, "CEDILLA_UPPER": 21, "CEDILLA_SPACE": 22,
    "CEDILLA_CEDILLA": 23, "DEAD_THIRD": 24, "ACUTE_ACUTE": 25,
    "GRAVE_GRAVE": 26, "DIARESIS_DIARESIS": 27, "CIRCUMFLEX_CIRCUMFLEX": 28,
    "FOURTH_SHIFT": 29, "DEAD_FOURTH": 30, "TILDE_LOWER": 31, "TILDE_UPPER": 32,
    "TILDE_SPACE": 33, "ALT_CASE": 34, "CTRL_CASE": 35, "NUMERIC_PAD": 36,
    "DIVIDE_SIGN": 37, "BOTLH_CAPS": 38, "BOTRH_CAPS": 39, "BOTLH_F_CAPS": 40,
    "BOTRH_F_CAPS": 41,
}


def resolve_state_id(tok):
    """Resolve a DB state-id operand (numeric or KEYBMAC symbol)."""
    t = tok.strip()
    if t.upper() in STATE_ID:
        return STATE_ID[t.upper()]
    return parse_num(t)


# XLAT option symbols (KEYBSHAR.INC).
OPTION_SYM = {"ASCII_ONLY": 0x80, "TYPE_2_TAB": 0x40, "ZERO_SCAN": 0x20,
              "STANDARD_TABLE": 0xC0}


def resolve_options(tok):
    """Resolve a DB options operand like 'STANDARD_TABLE+ZERO_SCAN'."""
    t = tok.strip().upper().replace(" ", "")
    val = 0
    for part in t.split("+"):
        if part in OPTION_SYM:
            val |= OPTION_SYM[part]
        else:
            try:
                val |= parse_num(part)
            except ValueError:
                pass
    return val

# Accent id -> dead-key states (lower, upper) and our descriptor id.
# Accent ids match kbd-layouts.inc: 1 grave, 2 acute, 3 circumflex, 4 diaeresis,
# 5 tilde.
ACCENTS = {
    1: ("GRAVE", GRAVE_LOWER, GRAVE_UPPER),
    2: ("ACUTE", ACUTE_LOWER, ACUTE_UPPER),
    3: ("CIRCUMFLEX", CIRCUMFLEX_LOWER, CIRCUMFLEX_UPPER),
    4: ("DIARESIS", DIARESIS_LOWER, DIARESIS_UPPER),
    5: ("TILDE", TILDE_LOWER, TILDE_UPPER),
}

# Dead-key flag bits (KEYBMAC.INC) -> accent id.
FLAG_TO_ACCENT = {0x80: 2, 0x40: 1, 0x20: 4, 0x10: 3, 0x04: 5}


def src_cp_for_section(cp_id):
    """Map a translate section's code-page id to a Python codec name."""
    return {437: "cp437", 850: "cp850", 860: "cp860", 863: "cp863",
            865: "cp865"}.get(cp_id)


class Tok:
    """A flat token stream of the assembled bytes of a KDF section.

    We turn each DB/DW directive into emitted bytes. DW values that are
    label-relative expressions (len fields) are placeholders we never read as
    data; we only ever read DB scan/char bytes and the DB/DW counts we need, by
    walking the structure rather than trusting absolute offsets.
    """


def assemble_section(text, label):
    """Return the byte stream for the named PUBLIC section in `text`.

    We expand DB/DW directives between `label:` and the matching section end.
    Strings, char literals (raw bytes), numbers (decimal / 0xNN / NNh), and the
    `$`-relative length expressions are handled. Length DW fields that reference
    forward labels are emitted as a 0xFFFF placeholder; the structure walk never
    relies on them.
    """
    lines = text.split("\n")
    # Find the label line.
    start = None
    for i, ln in enumerate(lines):
        if ln.strip().startswith(label + ":") or ln.strip() == label + ":":
            start = i + 1
            break
    if start is None:
        raise KeyError(label)
    out = bytearray()
    for ln in lines[start:]:
        # Stop at the next PUBLIC section label that is not ours.
        body = ln
        # Strip comments: ; starts a comment, but ';' inside a char/str literal
        # must be kept. Handle by scanning.
        body = strip_comment(body)
        s = body.strip()
        if not s:
            continue
        # A new top-level data label like FOO_XLAT: ends nothing by itself, but
        # the END directive or a new SEGMENT does. We stop at the section's
        # terminating "DW 0 ; LAST STATE" by structure walking later, so here we
        # just keep assembling until CODE ENDS / END.
        up = s.upper()
        if up.startswith("CODE") and "ENDS" in up:
            break
        if up == "END":
            break
        # Directive?
        parts = s.split(None, 1)
        d = parts[0].upper()
        rest = parts[1] if len(parts) > 1 else ""
        if d == "DB":
            out += emit_db(rest)
        elif d == "DW":
            out += emit_dw(rest)
        # else: label or macro line inside section -> ignore (our sections are
        # pure DB/DW once past the LOGIC, which we parse separately).
    return bytes(out)


def strip_comment(line):
    """Remove a trailing ; comment, honoring '...' and "..." literals."""
    res = []
    i = 0
    n = len(line)
    while i < n:
        c = line[i]
        if c == ";":
            break
        if c in "'\"":
            q = c
            res.append(c)
            i += 1
            while i < n and line[i] != q:
                res.append(line[i])
                i += 1
            if i < n:
                res.append(line[i])
                i += 1
            continue
        res.append(c)
        i += 1
    return "".join(res)


def split_operands(rest):
    """Split a DB/DW operand list on commas, honoring char/string literals."""
    ops = []
    cur = []
    i = 0
    n = len(rest)
    while i < n:
        c = rest[i]
        if c in "'\"":
            q = c
            cur.append(c)
            i += 1
            while i < n and rest[i] != q:
                cur.append(rest[i])
                i += 1
            if i < n:
                cur.append(rest[i])
                i += 1
            continue
        if c == ",":
            ops.append("".join(cur).strip())
            cur = []
            i += 1
            continue
        cur.append(c)
        i += 1
    if "".join(cur).strip():
        ops.append("".join(cur).strip())
    return ops


def parse_num(tok):
    """Parse a numeric operand into a byte value 0..255 (two's complement)."""
    t = tok.strip()
    if t.lower().endswith("h"):
        v = int(t[:-1], 16)
    elif t.lower().startswith("0x"):
        v = int(t, 16)
    else:
        v = int(t, 10)
    return v & 0xFF


def char_byte(tok, raw_line_bytes=None):
    """Return the raw byte of a 'x' char literal. We carry raw bytes per line."""
    # tok is like "'X'"; the inner char is a single raw byte from the file.
    inner = tok[1:-1]
    if len(inner) != 1:
        # Could be an escaped quote like '''' (a quote char). Treat first byte.
        if inner == "":
            return ord("'")
    return inner.encode("latin-1")[0]


def emit_db(rest):
    out = bytearray()
    for op in split_operands(rest):
        op = op.strip()
        if not op:
            continue
        if (op.startswith("'") and op.endswith("'") and len(op) >= 2) or \
           (op.startswith('"') and op.endswith('"') and len(op) >= 2):
            inner = op[1:-1]
            if len(inner) == 1:
                out.append(inner.encode("latin-1")[0])
            else:
                # A string: emit each byte.
                out += inner.encode("latin-1")
        elif op == "$" or "-$" in op or "$-" in op or "_END" in op.upper() \
                or "_PROC" in op.upper():
            out.append(0)  # placeholder, never read as data
        else:
            out.append(parse_num(op))
    return out


def emit_dw(rest):
    out = bytearray()
    for op in split_operands(rest):
        op = op.strip()
        if not op:
            continue
        if op == "$" or "-$" in op or "$-" in op or any(
                k in op.upper() for k in ("_END", "_PROC", "ANY_KB")) \
                or _is_kbd_expr(op):
            # length/label/keyboard placeholder
            out += (0xFFFF).to_bytes(2, "little")
        else:
            try:
                v = parse_dw_num(op)
            except ValueError:
                v = 0xFFFF
            out += (v & 0xFFFF).to_bytes(2, "little")
    return out


def _is_kbd_expr(op):
    u = op.upper()
    return any(k in u for k in ("G_KB", "P12_KB", "AT_KB", "XT_KB", "JR_KB",
                                "P_KB"))


def parse_dw_num(tok):
    t = tok.strip()
    if t.lower().endswith("h"):
        return int(t[:-1], 16)
    if t.lower().startswith("0x"):
        return int(t, 16)
    return int(t, 10)


# ---------------------------------------------------------------------------
# Higher-level: read whole file with raw-byte fidelity, then walk structures by
# re-parsing the DB/DW lines directly (so char literals stay raw bytes and the
# keyboard-type DW keeps its symbolic meaning).
# ---------------------------------------------------------------------------

def load_lines(path):
    """Return the file's lines as latin-1 strings (1 byte -> 1 char)."""
    data = open(path, "rb").read()
    return data.decode("latin-1").split("\n")


def find_label(lines, label):
    for i, ln in enumerate(lines):
        st = ln.strip()
        if st == label + ":" or st.startswith(label + ":"):
            return i
    return None


def kbd_mask(expr):
    """Evaluate a keyboard-type DW operand like 'G_KB+P12_KB' to a bitmask."""
    u = expr.upper().replace(" ", "")
    if "ANY_KB" in u:
        return ANY_KB
    val = 0
    for part in u.split("+"):
        val |= {"JR_KB": JR_KB, "XT_KB": XT_KB, "AT_KB": AT_KB, "G_KB": G_KB,
                "P_KB": P_KB, "P12_KB": P12_KB}.get(part, 0)
    return val


def iter_directives(lines, start_idx):
    """Yield (kind, operands_list, raw_line) for DB/DW lines from start_idx until
    a section terminator (CODE ENDS / END)."""
    for ln in lines[start_idx:]:
        body = strip_comment(ln)
        s = body.strip()
        if not s:
            continue
        up = s.upper()
        if (up.startswith("CODE") and "ENDS" in up) or up == "END":
            return
        parts = s.split(None, 1)
        d = parts[0].upper()
        rest = parts[1] if len(parts) > 1 else ""
        if d in ("DB", "DW"):
            yield d, split_operands(rest), s


def parse_section(lines, label, src_cp):
    """Parse a translate section into {state_id: {scan: unicode_char}} using the
    G_KB / ANY_KB states only.

    Returns a dict mapping state_id -> dict(scan -> unicode char str).
    Walks the DW/DB token stream structurally.
    """
    idx = find_label(lines, label)
    if idx is None:
        raise KeyError(label)
    toks = list(iter_directives(lines, idx + 1))
    # toks is a list of (kind, ops, raw). We consume in order.
    pos = [0]

    def next_tok():
        if pos[0] >= len(toks):
            return None
        t = toks[pos[0]]
        pos[0] += 1
        return t

    def back():
        if pos[0] > 0:
            pos[0] -= 1

    # Section header: DW len ; DW cp_id
    t = next_tok()  # DW len
    t = next_tok()  # DW cp_id  (numeric or -1)
    states = {}
    while True:
        # state header begins with DW state_len; a value of 0 ends the section.
        t = next_tok()
        if t is None:
            break
        kind, ops, raw = t
        if kind != "DW":
            # stray; skip
            continue
        # Is this the terminating "DW 0"? It is when the single operand is 0.
        if len(ops) == 1 and is_zero(ops[0]):
            break
        # Otherwise it is a state_len placeholder. Next: DB state_id.
        t = next_tok()
        sid = resolve_state_id(t[1][0]) if t else 0
        # DW kbd_type
        t = next_tok()
        mask = kbd_mask(t[1][0]) if t else 0
        # DB err_lo, err_hi  (one DB with two operands, or two DBs)
        t = next_tok()
        # t is the error-char DB. Could be "DB -1,-1" (two ops) -> fine.
        want = (mask & G_KB) or (mask == ANY_KB)
        # Set-flag (dead-key) states have a different body: DW num ; { DB scan ;
        # FLAG x }. They carry no ASCII, and we handle dead keys separately, so
        # consume the body and move on. They are state ids 1, 2 and the CEDILLA
        # dead variants 20..23. They do not end with a trailing DW 0.
        # Distinguish set-flag (dead-key) states from xlat states by peeking the
        # first body DW. A set-flag table starts with 'DW <literal count>'; an
        # xlat state starts with 'DW <label>-$' (a length placeholder). The FLAG
        # macro is filtered out of the token stream, so a set-flag table
        # 'DW num ; DB scan ; FLAG x ; ...' reduces to 'DW num' then num
        # 'DB scan' tokens, with no trailing DW 0.
        peek = toks[pos[0]] if pos[0] < len(toks) else None
        if peek and peek[0] == "DW" and len(peek[1]) == 1 \
                and is_decimal(peek[1][0]):
            tnum = next_tok()  # DW num
            num = parse_num(tnum[1][0])
            for _ in range(num):
                if next_tok() is None:
                    break
            continue
        # Now read xlat tables until DW 0 (end of state).
        table = {}
        while True:
            t = next_tok()
            if t is None:
                break
            if t[0] == "DW" and len(t[1]) == 1 and is_zero(t[1][0]):
                break  # null table -> end of this state's tables
            if t[0] != "DW":
                continue
            # t is DW tab_size placeholder. Next: DB options.
            topt = next_tok()
            options = resolve_options(topt[1][0]) if topt else 0
            # DB num
            tnum = next_tok()
            num = parse_num(tnum[1][0]) if tnum else 0
            ascii_only = bool(options & 0x80)
            type2 = bool(options & 0x40)
            # Read `num` entries. With STANDARD_TABLE each entry is scan,char.
            entries_read = 0
            while entries_read < num:
                te = next_tok()
                if te is None:
                    break
                if te[0] == "DW":
                    # A DW here is the null-table terminator that ends the
                    # state. Some KDF tables declare a `num` larger than the
                    # entries actually listed (an off-by-one in the MS-DOS
                    # source). Stop at the real table boundary and let the outer
                    # loop consume the DW, rather than reading entries out of the
                    # following state and desynchronizing the whole walk.
                    back()
                    break
                if te[0] != "DB":
                    continue
                # A lone DB carrying a non-numeric symbol (a state id like
                # NON_ALPHA_UPPER) means we have run past this state's entries
                # into the next state header. Stop and rewind.
                if len(te[1]) == 1 and operand_byte(te[1][0]) is None:
                    back()
                    break
                ops_e = te[1]
                # Entries may be packed several per DB line or one per line. With
                # ASCII_ONLY each entry is scan,char (2 ops); without it (a plain
                # TYPE_2_TAB) each entry is scan,ascii,scan2 (3 ops).
                step = 3 if (type2 and not ascii_only) else 2
                j = 0
                while j + 1 < len(ops_e) + 1 and entries_read < num \
                        and j + 1 < len(ops_e):
                    scan = parse_num(ops_e[j])
                    ch_byte = operand_byte(ops_e[j + 1])
                    j += step
                    entries_read += 1
                    if want and ch_byte is not None:
                        uni = decode_char(ch_byte, src_cp)
                        if uni is not None:
                            table[scan] = uni
        if want:
            # Merge: first matching G_KB/ANY state wins per scan, but later
            # states of the same id should not normally appear for one kbd type.
            if sid not in states:
                states[sid] = {}
            for k, v in table.items():
                states[sid].setdefault(k, v)
    return states


def is_zero(op):
    op = op.strip()
    if not op:
        return False
    try:
        return parse_dw_num(op) == 0
    except ValueError:
        return False


def operand_byte(op):
    """Return the raw byte value of a DB operand (number or 'x' char)."""
    op = op.strip()
    if (op.startswith("'") and op.endswith("'")) or \
       (op.startswith('"') and op.endswith('"')):
        inner = op[1:-1]
        if len(inner) >= 1:
            return inner.encode("latin-1")[0]
        return ord("'")
    try:
        return parse_num(op)
    except ValueError:
        return None


def decode_char(byte_val, src_cp):
    """Decode a source byte (in src_cp) to a Unicode char. -1/0 -> None."""
    if byte_val in (0, 0xFF):
        return None
    try:
        return bytes([byte_val]).decode(src_cp)
    except Exception:
        return None


# ---------------------------------------------------------------------------
# LOGIC parsing: which scancode arms which accent, lower vs upper.
# ---------------------------------------------------------------------------

def parse_dead_keys(lines, prefix):
    """Return list of (scan, shift, accent_id) from the COMMON section's
    DEAD_LOWER / DEAD_UPPER set-flag tables, for G_KB only.

    The set-flag table format inside a state:
        DW num ; DB scan ; FLAG <accent> ...
    where FLAG expands to DB flag_id, DB flag_mask. We read scan + mask.
    """
    label = prefix + "_COMMON_XLAT"
    idx = find_label(lines, label)
    if idx is None:
        return []
    # Scan textually for DEAD_LOWER / DEAD_UPPER state blocks with G_KB.
    result = []
    i = idx
    n = len(lines)
    # We find "DB <ws> DEAD_LOWER" / DEAD_UPPER lines, then within that state read
    # the keyboard type and the set-flag entries.
    state_id = None
    cur_shift = None
    cur_mask_ok = False
    pending_scan = None
    while i < n:
        ln = strip_comment(lines[i])
        s = ln.strip()
        u = s.upper()
        if u.startswith("CODE") and "ENDS" in u:
            break
        if u == "END":
            break
        # Detect a DB stating a dead state id.
        m = db_single_symbol(s)
        if m in ("DEAD_LOWER", "DEAD_UPPER"):
            state_id = m
            cur_shift = 0 if m == "DEAD_LOWER" else 1
            cur_mask_ok = False
            pending_scan = None
            # next DW is keyboard type
            j = i + 1
            while j < n:
                t = strip_comment(lines[j]).strip()
                if t.upper().startswith("DW"):
                    expr = t.split(None, 1)[1] if len(t.split(None, 1)) > 1 else ""
                    mask = kbd_mask(expr)
                    cur_mask_ok = bool(mask & G_KB) or mask == ANY_KB
                    break
                j += 1
            i += 1
            continue
        if state_id and cur_mask_ok:
            # Inside a wanted dead state. Look for "DB <num>" scan then FLAG.
            if u.startswith("DB"):
                expr = s.split(None, 1)[1] if len(s.split(None, 1)) > 1 else ""
                ops = split_operands(expr)
                # A lone numeric DB is a scan code (or the leading count).
                if len(ops) == 1 and is_decimal(ops[0]):
                    pending_scan = parse_num(ops[0])
            elif u.startswith("FLAG"):
                accent_name = s.split(None, 1)[1].strip().upper() if len(s.split(None, 1)) > 1 else ""
                acc = ACCENT_NAME_TO_ID.get(accent_name)
                if acc and pending_scan is not None:
                    result.append((pending_scan, cur_shift, acc))
                    pending_scan = None
            # End of state on next "DB <state_id>" handled at top, or section end.
        i += 1
    # Deduplicate, keep first occurrence.
    seen = set()
    uniq = []
    for r in result:
        key = (r[0], r[1])
        if key in seen:
            continue
        seen.add(key)
        uniq.append(r)
    return uniq


ACCENT_NAME_TO_ID = {"GRAVE": 1, "ACUTE": 2, "CIRCUMFLEX": 3, "DIARESIS": 4,
                     "TILDE": 5, "CEDILLA": 0}


def db_single_symbol(s):
    """If line is 'DB <SYMBOL>' return the symbol uppercased, else None."""
    parts = s.split(None, 1)
    if len(parts) == 2 and parts[0].upper() == "DB":
        sym = parts[1].strip()
        if sym.replace("_", "").isalpha():
            return sym.upper()
    return None


def is_decimal(tok):
    t = tok.strip()
    return t.isdigit() or (t.startswith("-") and t[1:].isdigit())


# ---------------------------------------------------------------------------
# Build a 384-byte block (lo, hi, altgr) from parsed states.
# ---------------------------------------------------------------------------

# US block bytes copied verbatim from the existing kbd-layouts.inc.
US_LO = bytes([
    0, 27, ord('1'), ord('2'), ord('3'), ord('4'), ord('5'), ord('6'),
    ord('7'), ord('8'), ord('9'), ord('0'), ord('-'), ord('='), 8, 9,
    ord('q'), ord('w'), ord('e'), ord('r'), ord('t'), ord('y'), ord('u'),
    ord('i'), ord('o'), ord('p'), ord('['), ord(']'), 13, 0, ord('a'),
    ord('s'), ord('d'), ord('f'), ord('g'), ord('h'), ord('j'), ord('k'),
    ord('l'), ord(';'), 39, ord('`'), 0, 92, ord('z'), ord('x'), ord('c'),
    ord('v'), ord('b'), ord('n'), ord('m'), ord(','), ord('.'), ord('/'),
    0, ord('*'), 0, ord(' '),
])
US_HI = bytes([
    0, 27, ord('!'), ord('@'), ord('#'), ord('$'), ord('%'), ord('^'),
    ord('&'), ord('*'), ord('('), ord(')'), ord('_'), ord('+'), 8, 9,
    ord('Q'), ord('W'), ord('E'), ord('R'), ord('T'), ord('Y'), ord('U'),
    ord('I'), ord('O'), ord('P'), ord('{'), ord('}'), 13, 0, ord('A'),
    ord('S'), ord('D'), ord('F'), ord('G'), ord('H'), ord('J'), ord('K'),
    ord('L'), ord(':'), 34, ord('~'), 0, ord('|'), ord('Z'), ord('X'),
    ord('C'), ord('V'), ord('B'), ord('N'), ord('M'), ord('<'), ord('>'),
    ord('?'), 0, ord('*'), 0, ord(' '),
])


def encode_char(uni, cp_name, log, ctx):
    """Encode a Unicode char to one byte in cp_name; 0 and a log entry if absent."""
    if uni is None:
        return 0
    try:
        b = uni.encode(cp_name)
        if len(b) == 1:
            return b[0]
        return 0
    except Exception:
        log.append("%s: char %r (U+%04X) not in %s" %
                   (ctx, uni, ord(uni), cp_name))
        return 0


def build_block(states, cp_name, log, ctx):
    """Build (lo[128], hi[128], altgr[128]) from parsed translate states.

    Unshifted (lo) = ALPHA_LOWER + NON_ALPHA_LOWER.
    Shifted   (hi) = ALPHA_UPPER + NON_ALPHA_UPPER.
    AltGr     (altgr) = THIRD_SHIFT.

    The KDF tables only carry the keys that differ from the base US scancode
    map; everything else (letters, digits, the unchanged punctuation) we seed
    from the US base so the block is a complete layout. KDF entries then
    override per scancode.
    """
    lo = bytearray(US_LO) + bytearray(128 - len(US_LO))
    hi = bytearray(US_HI) + bytearray(128 - len(US_HI))
    altgr = bytearray(128)

    def apply(state_id, dest):
        for scan, uni in states.get(state_id, {}).items():
            if 0 <= scan < 128:
                dest[scan] = encode_char(uni, cp_name, log, ctx)

    # Non-alpha first, then alpha (alpha letters never collide with non-alpha).
    apply(NON_ALPHA_LOWER, lo)
    apply(ALPHA_LOWER, lo)
    apply(NON_ALPHA_UPPER, hi)
    apply(ALPHA_UPPER, hi)
    apply(THIRD_SHIFT, altgr)
    return bytes(lo[:128]), bytes(hi[:128]), bytes(altgr[:128])


def build_deadkey_comp(lines, prefix, src_cps, cp_name, log, ctx):
    """Build the per-accent composition map for this layout.

    Returns {accent_id: {base_char: composed_byte}} where base_char is the plain
    vowel (a/e/i/o/u/y, lower and upper) and composed_byte is encoded in cp_name.

    The composed letters come from the *_LOWER and *_UPPER accent states. The
    base char for a composed letter is recovered by Unicode NFD decomposition of
    the composed char (e.g. composed 'a-acute' -> base 'a').
    """
    import unicodedata
    # Merge COMMON + each specific section's accent states.
    sections = [(prefix + "_COMMON_XLAT", -1)]
    for cp in src_cps:
        sections.append((prefix + "_" + str(cp) + "_XLAT", cp))
    merged = {}  # state_id -> {scan: uni}
    for label, cp in sections:
        # The COMMON section (cp == -1) carries the lowercase accent states
        # (GRAVE_LOWER etc.) as raw high bytes that are valid across pages.
        # Decode them through cp437 like build_block does; passing None here
        # silently dropped every high byte, so grave-lower composition was lost.
        scp = src_cp_for_section(cp) if cp != -1 else "cp437"
        try:
            st = parse_section(lines, label, scp)
        except KeyError:
            continue
        for sid, tab in st.items():
            merged.setdefault(sid, {})
            for scan, uni in tab.items():
                # specific sections override common
                merged[sid][scan] = uni

    comp = {}
    for acc_id, (name, lo_state, up_state) in ACCENTS.items():
        m = {}
        for state in (lo_state, up_state):
            for scan, uni in merged.get(state, {}).items():
                base = base_letter(uni)
                if base is None:
                    continue
                m[base] = encode_char(uni, cp_name, log, ctx + " comp " + name)
        if m:
            comp[acc_id] = m
    return comp


def base_letter(uni):
    """Recover the unaccented base vowel for a composed letter, or None."""
    import unicodedata
    if uni is None:
        return None
    decomp = unicodedata.normalize("NFD", uni)
    base = decomp[0]
    if base in "aeiouyAEIOUY":
        return base
    return None


# ---------------------------------------------------------------------------
# Emit the .inc files.
# ---------------------------------------------------------------------------

DEAD_BASES = "aeiouyAEIOUY"
# Per-accent flush glyph (the spacing accent), encoded later per layout. We use
# the ASCII fallbacks the existing routine used: grave ` acute ' circumflex ^
# diaeresis " ; tilde ~.
FLUSH_GLYPH = {1: ord('`'), 2: ord("'"), 3: ord('^'), 4: ord('"'), 5: ord('~')}


def fmt_db(byts):
    return "    db " + ",".join(str(b) for b in byts)


def emit_layouts_inc(blocks, deadkeys, comps, out_path):
    lines = []
    A = lines.append
    A("; kbd-layouts.inc - GENERATED by tools/gen_keyboard_layouts.py")
    A("; Source: vendored MS-DOS 4.0 keyboard files (MIT) in tools/msdos-keyboard.")
    A("; Do not edit by hand; re-run the generator.")
    A(";")
    A("; 17 layouts, each one 384-byte block: 128 unshifted, 128 shifted, 128")
    A("; AltGr, indexed by Set-1 scancode. Bytes are in the layout's code page")
    A("; (see kbd-layout-meta.inc). The INT 09h ISR computes")
    A(";   base = kbd_layout_tables + layout*384 + modifier offset")
    A("; then reads [base + scancode].")
    A(";")
    A("; Layout order: 0 US 1 UK 2 ES 3 FR 4 DE 5 IT 6 BE 7 CF 8 DK 9 NL")
    A(";   10 NO 11 PT 12 SF 13 SG 14 SU 15 SV 16 LA")
    A("")
    A("align 2")
    A("kbd_layout_tables:")
    for idx, code, base, cpidx in LAYOUTS:
        lo, hi, altgr = blocks[idx]
        A("")
        A("; --- %d: %s ---" % (idx, code))
        A(".l%d_lo:" % idx)
        A(fmt_db(lo))
        A(".l%d_hi:" % idx)
        A(fmt_db(hi))
        A(".l%d_altgr:" % idx)
        A(fmt_db(altgr))
    A("")
    A("; --- Dead-key composition data (consumed by kb_deadkey) ---------------")
    A("; Accent ids: 1 grave, 2 acute, 3 circumflex, 4 diaeresis, 5 tilde.")
    A("")
    A("; Descriptor rows: layout, scancode, shift (0/1), accent id. 0xFF ends.")
    A("; kbd_deadkey_desc is what the current kb_deadkey routine reads. It is")
    A("; scoped to ES (layout 2) only, matching the original hand-authored file,")
    A("; because the routine still composes against the single ES kbd_deadkey_comp")
    A("; table below. Arming a dead key on FR/DE/etc here would compose wrong, so")
    A("; the full per-layout descriptors live in kbd_layout_deadkey_desc, which the")
    A("; later per-layout ISR task consumes alongside kbd_layout_comp_ptr.")
    A("kbd_deadkey_desc:")
    for scan, shift, acc in deadkeys.get(2, []):
        A("    db 2, 0x%02x, %d, %d   ; ES" % (scan, shift, acc))
    A("    db 0xff")
    A("")
    A("; Full per-layout dead-key descriptors for the future per-layout ISR. Same")
    A("; row shape: layout, scancode, shift, accent id; 0xFF terminates.")
    A("kbd_layout_deadkey_desc:")
    for idx, code, base, cpidx in LAYOUTS:
        for scan, shift, acc in deadkeys.get(idx, []):
            A("    db %d, 0x%02x, %d, %d   ; %s" % (idx, scan, shift, acc, code))
    A("    db 0xff")
    A("")
    A("; Spacing glyph emitted when a dead key is flushed. Indexed by accent-1.")
    A("kbd_deadkey_flush:")
    A("    db 0x%02x, 0x%02x, 0x%02x, 0x%02x, 0x%02x   ; ` ' ^ \" ~" % (
        FLUSH_GLYPH[1], FLUSH_GLYPH[2], FLUSH_GLYPH[3], FLUSH_GLYPH[4],
        FLUSH_GLYPH[5]))
    A("")
    A("; Base letters for composition columns (12 columns).")
    A("kbd_deadkey_bases:")
    A("    db " + ",".join("'%s'" % c for c in DEAD_BASES))
    A("")
    A("; Per-layout composition tables. For each layout, 5 accent rows x 12")
    A("; columns (a e i o u y A E I O U Y). 0 = no composed form (flush instead).")
    A("; kbd_layout_comp_ptr indexes these by layout; a 0 pointer means the")
    A("; layout has no dead keys.")
    A("kbd_layout_comp_ptr:")
    for idx, code, base, cpidx in LAYOUTS:
        if comps.get(idx):
            A("    dw .comp_%d" % idx)
        else:
            A("    dw 0")
    for idx, code, base, cpidx in LAYOUTS:
        cmap = comps.get(idx)
        if not cmap:
            continue
        A(".comp_%d:   ; %s" % (idx, code))
        for acc in (1, 2, 3, 4, 5):
            row = cmap.get(acc, {})
            vals = [row.get(c, 0) for c in DEAD_BASES]
            A("    db " + ",".join("0x%02x" % v for v in vals) +
              "   ; %s" % ACCENTS[acc][0].lower())
    A("")
    # Back-compat: the existing kb_deadkey routine reads kbd_deadkey_comp as a
    # single 4-row table (grave/acute/circumflex/diaeresis) for the wired
    # layouts. Emit ES's table under that name so the current routine still
    # assembles and works for ES until the ISR task switches to the per-layout
    # pointer table above.
    es_comp = comps.get(2, {})
    A("; Back-compat single table (ES) for the current kb_deadkey routine.")
    A("kbd_deadkey_comp:")
    for acc in (1, 2, 3, 4):
        row = es_comp.get(acc, {})
        vals = [row.get(c, 0) for c in DEAD_BASES]
        A("    db " + ",".join("0x%02x" % v for v in vals) +
          "   ; %s" % ACCENTS[acc][0].lower())
    A("")
    A(KB_DEADKEY_ROUTINE.rstrip("\n"))
    open(out_path, "w", newline="\n").write("\n".join(lines) + "\n")


# The dead-key state machine, copied verbatim from the original hand-authored
# kbd-layouts.inc so the two INT 09h ISRs (izbios-kbd.inc, kbd-bios-core.inc)
# that %include this file still find kb_deadkey. It reads kbd_deadkey_bases and
# the back-compat kbd_deadkey_comp table emitted above. A later ISR task can
# switch it to the per-layout kbd_layout_comp_ptr table.
KB_DEADKEY_ROUTINE = r"""
; kb_deadkey: dead-key state machine, shared by both INT 09h ISRs.
; In:  AL = layout ASCII for this key, BL = make scancode, DS = BDA segment.
; Out: CF=0 -> caller enqueues AL as usual (AL may be a composed char).
;      CF=1 -> caller enqueues nothing (key swallowed or fully handled here).
kb_deadkey:
    push cx
    push si
    push di
    cmp byte [KB_DEAD], 0
    jne .have_pending
    call .lookup_dead          ; -> AH = accent id (0 if not dead)
    test ah, ah
    jz .pass
    mov [KB_DEAD], ah
    jmp .swallow
.have_pending:
    call .lookup_dead          ; -> AH = accent id (0 if not dead)
    test ah, ah
    jz .compose
    push ax
    call .flush
    pop ax
    mov [KB_DEAD], ah
    jmp .swallow
.compose:
    movzx di, byte [KB_DEAD]
    dec di
    mov si, kbd_deadkey_bases
    xor cx, cx
.col_scan:
    cmp cx, 12
    jae .no_match
    mov ah, [cs:si]
    cmp ah, al
    je .col_found
    inc cx
    inc si
    jmp .col_scan
.col_found:
    ; CX = column (0..11), DI = accent row (0..4), BL = make scancode. The
    ; composition table is per layout: kbd_layout_comp_ptr[KB_LAYOUT] points at a
    ; 5-row (grave/acute/circumflex/diaeresis/tilde) x 12-column block, or 0 when
    ; the layout has no dead keys.
    push bx                         ; save make scancode
    movzx bx, byte [KB_LAYOUT]
    shl bx, 1
    mov bx, [cs:kbd_layout_comp_ptr + bx]
    test bx, bx
    jz .col_found_none
    mov si, di
    imul si, si, 12                 ; SI = accent row * 12
    add si, cx                      ; SI += column
    add si, bx                      ; SI += per-layout table base
    mov ah, [cs:si]
    pop bx                          ; restore make scancode
    test ah, ah
    jz .no_match
    mov al, ah
    mov byte [KB_DEAD], 0
    jmp .pass
.col_found_none:
    pop bx                          ; layout has no composition table
    jmp .no_match
.no_match:
    push ax
    call .flush
    pop ax
    mov byte [KB_DEAD], 0
    cmp al, ' '
    je .swallow
    jmp .pass
.pass:
    pop di
    pop si
    pop cx
    clc
    ret
.swallow:
    pop di
    pop si
    pop cx
    stc
    ret
.lookup_dead:
    cmp byte [KB_ALTGR], 0
    jne .ld_none
    mov ah, 0
    test byte [KB_FLAGS], 0x03
    jz .ld_noshift
    mov ah, 1
.ld_noshift:
    mov cl, [KB_LAYOUT]
    mov si, kbd_layout_deadkey_desc
.ld_loop:
    mov ch, [cs:si]
    cmp ch, 0xff
    je .ld_none
    cmp ch, cl
    jne .ld_next
    mov ch, [cs:si + 1]
    cmp ch, bl
    jne .ld_next
    mov ch, [cs:si + 2]
    cmp ch, ah
    jne .ld_next
    mov ah, [cs:si + 3]
    ret
.ld_next:
    add si, 4
    jmp .ld_loop
.ld_none:
    xor ah, ah
    ret
.flush:
    movzx si, byte [KB_DEAD]
    dec si
    mov al, [cs:kbd_deadkey_flush + si]
    mov ah, bl
    call kb_enqueue
    ret
"""


def emit_meta_inc(out_path):
    lines = []
    lines.append("; kbd-layout-meta.inc - GENERATED by tools/gen_keyboard_layouts.py")
    lines.append("; Per-layout code page index: 0=437 1=850 2=860 3=863 4=865.")
    lines.append("; Sub-project A code-page font order.")
    lines.append("kbd_layout_codepage: db " + ",".join(str(c) for c in LAYOUT_CP))
    open(out_path, "w", newline="\n").write("\n".join(lines) + "\n")


# ---------------------------------------------------------------------------
# Validation gate.
# ---------------------------------------------------------------------------

# Each entry is (layout_index, scancode, shifted, expected_unicode). Every
# value here was read back from the KDF source for that layout (see the per-key
# notes); the generator decodes the emitted block at that scancode through the
# layout's code page and asserts equality.
EXPECT = [
    (1, 0x2b, False, "#"),    # UK: # on the ISO key US uses for backslash
    (2, 0x27, False, "ñ"),    # ES: n-tilde (ALPHA_LOWER scan 39)
    (3, 0x10, False, "a"),    # FR AZERTY: a where US has q
    (3, 0x1e, False, "q"),    # FR AZERTY: q where US has a
    (4, 0x15, False, "z"),    # DE QWERTZ: z where US has y
    (4, 0x2c, False, "y"),    # DE QWERTZ: y where US has z
    (5, 0x27, False, "ò"),    # IT: o-grave (NON_ALPHA_LOWER scan 39)
    (6, 0x10, False, "a"),    # BE AZERTY: a where US has q
    (7, 0x35, False, "é"),    # CF: e-acute on ALPHA_LOWER scan 53 (the . key)
    (8, 0x1a, False, "å"),    # DK: a-ring (scan 26)
    (9, 0x10, False, "q"),    # NL: QWERTY q (sanity anchor; NL is US-like base)
    (10, 0x1a, False, "å"),   # NO: a-ring (scan 26)
    (11, 0x27, False, "ç"),   # PO: c-cedilla (ALPHA_LOWER scan 39)
    (12, 0x1a, False, "è"),   # SF (Swiss French): e-grave (scan 26)
    (13, 0x15, False, "z"),   # SG (Swiss German) QWERTZ: z where US has y
    (14, 0x1a, False, "å"),   # SU (Finnish): a-ring (ALPHA_LOWER scan 26)
    (15, 0x1a, False, "å"),   # SV (Swedish): a-ring (ALPHA_LOWER scan 26)
    (16, 0x27, False, "ñ"),   # LA (Latin American): n-tilde (scan 39)
]

# Detailed Spanish checks mirroring the hand-verified machine tests
# (es_layout_fills_ordinal_and_iso_and_cedilla, es_dead_keys_compose_accents,
# int16_resident_keyboard_uses_bios_layout_byte). The sparse EXPECT gate above
# missed real converter bugs in the ES block and dead-key composition, so these
# rows pin the standard Spanish keys directly against the same oracle the Rust
# tests use. Layout 2 = ES, code page CP850.

# (scan, shifted, expected_unicode) against the lo/hi blocks of layout 2.
EXPECT_ES_BLOCK = [
    (0x29, False, "º"),   # ordinal key (left of 1): masculine ordinal
    (0x29, True, "ª"),    # shift -> feminine ordinal
    (0x56, False, "<"),   # ISO 102nd key: less-than
    (0x56, True, ">"),    # shift -> greater-than
    (0x2b, False, "ç"),   # cedilla key
    (0x2b, True, "Ç"),    # shift -> capital cedilla
    (0x0d, False, "¡"),   # inverted exclamation
    (0x0d, True, "¿"),    # shift -> inverted question
    (0x33, False, ","),   # comma
    (0x33, True, ";"),    # shift -> semicolon
    (0x27, False, "ñ"),   # n-tilde
    (0x27, True, "Ñ"),    # shift -> capital n-tilde
]

# (scan, expected_unicode) against the AltGr block of layout 2.
EXPECT_ES_ALTGR = [
    (0x29, "\\"),         # AltGr+ordinal key -> backslash
    (0x2b, "}"),          # AltGr+cedilla key -> right brace
]

# (accent_id, base_char, expected_composed_unicode) against the ES composition
# table. Accent ids: 1 grave, 2 acute, 4 diaeresis.
EXPECT_ES_COMPOSE = [
    (2, "a", "á"),        # acute + a
    (1, "e", "è"),        # grave + e
    (4, "u", "ü"),        # diaeresis + u
    (1, "a", "à"),        # grave + a (lowercase, the one the old bug dropped)
    (2, "E", "É"),        # acute + E (uppercase path)
]

# ES dead-key descriptor rows the machine relies on: (scan, shift, accent_id).
EXPECT_ES_DEADKEYS = [
    (0x1a, 0, 1),         # grave armed unshifted
    (0x28, 0, 2),         # acute armed unshifted
    (0x1a, 1, 3),         # circumflex armed shifted
    (0x28, 1, 4),         # diaeresis armed shifted
]


def decode_block_char(block, scan, shifted, cp_name):
    lo, hi, altgr = block
    arr = hi if shifted else lo
    b = arr[scan]
    if b == 0:
        return None
    try:
        return bytes([b]).decode(cp_name)
    except Exception:
        return None


def main():
    log = []
    blocks = {}
    deadkeys = {}
    comps = {}

    for idx, code, base, cpidx in LAYOUTS:
        cp_name = CP_NAME[cpidx]
        if base is None:
            # US: copy verbatim.
            lo = bytes(US_LO) + bytes(128 - len(US_LO))
            hi = bytes(US_HI) + bytes(128 - len(US_HI))
            blocks[idx] = (lo, hi, bytes(128))
            continue
        path = os.path.join(SRC, base + ".ASM")
        lines = load_lines(path)
        prefix = detect_prefix(lines)
        src_cps = detect_src_cps(lines, prefix)
        # Merge COMMON + each specific section into one state set.
        merged = {}
        # Parse COMMON (low ASCII, no source cp -> treat as cp437 for any high
        # bytes that appear, which are valid across pages).
        for label, cp in [(prefix + "_COMMON_XLAT", None)] + \
                [(prefix + "_" + str(c) + "_XLAT", c) for c in src_cps]:
            scp = src_cp_for_section(cp) if cp not in (None, -1) else "cp437"
            try:
                st = parse_section(lines, label, scp)
            except KeyError:
                continue
            for sid, tab in st.items():
                merged.setdefault(sid, {})
                for scan, uni in tab.items():
                    merged[sid][scan] = uni  # specific overrides common
        ctx = code
        block = build_block(merged, cp_name, log, ctx)
        blocks[idx] = block
        dk = parse_dead_keys(lines, prefix)
        deadkeys[idx] = dk
        comps[idx] = build_deadkey_comp(lines, prefix, src_cps, cp_name, log, ctx)

    # Emit.
    emit_layouts_inc(blocks, deadkeys, comps,
                     os.path.join(ROMS, "kbd-layouts.inc"))
    emit_meta_inc(os.path.join(ROMS, "kbd-layout-meta.inc"))

    # Validation.
    failures = []
    for idx, scan, shifted, expected in EXPECT:
        cp_name = CP_NAME[LAYOUT_CP[idx]]
        got = decode_block_char(blocks[idx], scan, shifted, cp_name)
        if got != expected:
            failures.append("layout %d (%s) scan 0x%02x shift=%s: expected %r "
                            "got %r" % (idx, LAYOUTS[idx][1], scan, shifted,
                                        expected, got))

    # Detailed ES checks. Layout 2 is Spanish, code page CP850.
    es_cp = CP_NAME[LAYOUT_CP[2]]
    es_lo, es_hi, es_altgr = blocks[2]
    extra = 0
    for scan, shifted, expected in EXPECT_ES_BLOCK:
        got = decode_block_char(blocks[2], scan, shifted, es_cp)
        extra += 1
        if got != expected:
            failures.append("ES scan 0x%02x shift=%s: expected %r got %r"
                            % (scan, shifted, expected, got))
    for scan, expected in EXPECT_ES_ALTGR:
        b = es_altgr[scan]
        got = bytes([b]).decode(es_cp) if b else None
        extra += 1
        if got != expected:
            failures.append("ES AltGr scan 0x%02x: expected %r got %r"
                            % (scan, expected, got))
    es_comp = comps.get(2, {})
    for acc, base, expected in EXPECT_ES_COMPOSE:
        b = es_comp.get(acc, {}).get(base, 0)
        got = bytes([b]).decode(es_cp) if b else None
        extra += 1
        if got != expected:
            failures.append("ES compose accent %d + %r: expected %r got %r"
                            % (acc, base, expected, got))
    es_dk = set(deadkeys.get(2, []))
    for row in EXPECT_ES_DEADKEYS:
        extra += 1
        if row not in es_dk:
            failures.append("ES dead-key descriptor missing: scan 0x%02x "
                            "shift=%d accent=%d" % row)

    if log:
        print("Encoding notes (char absent from code page -> emitted 0):")
        for entry in sorted(set(log)):
            print("  " + entry)

    if failures:
        print("\nVALIDATION FAILED:")
        for f in failures:
            print("  " + f)
        sys.exit(1)

    print("\nAll %d EXPECT checks passed (+%d detailed ES checks)."
          % (len(EXPECT), extra))
    print("Wrote %s" % os.path.join(ROMS, "kbd-layouts.inc"))
    print("Wrote %s" % os.path.join(ROMS, "kbd-layout-meta.inc"))


def detect_prefix(lines):
    """Find the 2-letter section prefix from the first PUBLIC <P>_LOGIC line."""
    import re
    for ln in lines:
        m = re.search(r"PUBLIC\s+([A-Z]{2})_LOGIC", ln)
        if m:
            return m.group(1)
    raise RuntimeError("no _LOGIC prefix found")


def detect_src_cps(lines, prefix):
    """Return the list of specific code pages declared, e.g. [437, 850]."""
    import re
    cps = []
    for ln in lines:
        m = re.search(r"PUBLIC\s+" + prefix + r"_(\d{3})_XLAT", ln)
        if m:
            cp = int(m.group(1))
            if cp not in cps:
                cps.append(cp)
    return cps


if __name__ == "__main__":
    main()
