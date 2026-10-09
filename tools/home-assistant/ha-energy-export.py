#!/usr/bin/env python3
"""Hourly energy history from Home Assistant's long-term statistics, as
JSON lines for `moli-os energy import`.

Reads HA's database read-only (no HA token, nothing written there).
Usage: HA_DB=<HA config>/home-assistant_v2.db ha-energy-export.py <meter id>=<statistic id> ... > rows.jsonl
  e.g. linky-hc=sensor.linky_hchc
"""
import json
import os
import sqlite3
import sys

DB = f"file:{os.environ.get('HA_DB', 'home-assistant_v2.db')}?mode=ro"
HOUR = 3_600_000
FACTOR = {"kWh": 1.0, "Wh": 0.001, "MWh": 1000.0}


def export(db, meter, statistic):
    meta = db.execute(
        "SELECT id, unit_of_measurement FROM statistics_meta WHERE statistic_id = ? AND has_sum = 1",
        (statistic,),
    ).fetchone()
    if not meta or meta[1] not in FACTOR:
        print(f"{statistic}: not an energy statistic with a sum", file=sys.stderr)
        return 0
    factor = FACTOR[meta[1]]
    rows = db.execute(
        "SELECT start_ts, sum FROM statistics WHERE metadata_id = ? AND sum IS NOT NULL ORDER BY start_ts",
        (meta[0],),
    ).fetchall()
    count = 0
    for (t0, s0), (t1, s1) in zip(rows, rows[1:]):
        delta = (s1 - s0) * factor
        if delta < 0:  # HA corrected its sum: no credible figure
            continue
        # A row's sum is cumulative at the end of its hour; after a gap
        # (HA stopped), the energy is spread over the missing hours.
        h0 = int(round(t0 * 1000)) // HOUR * HOUR + HOUR
        h1 = int(round(t1 * 1000)) // HOUR * HOUR
        hours = max(1, (h1 - h0) // HOUR + 1)
        for i in range(hours):
            print(json.dumps({"meter": meter, "hour": h0 + i * HOUR if hours > 1 else h1, "kwh": round(delta / hours, 6)}))
            count += 1
    print(f"{meter} <- {statistic}: {count} hours", file=sys.stderr)
    return count


def main():
    pairs = [a.split("=", 1) for a in sys.argv[1:]]
    if not pairs or any(len(p) != 2 for p in pairs):
        sys.exit(__doc__)
    db = sqlite3.connect(DB, uri=True, timeout=30)
    for meter, statistic in pairs:
        export(db, meter, statistic)


if __name__ == "__main__":
    main()
