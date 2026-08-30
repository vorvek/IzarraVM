# This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
# SPDX-License-Identifier: GPL-3.0-only
"""Print the corpus evidence block for one result directory.

Usage: python summarize-run.py <result_dir>

Reads profile.json and run-meta.json and prints the fields the ledger and
the performance campaign want, including the top barrier-census rows by
dynamic_unbound_exits when the census ran.
"""

import json
import sys
from pathlib import Path


def main() -> int:
    result_dir = Path(sys.argv[1])
    profile = json.loads((result_dir / "profile.json").read_text())
    meta = json.loads((result_dir / "run-meta.json").read_text(encoding="utf-8-sig"))

    perf = profile.get("perf", {})
    timer = profile.get("timer", {})
    guest_seconds = profile.get("guest_seconds", 0.0)

    print(f"game={meta.get('game')} slug={meta.get('slug')} label={meta.get('label')}")
    print(f"cpu={meta.get('cpu')} exe_sha256={meta.get('exe_sha256', '')[:12]}")
    print(f"real_time_factor={profile.get('real_time_factor')}")
    print(f"direct_native_coverage={profile.get('direct_native_coverage')}")
    print(f"guest_seconds={guest_seconds}")
    print(f"stop={profile.get('stop')}")
    print(f"instructions={perf.get('instructions')}")
    print(f"brk_cont_not_continuable={perf.get('brk_cont_not_continuable')}")
    pit_writes = timer.get("pit_writes")
    if pit_writes is not None and guest_seconds:
        rate = pit_writes / guest_seconds
        storm = "  <-- LATCH-POLL STORM" if rate > 300_000 else ""
        print(f"pit_writes={pit_writes} ({rate:.1f}/guest-s){storm}")
    print(f"irq0_edges={timer.get('irq0_edges')}")
    print(
        f"video mode={profile.get('mode')} legacy={profile.get('legacy_video_mode')} "
        f"display={profile.get('active_display')}"
    )

    census = profile.get("direct_barrier_census")
    if census:
        rows = census.get("rows", []) if isinstance(census, dict) else census
        rows = sorted(rows, key=lambda r: r.get("dynamic_unbound_exits", 0), reverse=True)
        total = sum(r.get("dynamic_unbound_exits", 0) for r in rows)
        print(f"census rows={len(rows)} total_unbound={total}")
        for r in rows[:5]:
            print(
                f"  op={r['opcode']:#04x} reg={r.get('modrm_reg')} form={r['operand_form']}"
                f" asz={r['address_size']} unbound={r['dynamic_unbound_exits']}"
                f" hits={r['hits']} stop={r.get('stop_reason')}"
            )
    return 0


if __name__ == "__main__":
    sys.exit(main())
