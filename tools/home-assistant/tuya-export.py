#!/usr/bin/env python3
"""What the Tuya cloud knows about the home's devices, for `moli-os tuya import`.

Runs inside the Home Assistant container, through the Tuya account Home
Assistant's Tuya integration is linked to (read-only on Tuya's side).

Never refreshes HA's token: a refresh here would leave Home Assistant with
a revoked one. It refuses to run when the token is about to expire; retry
once Home Assistant has renewed it.

    docker exec -i homeassistant python3 - --summary < tuya-export.py     # check, no secret
    docker exec -i homeassistant python3 - < tuya-export.py \\
        | docker exec -i moli-os /moli-os tuya import                    # keys never shown

The JSON holds the devices' local keys: only ever pipe it.
"""
import json
import sys
import time

from tuya_sharing import Manager, SharingTokenListener

MARGIN_S = 900
TYPES = {"Boolean": "Boolean", "Integer": "Integer", "Enum": "Enum", "String": "String",
         "Json": "Json", "Raw": "Raw", "Bitmap": "Bitmap"}


def client_id():
    with open("/usr/src/homeassistant/homeassistant/components/tuya/const.py") as f:
        for line in f:
            if line.startswith("TUYA_CLIENT_ID"):
                return line.split("=", 1)[1].strip().strip('"')
    sys.exit("TUYA_CLIENT_ID not found in Home Assistant's Tuya integration")


class KeepHasToken(SharingTokenListener):
    # Called only after a refresh has happened: too late to protect HA's
    # token. What protects it is the expiry check in main().
    def update_token(self, token_info):
        sys.exit("the SDK refreshed Home Assistant's token: HA may need to re-link Tuya")


def numeric(value):
    """Tuya sends numbers, or numbers as text."""
    try:
        return None if value in (None, "") else float(value)
    except (TypeError, ValueError):
        return None


def dps_of(device):
    writable = set((getattr(device, "function", None) or {}).keys())
    dps = []
    for dp_id, item in (getattr(device, "local_strategy", None) or {}).items():
        code = item.get("status_code")
        conf = item.get("config_item") or {}
        if not code:
            continue
        try:
            desc = json.loads(conf.get("valueDesc") or "{}")
        except ValueError:
            desc = {}
        dp = {"id": int(dp_id), "code": code, "type": TYPES.get(conf.get("valueType"), "Raw"),
              "writable": code in writable}
        if desc.get("unit") not in (None, ""):
            dp["unit"] = str(desc["unit"])
        for key in ("scale", "min", "max", "step"):
            number = numeric(desc.get(key))
            if number is not None:
                dp[key] = int(number) if key == "scale" else number
        if desc.get("range"):
            dp["range"] = [str(v) for v in desc["range"]]
        dps.append(dp)
    return sorted(dps, key=lambda d: d["id"])


def main(summary):
    entries = json.load(open("/config/.storage/core.config_entries"))["data"]["entries"]
    entry = next(e for e in entries if e["domain"] == "tuya")["data"]
    token = entry["token_info"]
    left = token.get("t", 0) / 1000 + token.get("expire_time", 0) - time.time()
    if left < MARGIN_S:
        sys.exit(f"Home Assistant's Tuya token expires in {int(left)} s: retry once HA has renewed it")
    manager = Manager(client_id(), entry["user_code"], entry["terminal_id"], entry["endpoint"],
                      token, KeepHasToken())
    manager.update_device_cache()
    devices = []
    for d in manager.device_map.values():
        out = {"id": d.id, "name": (d.name or d.id).strip(), "category": d.category or "",
               "product": (d.product_name or "").strip() or None, "dps": dps_of(d),
               "local_key": getattr(d, "local_key", None)}
        if getattr(d, "sub", False) or getattr(d, "node_id", None):
            out["gateway"] = "tuya"
        devices.append(out)
    if summary:
        for d in devices:
            print(f'{d["id"][-6:]:8} {d["category"]:6} {len(d["dps"]):2} DPs '
                  f'key:{"yes" if d["local_key"] else "no "} {"sub " if "gateway" in d else "    "}{d["name"]}')
        print(f"{len(devices)} devices", file=sys.stderr)
    else:
        json.dump({"devices": devices}, sys.stdout, ensure_ascii=False)


main("--summary" in sys.argv)
