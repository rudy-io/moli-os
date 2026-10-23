#!/usr/bin/env python3
"""What the Tuya cloud knows about the home's devices, for `moli-os tuya import`.

Without Home Assistant: through a Tuya IoT cloud project (platform.tuya.com)
linked to the household's Smart Life account. Its Access ID and Access Secret
are typed at the prompt, hidden; never stored, never shown.

Same output as tools/home-assistant/tuya-export.py: each device's data points
under the codes the cloud reports states with, its local key, and for a
sub-device the gateway it is behind and its address there (`cid`), so that
the gateway's local link reads it.

    python3 -I tuya-iot-export.py --summary            # check, no secret
    python3 -I tuya-iot-export.py \\
        | ssh <host> docker exec -i moli-os /moli-os tuya import   # keys never shown
    python3 -I tuya-iot-export.py --key=<id end> | <program>      # one key, into a pipe

`--region=eu` (Central Europe, default), `eu-west`, `us`, `us-east`, `in`, `cn`:
the data center of the cloud project. Run the import with the Moli version that
will read the file: an older one drops the fields it does not know.

The JSON holds the devices' local keys: only ever pipe it.
"""
import getpass
import hashlib
import hmac
import json
import os
import ssl
import sys
import time
import urllib.parse
import urllib.request
import uuid

ENDPOINTS = {
    "eu": "https://openapi.tuyaeu.com",
    "eu-west": "https://openapi-weaz.tuyaeu.com",
    "us": "https://openapi.tuyaus.com",
    "us-east": "https://openapi-ueaz.tuyaus.com",
    "in": "https://openapi.tuyain.com",
    "cn": "https://openapi.tuyacn.com",
}
# Thing model type -> Moli DP type.
MODEL_TYPES = {"bool": "Boolean", "value": "Integer", "enum": "Enum", "string": "String",
               "bitmap": "Bitmap", "raw": "Raw", "struct": "Json", "array": "Json"}
TYPES = {"Boolean", "Integer", "Enum", "String", "Json", "Raw", "Bitmap"}
# python.org's Python on macOS ships without trusted roots: the system's then;
# verification stays on.
TLS = ssl.create_default_context(
    cafile=None if os.path.exists(ssl.get_default_verify_paths().openssl_cafile)
    else "/etc/ssl/cert.pem")


class Cloud:
    """Tuya's OpenAPI, signed (HMAC-SHA256), read only."""

    def __init__(self, base, client_id, secret):
        self.base, self.client_id, self.secret = base, client_id, secret
        self.token = ""

    def _sign(self, method, path, token):
        t, nonce = str(int(time.time() * 1000)), uuid.uuid4().hex
        to_sign = "\n".join([method, hashlib.sha256(b"").hexdigest(), "", path])
        message = self.client_id + token + t + nonce + to_sign
        sign = hmac.new(self.secret.encode(), message.encode(), hashlib.sha256).hexdigest().upper()
        headers = {"client_id": self.client_id, "sign": sign, "t": t, "nonce": nonce,
                   "sign_method": "HMAC-SHA256"}
        if token:
            headers["access_token"] = token
        return headers

    def get(self, path, query=None, token=None):
        if query:
            path += "?" + urllib.parse.urlencode(sorted(query.items()))
        token = self.token if token is None else token
        request = urllib.request.Request(self.base + path, headers=self._sign("GET", path, token))
        with urllib.request.urlopen(request, timeout=20, context=TLS) as response:
            body = json.load(response)
        if not body.get("success"):
            sys.exit(f"Tuya refused {path.split('?')[0]}: {body.get('code')} {body.get('msg')}")
        return body["result"]

    def login(self):
        self.token = self.get("/v1.0/token", {"grant_type": "1"}, token="")["access_token"]


def devices(cloud):
    found, last = [], ""
    while True:
        query = {"size": "100"}
        if last:
            query["last_row_key"] = last
        page = cloud.get("/v1.0/iot-01/associated-users/devices", query)
        found += page.get("devices") or []
        last = page.get("last_row_key") or ""
        if not page.get("has_more") or not last:
            return found


def described(dp, spec):
    """Unit, scale, bounds and choices, from a thing model or a standard set."""
    if spec.get("unit"):
        dp["unit"] = str(spec["unit"])
    for key in ("scale", "min", "max", "step"):
        if isinstance(spec.get(key), (int, float)):
            dp[key] = int(spec[key]) if key == "scale" else float(spec[key])
    if spec.get("range"):
        dp["range"] = [str(v) for v in spec["range"]]
    return dp


def model_dps(cloud, device_id):
    """The thing model: the DPs with their local ids."""
    model = json.loads(cloud.get(f"/v2.0/cloud/thing/{device_id}/model").get("model") or "{}")
    dps = []
    for service in model.get("services") or []:
        for prop in service.get("properties") or []:
            spec = prop.get("typeSpec") or {}
            dps.append(described({"id": int(prop["abilityId"]), "code": prop["code"],
                                  "type": MODEL_TYPES.get(spec.get("type"), "Raw"),
                                  "writable": "w" in (prop.get("accessMode") or "")}, spec))
    return sorted(dps, key=lambda d: d["id"])


def same_shape(dp, standard):
    unit = lambda d: {"°C": "℃", "C": "℃"}.get(d.get("unit") or "", d.get("unit") or "")
    return (dp["type"] == standard["type"] and unit(dp) == unit(standard)
            and all(dp.get(k) == standard.get(k) for k in ("min", "max", "scale")))


def dps_of(cloud, device_id):
    """The thing model's DPs, named with the standard codes the cloud reports
    states with. The standard set gives no DP id: a standard code missing from
    the model takes the one model DP of the same type, unit and range, or none
    (then the model's code stays)."""
    dps = model_dps(cloud, device_id)
    try:
        spec = cloud.get(f"/v1.0/iot-03/devices/{device_id}/specification")
    except SystemExit:
        return dps
    standard = []
    for item in spec.get("status") or []:
        try:
            values = json.loads(item.get("values") or "{}")
        except ValueError:
            values = {}
        kind = item.get("type") if item.get("type") in TYPES else "Raw"
        standard.append(described({"code": item["code"], "type": kind}, values))
    codes = {s["code"] for s in standard}
    for s in standard:
        if any(d["code"] == s["code"] for d in dps):
            continue
        twins = [d for d in dps if d["code"] not in codes and same_shape(d, s)]
        if len(twins) == 1:
            twins[0]["code"] = s["code"]
    return dps


def gateways_of(cloud, found):
    """Each sub-device's gateway, asked of every device that may be one."""
    parent = {}
    if any(d.get("sub") for d in found):
        for gateway in (d for d in found if not d.get("sub")):
            try:
                subs = cloud.get(f"/v1.0/devices/{gateway['id']}/sub-devices")
            except SystemExit:
                continue
            for s in subs if isinstance(subs, list) else subs.get("list", []):
                parent[s.get("id")] = gateway["id"]
    return parent


def main(args):
    region = next((a.split("=", 1)[1] for a in args if a.startswith("--region=")), "eu")
    wanted = next((a.split("=", 1)[1] for a in args if a.startswith("--key=")), None)
    if wanted and sys.stdout.isatty():
        sys.exit("--key never prints: pipe it into the program that needs it")
    cloud = Cloud(ENDPOINTS[region], getpass.getpass("Access ID (hidden): ").strip(),
                  getpass.getpass("Access Secret (hidden): ").strip())
    cloud.login()
    found = devices(cloud)
    if wanted:
        match = [d for d in found if d["id"].endswith(wanted)]
        if len(match) != 1 or not match[0].get("local_key"):
            sys.exit(f"no single device with a key ends with {wanted}")
        sys.stdout.write(match[0]["local_key"])
        return
    parent = gateways_of(cloud, found)
    out = []
    for d in found:
        entry = {"id": d["id"], "name": (d.get("name") or d["id"]).strip(),
                 "category": d.get("category") or "",
                 "product": (d.get("product_name") or "").strip() or None,
                 "dps": dps_of(cloud, d["id"]), "local_key": d.get("local_key") or None}
        if d.get("sub"):
            entry["gateway"] = parent.get(d["id"], "tuya")
            if d["id"] in parent and d.get("node_id"):
                entry["cid"] = d["node_id"]
        out.append(entry)
    if "--summary" in args:
        for d in out:
            local = f'via {d["gateway"][-6:]} cid …{d["cid"][-4:]} ' if d.get("cid") else ""
            codes = f'  [{", ".join(p["code"] for p in d["dps"])}]' if len(d["dps"]) <= 5 else ""
            print(f'{d["id"][-6:]:8} {d["category"]:6} {len(d["dps"]):2} DPs '
                  f'key:{"yes" if d["local_key"] else "no "} {"sub " if "gateway" in d else "    "}'
                  f'{local}{d["name"]}{codes}')
        print(f"{len(out)} devices", file=sys.stderr)
    else:
        json.dump({"devices": out}, sys.stdout, ensure_ascii=False)


main(sys.argv[1:])
