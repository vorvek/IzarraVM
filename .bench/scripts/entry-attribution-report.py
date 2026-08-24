#!/usr/bin/env python3
# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
"""Acceptance report for the 16-bit Direct entry-attribution observer.

Reads the profile JSON that `izarravm --profile-json` writes in a build carrying the
`direct-entry-attribution` feature, and prints:

  * the per-phase table (ns per entry, per lane), with the resolution floor applied;
  * the A1 closures, per population, CUMULATIVE, with the residual reported and never absorbed;
  * A2's comparand `E` against the 200 ns model output, stated as the model's tolerance;
  * the A3 count identities against counters that exist;
  * A4 / A5 / A6 when the corresponding legs are supplied;
  * the section 6 fit -- the primary `linked_transfers == 0 && !self_loop` stratum, then the
    two-term cross-check with its intercept CI and condition number;
  * `F = E - B` and the pre-registered verdict.

No third-party dependency: the weighted least squares, the CIs and the condition number are all
computed here in plain Python, so the report runs anywhere the fixture does.

Usage:
    entry-attribution-report.py --full FULL.json
                                [--coarse COARSE.json] [--sample SAMPLE_N.json]
                                [--disarmed DISARMED.json] [--plain PLAIN.json]
"""

import argparse
import json
import math
import sys

MODEL_NS = 200.0
MODEL_TOLERANCE = 0.25
SIXTEEN_BIT_LANES = ("sixteen_bit", "v86_sixteen")

# ----------------------------------------------------------------------------------------------
# tiny linear algebra (weighted least squares on a design matrix with an explicit intercept)
# ----------------------------------------------------------------------------------------------


def _solve(matrix, rhs):
    """Gauss-Jordan with partial pivoting. Returns None on a singular system."""
    n = len(matrix)
    aug = [list(matrix[i]) + [rhs[i]] for i in range(n)]
    for col in range(n):
        pivot = max(range(col, n), key=lambda r: abs(aug[r][col]))
        if abs(aug[pivot][col]) < 1e-15:
            return None
        aug[col], aug[pivot] = aug[pivot], aug[col]
        scale = aug[col][col]
        aug[col] = [value / scale for value in aug[col]]
        for row in range(n):
            if row == col:
                continue
            factor = aug[row][col]
            if factor == 0.0:
                continue
            aug[row] = [a - factor * b for a, b in zip(aug[row], aug[col])]
    return [aug[i][n] for i in range(n)]


def _inverse(matrix):
    n = len(matrix)
    columns = []
    for i in range(n):
        unit = [1.0 if j == i else 0.0 for j in range(n)]
        column = _solve(matrix, unit)
        if column is None:
            return None
        columns.append(column)
    return [[columns[j][i] for j in range(n)] for i in range(n)]


def _symmetric_eigenvalues(matrix):
    """Jacobi eigenvalues of a small symmetric matrix, for the condition number."""
    n = len(matrix)
    a = [list(row) for row in matrix]
    for _ in range(200):
        off = 0.0
        p = q = 0
        for i in range(n):
            for j in range(i + 1, n):
                if abs(a[i][j]) > off:
                    off = abs(a[i][j])
                    p, q = i, j
        if off < 1e-14:
            break
        theta = 0.5 * math.atan2(2.0 * a[p][q], a[q][q] - a[p][p])
        c, s = math.cos(theta), math.sin(theta)
        for k in range(n):
            akp = c * a[k][p] - s * a[k][q]
            akq = s * a[k][p] + c * a[k][q]
            a[k][p], a[k][q] = akp, akq
        for k in range(n):
            apk = c * a[p][k] - s * a[q][k]
            aqk = s * a[p][k] + c * a[q][k]
            a[p][k], a[q][k] = apk, aqk
    return sorted(abs(a[i][i]) for i in range(n))


def wls(points, n_terms):
    """`points` = list of (weight, [regressors...], y). Returns a dict or None.

    The design matrix always carries an explicit intercept column, so `n_terms` counts the
    regressors and the fit has `n_terms + 1` parameters.
    """
    rows = [(w, [1.0] + list(x), y) for (w, x, y) in points if w > 0]
    k = n_terms + 1
    if len(rows) <= k:
        return None
    xtx = [[0.0] * k for _ in range(k)]
    xty = [0.0] * k
    for w, x, y in rows:
        for i in range(k):
            xty[i] += w * x[i] * y
            for j in range(k):
                xtx[i][j] += w * x[i] * x[j]
    beta = _solve(xtx, xty)
    if beta is None:
        return None
    inv = _inverse(xtx)
    if inv is None:
        return None
    ss_res = 0.0
    weight_total = 0.0
    y_mean = sum(w * y for w, _, y in rows) / sum(w for w, _, _ in rows)
    ss_tot = 0.0
    for w, x, y in rows:
        fitted = sum(b * xi for b, xi in zip(beta, x))
        ss_res += w * (y - fitted) ** 2
        ss_tot += w * (y - y_mean) ** 2
        weight_total += w
    dof = len(rows) - k
    sigma2 = ss_res / dof if dof > 0 else float("nan")
    se = [math.sqrt(max(sigma2 * inv[i][i], 0.0)) for i in range(k)]
    eigenvalues = _symmetric_eigenvalues(xtx)
    condition = (
        math.sqrt(eigenvalues[-1] / eigenvalues[0]) if eigenvalues[0] > 0 else float("inf")
    )
    return {
        "beta": beta,
        "se": se,
        "n": len(rows),
        "dof": dof,
        "r2": 1.0 - ss_res / ss_tot if ss_tot > 0 else float("nan"),
        "condition_number": condition,
        "weight_total": weight_total,
    }


# ----------------------------------------------------------------------------------------------
# JSON access
# ----------------------------------------------------------------------------------------------


def load(path):
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


def attribution(report, path):
    block = report.get("direct_entry_attribution")
    if block is None:
        sys.exit(f"{path}: no `direct_entry_attribution` object -- was this an observer build?")
    if block is None or block == {}:
        sys.exit(f"{path}: the observer was never armed")
    return block


def lane(block, name):
    for entry in block["lanes"]:
        if entry["lane"] == name:
            return entry
    sys.exit(f"lane {name!r} not present")


def phase(lane_block, name_prefix):
    for entry in lane_block["phases"]:
        if entry["phase"].startswith(name_prefix + "_"):
            return entry
    sys.exit(f"phase {name_prefix!r} not present")


def total(lane_block, population):
    for entry in lane_block["totals"]:
        if entry["population"] == population:
            return entry
    sys.exit(f"population {population!r} not present")


def perf(report, key):
    return report["perf"].get(key, 0)


# ----------------------------------------------------------------------------------------------
# report sections
# ----------------------------------------------------------------------------------------------


def section(title):
    print()
    print("=" * 96)
    print(title)
    print("=" * 96)


def phase_table(block, lane_name):
    lane_block = lane(block, lane_name)
    entered_marks = phase(lane_block, "P8")["marks"]
    entries = entered_marks / 2.0 if entered_marks else 0.0
    print()
    print(f"lane {lane_name}: {entries:,.0f} entered traversals (marks(P8) / 2)")
    print(
        f"  {'phase':<24}{'marks':>14}{'ticks_raw':>16}{'ns':>16}"
        f"{'ns/entry':>12}{'ns/mark':>12}  note"
    )
    floor = block["resolution_floor_ns"]
    for entry in lane_block["phases"]:
        per_entry = entry["ns"] / entries if entries else float("nan")
        note = "below resolution" if entry["below_resolution"] else ""
        print(
            f"  {entry['phase']:<24}{entry['marks']:>14,}{entry['ticks_raw']:>16,}"
            f"{entry['ns']:>16,.0f}{per_entry:>12.2f}{entry['ns_per_mark']:>12.2f}  {note}"
        )
    print(f"  resolution floor: {floor:.2f} ns/mark (overhead {block['overhead_ticks']} ticks)")
    return lane_block, entries


def arm_difference_mark_cost(full, coarse, lane_name):
    """Per-mark cost measured END TO END, from the FULL/COARSE difference.

    The arm-time calibration cannot be trusted in a release build: it brackets a real
    `accumulate()` between two `rdtsc` reads, and with optimisation on, the three reads pipeline
    so heavily that the marginal cost measures near zero (2 ticks on this host, against a debug
    build's 16). Subtracting 2 ticks leaves each phase carrying a whole `rdtsc` it did not do any
    work for.

    This measurement has no such problem, because nothing about it is a micro-benchmark: an
    entered traversal takes 15 marks in the FULL arm and 2 in COARSE, and the two arms run the
    same guest to the same frame hash. The difference in `total_entered` per entry, over 13 marks,
    is what one mark costs where it actually sits.

    Returns `(ns_per_mark, full_per_entry, coarse_per_entry)`, or None without a COARSE leg.
    """
    if coarse is None:
        return None
    full_lane, coarse_lane = lane(full, lane_name), lane(coarse, lane_name)
    entries = phase(full_lane, "P8")["marks"] // 2
    if entries == 0:
        return None
    if phase(coarse_lane, "P8")["marks"] != entries:
        return None
    full_per_entry = total(full_lane, "entered")["ns"] / entries
    coarse_per_entry = total(coarse_lane, "entered")["ns"] / entries
    # P0..P11 with P4, P5 and P8 stamped twice; COARSE keeps only the two native-window marks.
    full_marks, coarse_marks = 15, 2
    ns = (full_per_entry - coarse_per_entry) / (full_marks - coarse_marks)
    return ns, full_per_entry, coarse_per_entry


def corrected_phase_table(full, coarse, lane_name):
    """The per-phase table with the arm-difference per-mark cost removed."""
    measured = arm_difference_mark_cost(full, coarse, lane_name)
    if measured is None:
        print()
        print("  (no COARSE leg: cannot measure the per-mark cost end to end)")
        return
    ns_per_mark, full_per_entry, coarse_per_entry = measured
    lane_block = lane(full, lane_name)
    entries = phase(lane_block, "P8")["marks"] // 2
    print()
    print(f"  CORRECTED per-phase table, lane {lane_name}, {entries:,} entered traversals")
    print(f"    per-mark cost {ns_per_mark:.2f} ns, measured as "
          f"({full_per_entry:.2f} - {coarse_per_entry:.2f}) / 13 marks -- NOT the arm-time")
    print(f"    calibration, which reads {full['overhead_ticks']} ticks "
          f"({full['resolution_floor_ns']:.2f} ns) and is not believable in a release build.")
    print(f"    {'phase':<24}{'marks/entry':>13}{'raw ns/entry':>14}{'instrument':>12}"
          f"{'corrected':>12}")
    corrected_total = 0.0
    rows = []
    for entry in lane_block["phases"][:12]:
        marks_per_entry = entry["marks"] / entries
        raw = entry["ticks_raw"] / full["tsc_hz"] * 1e9 / entries
        instrument = ns_per_mark * marks_per_entry
        value = raw - instrument
        corrected_total += value
        rows.append((entry["phase"], marks_per_entry, raw, instrument, value))
        print(f"    {entry['phase']:<24}{marks_per_entry:>13.3f}{raw:>14.2f}"
              f"{instrument:>12.2f}{value:>12.2f}")
    print(f"    {'TOTAL P0..P11':<24}{'':>13}{'':>14}{'':>12}{corrected_total:>12.2f}")
    print(f"    cross-check: COARSE total {coarse_per_entry:.2f} minus its own two marks "
          f"({2 * ns_per_mark:.2f}) = {coarse_per_entry - 2 * ns_per_mark:.2f} ns/entry")
    print("    NOTE: P0..P5's ns/entry divides work that REFUSED traversals also did by the")
    print("    ENTERED count, so those five read high against COARSE's entered-only pre-native.")
    return rows, corrected_total, ns_per_mark


def closures(block, lane_name):
    lane_block = lane(block, lane_name)
    ns = {entry["phase"].split("_")[0]: entry["ns"] for entry in lane_block["phases"]}
    hz = block["tsc_hz"]
    raw = {
        entry["phase"].split("_")[0]: entry["ticks_raw"] / hz * 1e9 if hz else float("nan")
        for entry in lane_block["phases"]
    }

    def cumulative(last):
        return sum(ns[f"P{i}"] for i in range(0, last + 1))

    def cumulative_raw(last):
        return sum(raw[f"P{i}"] for i in range(0, last + 1))

    print()
    print("  A1 closure, per population, CUMULATIVE (P14 excluded from every numerator):")
    verdict = True
    for label, upto, population in (
        ("entered ", 11, "entered"),
        ("refused ", 12, "refused"),
        ("fallback", 13, "fallback"),
    ):
        denominator = total(lane_block, population)["ns"]
        numerator = cumulative(upto)
        ratio = numerator / denominator if denominator else float("nan")
        ok = ratio >= 0.90
        verdict = verdict and (ok or math.isnan(ratio))
        raw_numerator = cumulative_raw(upto)
        raw_ratio = raw_numerator / denominator if denominator else float("nan")
        print(
            f"    sum(P0..P{upto})/total_{label} = {numerator:>16,.0f} / {denominator:>16,.0f}"
            f" = {ratio:6.3f}   residual {denominator - numerator:>14,.0f} ns"
            f"   [{'PASS' if ok else 'FAIL'}]"
        )
        # The corrected numerator subtracts `overhead x marks`; the raw one subtracts nothing. When
        # the two disagree by more than the residual, the CALIBRATION is what failed, not the
        # closure -- which is why the design keeps the raw sums alongside.
        print(
            f"      on RAW ticks (no subtraction)      {raw_numerator:>16,.0f}"
            f" / {denominator:>16,.0f} = {raw_ratio:6.3f}"
        )
    compile_total = total(lane_block, "compile")["ns"]
    print(
        f"    P14 / total_compile           = {ns['P14']:>16,.0f} / {compile_total:>16,.0f}"
        f" = {(ns['P14'] / compile_total) if compile_total else float('nan'):6.3f}"
        "   (own denominator, outside every closure)"
    )
    return verdict


def a2_coarse(coarse):
    """E measured in the COARSE arm: four marks per entry instead of sixteen.

    The design defines E on the armed leg without subtracting the instrument's own cost from
    `total_entered`. FULL carries >=16 marks inside that span, so its E is inflated by exactly the
    marks A6 is meant to bound. COARSE is the same measurement with 14 of them removed, and is
    therefore the honest comparand; it also decomposes the entry into pre-native / native / tail
    with no other stamp in the way.
    """
    entered_ns = 0.0
    entries = 0
    parts = []
    for name in SIXTEEN_BIT_LANES:
        lane_block = lane(coarse, name)
        marks_p8 = phase(lane_block, "P8")["marks"]
        if marks_p8 == 0:
            continue
        lane_total = total(lane_block, "entered")["ns"]
        pre = phase(lane_block, "P8")["ns"]
        native = phase(lane_block, "P9")["ns"]
        entered_ns += lane_total
        entries += marks_p8
        parts.append((name, marks_p8, lane_total, pre, native))
    print()
    print("  COARSE-arm comparand (four marks per entry, so ~14 fewer stamps inside the span):")
    for name, n, lane_total, pre, native in parts:
        print(
            f"    lane {name:<14}{n:>12,} entries   total {lane_total / n:7.1f} ns/entry"
            f"   pre-native {pre / n:6.1f}   native {native / n:6.1f}"
            f"   tail {(lane_total - pre - native) / n:6.1f}"
        )
    e = entered_ns / entries if entries else float("nan")
    low, high = MODEL_NS * (1 - MODEL_TOLERANCE), MODEL_NS * (1 + MODEL_TOLERANCE)
    ok = low <= e <= high
    print(f"    E_coarse = {e:.2f} ns/entry   band [{low:.1f}, {high:.1f}] -> "
          f"[{'PASS' if ok else 'FAIL'}]")
    return e, ok


def a2(block, report):
    print()
    print("  A2 comparand (the 200 ns figure is a MODEL OUTPUT; +/-25% is its tolerance, not an")
    print("     independent measurement):")
    entered_ns = 0.0
    entries = 0
    for name in SIXTEEN_BIT_LANES:
        lane_block = lane(block, name)
        entered_ns += total(lane_block, "entered")["ns"]
        entries += phase(lane_block, "P8")["marks"] // 2
    counter_entries = perf(report, "jit_direct_entries_sixteen_bit")
    e = entered_ns / entries if entries else float("nan")
    low, high = MODEL_NS * (1 - MODEL_TOLERANCE), MODEL_NS * (1 + MODEL_TOLERANCE)
    ok = low <= e <= high
    print(f"    E = total_entered_16 / entries_16 = {e:.2f} ns/entry over {entries:,} entries")
    print(f"    jit_direct_entries_sixteen_bit    = {counter_entries:,}")
    print(f"    band [{low:.1f}, {high:.1f}] -> [{'PASS' if ok else 'FAIL'}]")
    if not ok:
        print("    A2 FAILED: the model was wrong. Every lever priced against 200 ns is re-priced")
        print("    before anything is built (section 9 falsifier).")
    return e, ok


def a3(block, report):
    print()
    print("  A3 count identities, against counters that exist:")
    marks_p0 = sum(phase(lane(block, n["lane"]), "P0")["marks"] for n in block["lanes"])
    marks_p8 = sum(phase(lane(block, n["lane"]), "P8")["marks"] for n in block["lanes"])
    marks_p13 = sum(phase(lane(block, n["lane"]), "P13")["marks"] for n in block["lanes"])
    marks_p14 = sum(phase(lane(block, n["lane"]), "P14")["marks"] for n in block["lanes"])
    above_p0 = 0
    refusals = 0
    for entry in block["lanes"]:
        for site in entry["refusal_site"]:
            refusals += site["count"]
            if site["above_p0_mark"]:
                above_p0 += site["count"]
    declined = skipped = 0
    for entry in block["lanes"]:
        for tag in entry["fallback_tags"]:
            if tag["tag"] == "declined":
                declined += tag["count"]
            elif tag["tag"] == "skipped":
                skipped += tag["count"]
    heat_demote = 0
    for entry in block["lanes"]:
        for site in entry["compile_site"]:
            if site["site"] == "heat_demote":
                heat_demote += site["count"]

    results = []

    def check(label, left, right, exact=True):
        ok = left == right if exact else left >= right
        results.append(ok)
        relation = "==" if exact else ">="
        print(
            f"    {label:<58}{left:>16,} {relation} {right:>16,}   [{'PASS' if ok else 'FAIL'}]"
        )

    probes = perf(report, "decode_probes")
    # `seam_probes` is bumped at run.rs:737 and `begin()` is at 807, so a traversal that ends the
    # run on the decode screen between them counts a probe and never reaches the P0 mark. There
    # are exactly four such breaks (run.rs:791-812) and all four are counted by the three
    # `brk_cont_*` keys below. The design's A3 form omits this term; it is 16.5% of probes on the
    # loader, so the identity is unsatisfiable without it.
    # ...and MINUS the fifth `brk_cont_decode_miss` site, which sits BELOW `begin()`
    # (run.rs:857-862, the late view miss). That one bumped the counter on a traversal that DID
    # reach the P0 mark, so leaving it in double-counts. The observer exports the subtrahend
    # itself so this does not have to reach into another object.
    late_view_miss = block.get("decode_pack_late_view_miss", 0)
    screen_breaks = (
        perf(report, "brk_cont_not_continuable")
        + perf(report, "brk_cont_page_cross")
        + perf(report, "brk_cont_decode_miss")
        - late_view_miss
    )
    print(f"    (literal form) marks(P0) vs decode_probes:  {marks_p0:,} vs {probes:,}")
    print("      marks(P0) is at run.rs:1418; eight refusal sites return above it and four decode")
    print("      screen breaks end the run above `begin()`, so the exact identity carries both.")
    print(
        f"      refusals above the P0 mark {above_p0:,}; screen breaks {screen_breaks:,}"
        f" (late view misses subtracted: {late_view_miss:,})"
    )
    check(
        "marks(P0) + refusals above P0 + screen breaks == decode_probes",
        marks_p0 + above_p0 + screen_breaks,
        probes,
    )
    check("marks(P8) == 2 x jit_direct_entries", marks_p8, 2 * perf(report, "jit_direct_entries"))
    check(
        "marks(P13) == jit_direct_dispatch_declines + skips",
        marks_p13,
        perf(report, "jit_direct_dispatch_declines") + skipped,
    )
    check(
        "site tag declines == jit_direct_dispatch_declines",
        declined,
        perf(report, "jit_direct_dispatch_declines"),
    )
    print(f"    skips (production counts these nowhere)                   {skipped:>16,}")
    check(
        "marks(P14) == jit_direct_compile_attempts + compile_site[1512]",
        marks_p14,
        perf(report, "jit_direct_compile_attempts") + heat_demote,
    )
    ticks_p14_ns = sum(phase(lane(block, n["lane"]), "P14")["ns"] for n in block["lanes"])
    compile_ns = perf(report, "jit_direct_compile_ns")
    gap = ticks_p14_ns - compile_ns
    ok = gap >= 0
    results.append(ok)
    print(
        f"    ticks(P14) >= jit_direct_compile_ns                       "
        f"{ticks_p14_ns:>16,.0f} >= {compile_ns:>16,}   [{'PASS' if ok else 'FAIL'}]"
    )
    print(
        f"      gap P14 - compile_ns = {gap:,.0f} ns  (arm prologue 1503-1514, install, fast-map"
        " fill, sweep, heat/lane bookkeeping)"
    )
    if gap < 0:
        print("      NEGATIVE GAP -- falsifies the P14 placement (design A3).")
    print(f"    sum(refusal_site) over all lanes                          {refusals:>16,}")
    print(
        "      the two returns production counts nowhere (run.rs:2294, run.rs:2566) have"
        " expected value 0:"
    )
    for entry in block["lanes"]:
        for site in entry["refusal_site"]:
            if site["site"] in ("segment_layout_none", "block_regenerated_none"):
                print(
                    f"        lane {entry['lane']}: {site['site']} = {site['count']:,}"
                    "   (NON-ZERO; the histogram is what makes this visible)"
                )
    return all(results)


def a4(full, disarmed):
    print()
    print("  A4 state pins, armed vs disarmed on the SAME observer binary:")
    ok = True
    for key, getter in (
        ("perf.instructions", lambda r: r["perf"].get("instructions")),
        ("elapsed_clocks", lambda r: r.get("elapsed_clocks")),
        ("master_ticks", lambda r: r.get("master_ticks")),
        ("frame_hash", lambda r: r.get("frame_hash") or r.get("framebuffer_hash")),
        ("stop_reason", lambda r: json.dumps(r.get("stop_reason"), sort_keys=True)),
    ):
        left, right = getter(full), getter(disarmed)
        if left is None and right is None:
            print(f"    {key:<24}(absent from both profiles)")
            continue
        same = left == right
        ok = ok and same
        print(f"    {key:<24}{str(left):>24} vs {str(right):>24}   [{'PASS' if same else 'FAIL'}]")
    return ok


def a5(observer_disarmed, plain):
    print()
    print("  A5 plain-build identity (observer binary DISARMED vs plain build):")
    left = observer_disarmed["perf"]
    right = plain["perf"]
    left_keys, right_keys = list(left.keys()), list(right.keys())
    ordered = left_keys == right_keys
    print(
        f"    perf key set identical AND identically ordered: "
        f"{len(left_keys)} vs {len(right_keys)} keys   [{'PASS' if ordered else 'FAIL'}]"
    )
    if not ordered:
        print(f"      only in observer: {sorted(set(left_keys) - set(right_keys))}")
        print(f"      only in plain:    {sorted(set(right_keys) - set(left_keys))}")
    banded = {"jit_direct_compile_ns", "jit_direct_arena_compaction_ns"}
    mismatches = []
    for key in left_keys:
        if key in banded or key not in right:
            continue
        if left[key] != right[key]:
            mismatches.append((key, left[key], right[key]))
    print(
        f"    every counter identical except the host-time pair: "
        f"{len(mismatches)} mismatch(es)   [{'PASS' if not mismatches else 'FAIL'}]"
    )
    for key, a, b in mismatches[:20]:
        print(f"      {key}: {a} vs {b}")
    for key in sorted(banded & set(left_keys) & set(right_keys)):
        a, b = left[key], right[key]
        band = abs(a - b) / max(a, b, 1)
        print(f"    band {key}: {a:,} vs {b:,}  ({band * 100:.1f}% apart)")
    top = list(observer_disarmed.keys())
    plain_top = list(plain.keys())
    extra = [k for k in top if k not in plain_top]
    print(f"    top-level keys only in the observer profile: {extra}")
    return ordered and not mismatches


def a6(full, coarse, sampled):
    print()
    print("  A6 self-check:")
    ok = True
    if coarse is not None:
        for name in SIXTEEN_BIT_LANES:
            f_total = total(lane(full, name), "entered")["ns"]
            c_total = total(lane(coarse, name), "entered")["ns"]
            if f_total == 0 and c_total == 0:
                continue
            delta = abs(f_total - c_total) / max(f_total, c_total, 1.0)
            passed = delta <= 0.05
            ok = ok and passed
            print(
                f"    COARSE vs FULL total_entered, lane {name:<14}"
                f"{c_total:>16,.0f} vs {f_total:>16,.0f}  ({delta * 100:5.2f}%)"
                f"   [{'PASS' if passed else 'FAIL'}]"
            )
    else:
        print("    COARSE leg not supplied")
    if sampled is not None:
        print(f"    SAMPLE={sampled['sample_n']} shares vs SAMPLE={full['sample_n']} shares:")
        for name in SIXTEEN_BIT_LANES:
            f_lane, s_lane = lane(full, name), lane(sampled, name)
            f_sum = sum(p["ns"] for p in f_lane["phases"][:12])
            s_sum = sum(p["ns"] for p in s_lane["phases"][:12])
            if f_sum <= 0 or s_sum <= 0:
                continue
            worst = 0.0
            worst_phase = ""
            for f_phase, s_phase in zip(f_lane["phases"][:12], s_lane["phases"][:12]):
                delta = abs(f_phase["ns"] / f_sum - s_phase["ns"] / s_sum) * 100
                if delta > worst:
                    worst, worst_phase = delta, f_phase["phase"]
            passed = worst <= 3.0
            ok = ok and passed
            print(
                f"      lane {name:<14} worst share drift {worst:5.2f} pp on {worst_phase}"
                f"   [{'PASS' if passed else 'FAIL'}]"
            )
    else:
        print("    SAMPLE leg not supplied")
    return ok


def fit(block, lane_name):
    lane_block = lane(block, lane_name)
    tsc_hz = block["tsc_hz"]

    def to_ns(ticks):
        return ticks / tsc_hz * 1e9 if tsc_hz else float("nan")

    primary, two_term, self_loop = [], [], []
    dropped = 0
    for row in lane_block["native_bins"]:
        count = row["count"]
        if count < 1000:
            dropped += 1
            continue
        y = to_ns(row["ticks"] / count)
        x1 = row["instructions_sum"] / count
        x2 = row["linked_transfers_sum"] / count
        if row["self_loop"]:
            self_loop.append((count, [x1], y))
            continue
        two_term.append((count, [x1, x2], y))
        if row["linked_transfers_class"] == 0:
            primary.append((count, [x1], y))

    print()
    print(f"  section 6 fit, lane {lane_name} (bins under 1000 samples dropped: {dropped})")

    def show(label, result, names):
        if result is None:
            print(f"    {label}: not identifiable (too few bins)")
            return None
        beta, se = result["beta"], result["se"]
        print(
            f"    {label}: n={result['n']} bins, weight={result['weight_total']:,.0f}, "
            f"R2={result['r2']:.4f}, condition={result['condition_number']:.1f}"
        )
        for name, value, error in zip(names, beta, se):
            lo, hi = value - 1.96 * error, value + 1.96 * error
            print(
                f"      {name:<10}{value:>12.3f} ns  SE {error:>10.3f}  95% CI"
                f" [{lo:>10.3f}, {hi:>10.3f}]"
            )
        return result

    print("    PRIMARY = the linked_transfers == 0 && !self_loop stratum (no collinearity at all);")
    print("    the two-term fit is the CROSS-CHECK and the only source of c.")
    primary_fit = show("primary  ", wls(primary, 1), ["a (intercept)", "b (per insn)"])
    two_fit = show("two-term ", wls(two_term, 2), ["a (intercept)", "b (per insn)", "c (per hop)"])
    show("self-loop", wls(self_loop, 1), ["a (intercept)", "b (per insn)"])

    stops = []
    if primary_fit is not None:
        a_value, a_se = primary_fit["beta"][0], primary_fit["se"][0]
        ci_width = 2 * 1.96 * a_se
        wider = ci_width > abs(a_value)
        stops.append(("intercept CI wider than a itself", wider))
        print(
            f"    STOP 1 -- intercept CI width {ci_width:.3f} vs |a| {abs(a_value):.3f}: "
            f"{'TRIPPED (the split is NOT established)' if wider else 'clear'}"
        )
    if two_fit is not None:
        condition = two_fit["condition_number"]
        tripped = condition > 30.0
        stops.append(("two-term condition number > 30", tripped))
        print(
            f"    STOP 2 -- two-term condition number {condition:.1f}: "
            f"{'TRIPPED (c is UNIDENTIFIED, not used)' if tripped else 'clear'}"
        )

    # mean regressors over the whole non-self-loop population, for B.
    total_count = sum(r["count"] for r in lane_block["native_bins"] if not r["self_loop"])
    mean_insns = (
        sum(r["instructions_sum"] for r in lane_block["native_bins"] if not r["self_loop"])
        / total_count
        if total_count
        else float("nan")
    )
    mean_hops = (
        sum(r["linked_transfers_sum"] for r in lane_block["native_bins"] if not r["self_loop"])
        / total_count
        if total_count
        else float("nan")
    )
    print(f"    mean(exit.instructions) = {mean_insns:.3f}, mean(linked_transfers) = {mean_hops:.3f}")
    return primary_fit, two_fit, mean_insns, mean_hops, stops


def verdict(e, primary_fit, two_fit, mean_insns, mean_hops, block, lane_name, a1_ok, a2_ok, stops):
    section("PRE-REGISTERED DECISION RULE (design section 9)")
    if primary_fit is None:
        print("  no primary fit -> B is not computable; the ladder stops here.")
        return
    b = primary_fit["beta"][1]
    c = two_fit["beta"][2] if two_fit is not None else float("nan")
    c_unidentified = any(name.startswith("two-term") and tripped for name, tripped in stops)
    c_used = 0.0 if (c_unidentified or math.isnan(c)) else c
    big = b * mean_insns + c_used * mean_hops
    f_value = e - big
    print(f"  E = {e:.2f} ns/entry")
    print(
        f"  B = b*mean(insns) + c*mean(hops) = {b:.3f}*{mean_insns:.3f}"
        f" + {c_used:.3f}*{mean_hops:.3f} = {big:.2f} ns/entry"
    )
    print(f"  F = E - B = {f_value:.2f} ns/entry")
    lane_block = lane(block, lane_name)
    entered = phase(lane_block, "P8")["marks"] / 2.0
    ranked = []
    for entry in lane_block["phases"][:12]:
        ranked.append((entry["ns"] / entered if entered else 0.0, entry["phase"]))
    ranked.sort(reverse=True)
    print("  phases ranked by ns/entry:")
    for value, name in ranked:
        print(f"    {name:<26}{value:8.2f} ns/entry")
    if not a2_ok:
        print("  VERDICT: STOPPED. A2 failed -- nothing is built against a model number the")
        print("  instrument contradicts; the loader entry cost is re-derived first.")
        return
    if not a1_ok:
        print("  VERDICT: STOPPED. A1 failed per population -- the split is not established.")
        return
    if any(tripped for name, tripped in stops if name.startswith("intercept")):
        print("  VERDICT: STOPPED. The section 6 intercept CI is wider than a itself.")
        return
    p10_p11 = sum(
        entry["ns"] / entered
        for entry in lane_block["phases"]
        if entry["phase"].startswith(("P10_", "P11_"))
    )
    if f_value >= 120.0:
        print("  VERDICT: CUT ENTRY COST (F >= 120).")
        if p10_p11 >= 40.0:
            print(
                f"    SUBORDINATE OVERRIDE: P10+P11 = {p10_p11:.2f} ns/entry >= 40, so take the"
                " batchable half of P10+P11 first"
            )
            print(
                "    (the perf counter block run.rs:2888-2910 and the unresolved / side-exit"
                " matches 2911-3010 -- and nothing else: the membership test is"
                " 'does not reach trace.add_elapsed_clocks')."
            )
        else:
            print(f"    Order: largest phase first -> {ranked[0][1]} at {ranked[0][0]:.2f} ns/entry")
    elif f_value <= 60.0:
        print("  VERDICT: CUT ENTRY COUNT (F <= 60). The entry is mostly real native work; the S4")
        print("  extension program continues and this instrument is retired. No override applies.")
    else:
        top_value, top_name = ranked[0]
        if top_value >= 35.0:
            print(f"  VERDICT: 60 < F < 120 -> take {top_name} ({top_value:.2f} ns/entry >= 35).")
        else:
            print(
                f"  VERDICT: 60 < F < 120 and the largest phase is only {top_value:.2f} ns/entry"
                " (< 35) -> CUT ENTRY COUNT."
            )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--full", required=True, help="observer build, FULL arm, SAMPLE=1")
    parser.add_argument("--coarse", help="observer build, COARSE arm")
    parser.add_argument("--sample", help="observer build, FULL arm, SAMPLE=N")
    parser.add_argument("--disarmed", help="observer build with the knob OFF (A4/A5 comparand)")
    parser.add_argument("--plain", help="plain build (no feature) -- A5's other side")
    parser.add_argument("--lane", default="sixteen_bit", help="lane for the headline tables")
    args = parser.parse_args()

    full_report = load(args.full)
    full = attribution(full_report, args.full)

    section("ARM AND CALIBRATION")
    print(
        f"  arm={full['arm']}  sample_n={full['sample_n']}  tsc_hz={full['tsc_hz']:,}"
        f"  overhead={full['overhead_ticks']} ticks"
        f"  resolution floor={full['resolution_floor_ns']:.2f} ns"
    )
    print(
        f"  outliers: {full['outlier_marks']:,} clamped marks shed {full['outlier_ticks']:,}"
        f" ticks (clamp {full['outlier_clamp_ticks']:,}); P14 is EXEMPT from the clamp (M-R4)"
    )
    print(f"  lane pin mismatches (H9): {full['lane_pin_mismatches']:,}")

    coarse_block_early = attribution(load(args.coarse), args.coarse) if args.coarse else None

    section("PER-PHASE TABLE")
    for name in full["lane_names"]:
        entered = phase(lane(full, name), "P8")["marks"]
        if entered == 0 and sum(p["marks"] for p in lane(full, name)["phases"]) == 0:
            continue
        phase_table(full, name)

    section("CORRECTED PER-PHASE TABLE (arm-difference calibration)")
    corrected_phase_table(full, coarse_block_early, args.lane)

    section("ACCEPTANCE CHECKS")
    a1_ok = closures(full, args.lane)
    e, a2_ok = a2(full, full_report)
    coarse_block = coarse_block_early
    if coarse_block is not None:
        e_coarse, a2_coarse_ok = a2_coarse(coarse_block)
        e, a2_ok = e_coarse, a2_coarse_ok
        print("    The verdict below uses E_coarse: it is the same quantity with the instrument's")
        print("    own marks mostly removed, and the design's E has no instrument correction.")
    a3_ok = a3(full, full_report)
    a4_ok = None
    a5_ok = None
    if args.disarmed:
        a4_ok = a4(full_report, load(args.disarmed))
        if args.plain:
            a5_ok = a5(load(args.disarmed), load(args.plain))
    a6_ok = a6(
        full,
        coarse_block,
        attribution(load(args.sample), args.sample) if args.sample else None,
    )

    section("SECTION 6 FIT")
    primary_fit, two_fit, mean_insns, mean_hops, stops = fit(full, args.lane)

    verdict(e, primary_fit, two_fit, mean_insns, mean_hops, full, args.lane, a1_ok, a2_ok, stops)

    section("SUMMARY")
    for label, value in (
        ("A1 closure", a1_ok),
        ("A2 comparand", a2_ok),
        ("A3 counts", a3_ok),
        ("A4 state pins", a4_ok),
        ("A5 plain identity", a5_ok),
        ("A6 self-check", a6_ok),
    ):
        state = "not run" if value is None else ("PASS" if value else "FAIL")
        print(f"  {label:<22}{state}")


if __name__ == "__main__":
    main()
