#!/usr/bin/env python3
"""Rooms from Home Assistant's areas, for Moli devices that have none.

Reads HA's registries (read-only), matches each HA device to its Moli
device by identity, and proposes `room = <HA area>` for Moli devices with
no room yet (no user label, no room from their own system). Nothing is
ever moved out of a room.

    ha-areas-import.py            # dry run: prints the plan
    ha-areas-import.py --apply    # writes the labels through Moli's API
"""
import json
import subprocess
import sys
import urllib.request

MOLI = "http://127.0.0.1:8790"


def ha(name):
    raw = subprocess.check_output(["docker", "exec", "homeassistant", "cat", f"/config/.storage/{name}"])
    return json.loads(raw)["data"]


def main(apply):
    areas = {a["id"]: a["name"] for a in ha("core.area_registry")["areas"]}
    devices = ha("core.device_registry")["devices"]
    moli = json.load(urllib.request.urlopen(f"{MOLI}/api/devices", timeout=10))
    moli = moli if isinstance(moli, list) else moli["devices"]
    by_native = {}
    for d in moli:
        native = d["id"].split(":", 1)[1].lower()
        by_native.setdefault(native, d)

    def match(device):
        for kind, value in device.get("identifiers") or []:
            value = str(value)
            candidates = [value, value.split(":")[-1], value.rsplit("_", 1)[-1]]
            if kind == "mqtt" and value.startswith("zigbee2mqtt_"):
                candidates.insert(0, value[len("zigbee2mqtt_"):])
            for c in candidates:
                if c.lower() in by_native:
                    return by_native[c.lower()]
        macs = [v.replace(":", "").lower() for k, v in device.get("connections") or [] if k == "mac"]
        for m in macs:
            if m in by_native:
                return by_native[m]
        return None

    plan = []
    for device in devices:
        area = areas.get(device.get("area_id"))
        if not area:
            continue
        target = match(device)
        if not target:
            continue
        has_room = (target.get("label") or {}).get("room") or target.get("native_room")
        if has_room:
            continue
        plan.append((target["id"], target.get("native_name"), area))
    seen = set()
    for moli_id, name, area in plan:
        if moli_id in seen:
            continue
        seen.add(moli_id)
        print(f"{'SET ' if apply else 'plan'}  {name!s:40.40}  →  {area}")
        if apply:
            req = urllib.request.Request(
                f"{MOLI}/api/labels/{urllib.request.quote(moli_id, safe='')}",
                data=json.dumps({"room": area}).encode(),
                method="PUT",
                headers={"content-type": "application/json", "x-moli-origin": "cli"},
            )
            urllib.request.urlopen(req, timeout=10)
    print(f"{len(seen)} devices {'labelled' if apply else 'to label (dry run)'}")


if __name__ == "__main__":
    main("--apply" in sys.argv)
