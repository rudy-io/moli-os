#!/usr/bin/env python3
"""Home Assistant's automations, for Moli to translate into drafts.

Runs inside the Home Assistant container (read-only: it only reads
automations.yaml and the registries), prints JSON for
`POST /api/automations/import`. Starting an import takes a person's
session (the dashboard code, 403 otherwise): post it from the computer
where the dashboard is open, with its `moli_session` cookie (browser
developer tools, Application, Cookies). A session is bound to the device
that opened it: the same cookie from the Moli host itself is refused.

    ssh <ha-host> 'docker exec -i homeassistant python3 -' \\
        < ha-automations-export.py > ha.json
    curl -s -X POST -H 'content-type: application/json' \\
         -H 'cookie: moli_session=<cookie>' --data-binary @ha.json \\
         http://<moli-host>:8790/api/automations/import

For each automation: its alias, description, mode, its YAML (webhook ids,
URLs and tokens masked), Home Assistant's id for it (`ha_id`, kept out of
the YAML) and the entities it mentions with their names, devices and
areas, so Moli can find the same devices by name.
Nothing is sent anywhere by this script.
"""

import json
import re
import sys

import yaml

SECRET_KEYS = re.compile(r"(webhook_id|url|token|password|api_key|secret|resource|authorization)", re.I)
ENTITY = re.compile(r"\b(?:light|switch|binary_sensor|sensor|climate|media_player|cover|camera|fan|lock|"
                    r"input_boolean|input_select|input_number|person|device_tracker|sun|weather|button|"
                    r"number|select|remote|alarm_control_panel|vacuum|counter|todo|script)\.[a-z0-9_]+")


class Loader(yaml.SafeLoader):
    pass


Loader.add_multi_constructor("!", lambda loader, suffix, node: f"<{suffix}>")


def mask(value):
    if isinstance(value, dict):
        return {k: ("<masqué>" if SECRET_KEYS.search(str(k)) else mask(v)) for k, v in value.items()}
    if isinstance(value, list):
        return [mask(v) for v in value]
    if isinstance(value, str):
        return re.sub(r"https?://\S+", "<url>", value)
    return value


def device_ids(value):
    if isinstance(value, dict):
        for k, v in value.items():
            if k == "device_id" and isinstance(v, str):
                yield v
            else:
                yield from device_ids(v)
    elif isinstance(value, list):
        for v in value:
            yield from device_ids(v)


def registry(name):
    try:
        return json.load(open(f"/config/.storage/{name}"))["data"]
    except (OSError, KeyError, ValueError):
        return {}


def enabled_only(value):
    """Drops what is switched off in HA (`enabled: false` on an action, a
    trigger or a condition): a voice muted in HA must stay muted in Moli."""
    if isinstance(value, list):
        return [enabled_only(v) for v in value if not (isinstance(v, dict) and v.get("enabled") is False)]
    if isinstance(value, dict):
        return {k: enabled_only(v) for k, v in value.items()}
    return value


def switched_off():
    """HA ids (the YAML `id`) of the automations switched off in HA: their
    toggle lives in the restored state, not in automations.yaml."""
    by_entity = {e["entity_id"]: e.get("unique_id") for e in registry("core.entity_registry").get("entities", [])
                 if e["entity_id"].startswith("automation.")}
    off = set()
    for item in registry("core.restore_state") or []:
        state = item.get("state", {})
        if state.get("entity_id", "").startswith("automation.") and state.get("state") == "off":
            uid = by_entity.get(state["entity_id"]) or state.get("attributes", {}).get("id")
            if uid:
                off.add(str(uid))
    return off


def main():
    automations = yaml.load(open("/config/automations.yaml"), Loader=Loader) or []
    off = switched_off()
    entities = {e["entity_id"]: e for e in registry("core.entity_registry").get("entities", [])}
    devices = {d["id"]: d for d in registry("core.device_registry").get("devices", [])}
    areas = {a.get("area_id") or a.get("id"): a["name"] for a in registry("core.area_registry").get("areas", [])}

    out = []
    for a in automations:
        if not isinstance(a, dict):
            continue
        # HA's id stays out of the YAML Moli reads, but goes with it: the
        # draft says which original to switch off once it is approved.
        a = enabled_only(a)
        masked = mask({k: v for k, v in a.items() if k != "id"})
        ha_id = str(a["id"]) if a.get("id") is not None else None
        text = yaml.safe_dump(masked, allow_unicode=True, sort_keys=False)
        mentioned = []
        for entity_id in sorted(set(ENTITY.findall(yaml.safe_dump(a, allow_unicode=True)))):
            e = entities.get(entity_id, {})
            d = devices.get(e.get("device_id") or "", {})
            area = areas.get(e.get("area_id") or "") or areas.get(d.get("area_id") or "")
            mentioned.append({
                "entity_id": entity_id,
                "name": e.get("name") or e.get("original_name"),
                "device": d.get("name_by_user") or d.get("name"),
                "area": area,
                "platform": e.get("platform"),
                "device_class": e.get("device_class") or e.get("original_device_class"),
            })
        # « device » triggers and actions name a device, not an entity.
        for device_id in sorted(set(device_ids(a))):
            d = devices.get(device_id, {})
            if d:
                mentioned.append({
                    "device_id": device_id,
                    "device": d.get("name_by_user") or d.get("name"),
                    "area": areas.get(d.get("area_id") or ""),
                    "model": d.get("model"),
                })
        out.append({
            "alias": str(a.get("alias") or a.get("id") or "Sans nom"),
            "description": str(a.get("description") or ""),
            "mode": a.get("mode", "single"),
            "yaml": text,
            "entities": mentioned,
            "ha_id": ha_id,
            "off_in_ha": ha_id in off,
        })
    json.dump({"automations": out}, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()
