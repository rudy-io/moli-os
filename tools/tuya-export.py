#!/usr/bin/env python3
"""What the Tuya cloud knows about the home's devices, for `moli-os tuya import`.

Standalone twin of tools/home-assistant/tuya-export.py: instead of reusing
Home Assistant's Tuya login, it logs into the home's Tuya account itself,
with the same QR-code flow as Home Assistant's Tuya integration (scan with
the Tuya Smart Life app). Read-only on Tuya's side. No Home Assistant, no
other hub: the plugs only need to be paired in the Tuya app.

    pip install tuya-device-sharing-sdk pyqrcode cryptography
    python3 tools/tuya-export.py --summary    # login, then a listing, no keys
    python3 tools/tuya-export.py \\
        | docker exec -i moli-os /moli-os tuya import                      # keys never shown

The JSON on stdout holds the devices' local keys: only ever pipe it.
"""
import argparse
import json
import sys
import time

from tuya_sharing import LoginControl, Manager, SharingTokenListener, logger

# Home Assistant's public Tuya client (homeassistant/components/tuya/const.py):
# the identifier Tuya registered for that QR flow, not a secret.
CLIENT_ID = "HA_3y9q4ak7g4ephrvke"
SCHEMA = "haauthorize"
APP_QR_CODE_HEADER = "tuyaSmart--qrLogin/?token="
WAIT_S = 300
POLL_S = 2

TYPES = {"Boolean": "Boolean", "Integer": "Integer", "Enum": "Enum", "String": "String",
         "Json": "Json", "Raw": "Raw", "Bitmap": "Bitmap"}


class TokenListener(SharingTokenListener):
    # The export is short-lived; if the token is renewed mid-run, note it.
    def update_token(self, token_info):
        print("tuya token renewed", file=sys.stderr)


def say(message):
    print(message, file=sys.stderr)


def user_code(args):
    code = args.user_code
    if not code:
        # Prompt on stderr: stdout is the JSON pipe to `moli-os tuya import`.
        print("Smart Life / Tuya Smart user code "
              "(app: Settings > Account and Security > User Code): ",
              end="", file=sys.stderr, flush=True)
        code = input().strip()
    if not code:
        sys.exit("a user code is needed (from the Tuya app, or --user-code)")
    return code


def login(code):
    control = LoginControl()
    response = control.qr_code(CLIENT_ID, SCHEMA, code)
    if not response.get("success"):
        sys.exit(f"could not open the Tuya login ({response.get('code')}: {response.get('msg')})")
    token = response["result"]["qrcode"]
    url = APP_QR_CODE_HEADER + token
    say("Scan this QR code with the Tuya Smart Life app (or open the URL):")
    try:
        import pyqrcode
        say(pyqrcode.create(url, mode="binary").terminal())
    except ImportError:
        say("pyqrcode is not installed; open this URL in the app's scanner instead:")
        say(url)
    deadline = time.monotonic() + WAIT_S
    waited_s = 0
    while True:
        ok, info = control.login_result(token, CLIENT_ID, code)
        if ok:
            return info
        if time.monotonic() >= deadline:
            sys.exit("not authorized in time: scan the QR code again (rerun the script)")
        waited_s += POLL_S
        if waited_s % 30 == 0:
            say(f"waiting for the Tuya app to authorize… ({waited_s}/{WAIT_S}s)")
        time.sleep(POLL_S)


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


def device_of(device):
    out = {"id": device.id, "name": (device.name or device.id).strip(),
           "category": device.category or "", "product": (device.product_name or "").strip() or None,
           "dps": dps_of(device), "local_key": getattr(device, "local_key", None)}
    if getattr(device, "sub", False) or getattr(device, "node_id", None):
        out["gateway"] = "tuya"
    return out


def main():
    logger.setLevel("WARNING")
    parser = argparse.ArgumentParser(description="export the home's Tuya devices for moli-os")
    parser.add_argument("--summary", action="store_true",
                        help="list the devices instead of piping the import JSON")
    parser.add_argument("--user-code",
                        help="the account's user code (default: asked once)")
    args = parser.parse_args()

    code = user_code(args)
    info = login(code)
    endpoint = info.get("endpoint")
    terminal_id = info.get("terminal_id")
    if not endpoint or not terminal_id:
        sys.exit("login succeeded but no endpoint/terminal_id returned: " + json.dumps(sorted(info.keys())))
    manager = Manager(CLIENT_ID, code, terminal_id, endpoint,
                      {"t": info.get("t"), "uid": info.get("uid"),
                       "expire_time": info.get("expire_time"),
                       "access_token": info.get("access_token"),
                       "refresh_token": info.get("refresh_token")},
                      TokenListener())
    manager.update_device_cache()

    devices = [device_of(device) for device in manager.device_map.values()]
    if args.summary:
        for device in devices:
            say(f'{device["id"][-6:]:8} {device["category"]:6} {len(device["dps"]):2} DPs '
                f'key:{"yes" if device["local_key"] else "no "} {"sub " if "gateway" in device else "    "}{device["name"]}')
        say(f"{len(devices)} devices")
    else:
        json.dump({"devices": devices}, sys.stdout, ensure_ascii=False)
        sys.stdout.write("\n")


if __name__ == "__main__":
    main()
