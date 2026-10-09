#!/usr/bin/env python3
"""The API encryption key Home Assistant holds for one ESPHome device, for
`moli-os secrets set <instance> api_key`.

Runs inside the Home Assistant container and reads its ESPHome entries
(read-only). The key goes to stdout, nothing else: only ever pipe it.

    docker exec -i homeassistant python3 - --list < esphome-key-export.py       # which devices, no key
    docker exec -i homeassistant python3 - <host> < esphome-key-export.py \\
        | docker exec -i moli-os /moli-os secrets set <instance> api_key        # the key, never shown
"""
import json
import sys

ENTRIES = "/config/.storage/core.config_entries"


def esphome_entries():
    with open(ENTRIES) as f:
        entries = json.load(f)["data"]["entries"]
    return [e for e in entries if e.get("domain") == "esphome"]


def main():
    args = sys.argv[1:]
    if not args:
        sys.exit("give the device's address (or --list)")
    if args[0] == "--list":
        for e in esphome_entries():
            data = e.get("data", {})
            has_key = "yes" if data.get("noise_psk") else "no"
            print(f"{data.get('host', '?'):16} {e.get('title', '?'):40} key: {has_key}")
        return
    host = args[0]
    found = [e for e in esphome_entries() if e.get("data", {}).get("host") == host]
    if len(found) != 1:
        sys.exit(f"{len(found)} ESPHome entries for {host} (see --list)")
    key = found[0].get("data", {}).get("noise_psk")
    if not key:
        sys.exit(f"no encryption key for {host}: the device speaks the API in clear")
    if sys.stdout.isatty():
        sys.exit("pipe the key into `moli-os secrets set`: it is never shown")
    sys.stdout.write(key)


if __name__ == "__main__":
    main()
