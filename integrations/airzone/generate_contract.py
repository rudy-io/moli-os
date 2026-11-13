#!/usr/bin/env python3
"""Regenerate the compact inventory from checksum-pinned official source files.

Offline; downloads are explicit, and no gateway or account is contacted.
PyYAML is needed only to read the three official YAML documents, not validation.
"""

import argparse
import copy
import hashlib
import json
from pathlib import Path
import re

FILES = {
    "local": "local-openapi.txt",
    "open": "open-api.yaml",
    "web": "web-api.yaml",
    "mqtt": "mqtt-page.txt",
    "websocket": "websocket-page.txt",
}
METHODS = {"get", "post", "put", "patch", "delete", "head", "options"}


def load_source(path, checksum):
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != checksum:
        raise ValueError("source checksum changed: " + path.name)
    text = data.decode("utf-8")
    if "const schema = " in text:
        return json.JSONDecoder().raw_decode(
            text.rsplit("const schema = ", 1)[1].lstrip()
        )[0]
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        import yaml

        return yaml.safe_load(text)


def project(contract):
    result = copy.deepcopy(contract)
    result["format_version"] = 2
    local_ids = set(result["interfaces"]["local"]["schemas"].values())
    result["schema_catalog"] = {
        key: value
        for key, value in result["schema_catalog"].items()
        if key in local_ids
    }
    for interface in result["interfaces"].values():
        interface.pop("messages", None)
        interface.pop("channels", None)
    return {
        "format_version": 2,
        "schema_catalog": result["schema_catalog"],
        "interfaces": result["interfaces"],
    }


def render(value, path=(), depth=0):
    """One reviewable schema/operation/field per line; keep surrounding structure."""
    if (
        len(path) == 2
        and path[0] in {"schema_catalog", "local_fields", "fields", "operations"}
    ) or (len(path) == 4 and path[0] == "interfaces" and path[2] == "operations"):
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    if isinstance(value, dict) and value:
        children = [
            " " * (depth + 2)
            + json.dumps(key)
            + ": "
            + render(item, path + (key,), depth + 2)
            for key, item in value.items()
        ]
        return "{\n" + ",\n".join(children) + "\n" + " " * depth + "}"
    if isinstance(value, list) and value:
        children = [
            " " * (depth + 2) + render(item, path + (index,), depth + 2)
            for index, item in enumerate(value)
        ]
        return "[\n" + ",\n".join(children) + "\n" + " " * depth + "]"
    return json.dumps(value, ensure_ascii=False)


DROP = {
    "description",
    "summary",
    "title",
    "example",
    "examples",
    "externalDocs",
    "contact",
    "license",
    "tags",
    "xml",
}


def resolve(s, ref):
    if ref.startswith("#/"):
        out = s
        for k in ref[2:].split("/"):
            out = out[k.replace("~1", "/").replace("~0", "~")]
        return out
    if ref.startswith("$ref:$"):
        out = s
        for prop, idx in re.findall(r"\.([^\.\[\]]+)|\[(\d+)\]", ref[6:]):
            out = out[int(idx)] if idx else out[prop]
        return out
    return ref


def compact(s, node, seen=()):
    if isinstance(node, str) and node.startswith("$ref:$"):
        if node in seen:
            return {"documentation_reference": node[5:]}
        return compact(s, resolve(s, node), seen + (node,))
    if isinstance(node, list):
        return [compact(s, v, seen) for v in node]
    if not isinstance(node, dict):
        return node
    return {
        k: compact(s, v, seen)
        for k, v in node.items()
        if k not in DROP and not k.startswith("x-parser-")
    }


def endpoint(s, p, m):
    op = s["paths"][p][m]
    body = op.get("requestBody", {})
    if "$ref" in body:
        body = resolve(s, body["$ref"])
    params = s["paths"][p].get("parameters", []) + op.get("parameters", [])
    return {
        "parameters": compact(s, params),
        "request": compact(s, body),
        "responses": compact(s, op.get("responses", {})),
        "security": compact(s, op.get("security", s.get("security", []))),
    }


def field_facts(node, path="", out=None):
    out = [] if out is None else out
    if not isinstance(node, dict):
        return out
    facts = {
        k: node[k]
        for k in [
            "type",
            "format",
            "enum",
            "const",
            "minimum",
            "maximum",
            "minLength",
            "maxLength",
            "multipleOf",
            "nullable",
            "readOnly",
            "writeOnly",
            "required",
            "$ref",
        ]
        if k in node
    }
    if facts:
        out.append({"path": path or "$", **facts})
    for k, v in node.get("properties", {}).items():
        field_facts(v, path + "." + str(k) if path else str(k), out)
    if isinstance(node.get("items"), dict):
        field_facts(node["items"], path + "[]", out)
    for kind in ["allOf", "oneOf", "anyOf"]:
        for i, v in enumerate(node.get(kind, [])):
            field_facts(v, f"{path}<{kind}:{i}>", out)
    if isinstance(node.get("schema"), dict):
        field_facts(node["schema"], path, out)
    return out


def generate(specs, manifest):
    contracts = {}
    for k, s in specs.items():
        contract = {
            "version": s["info"]["version"],
            "source": manifest["interfaces"][k]["source"],
            "source_sha256": manifest["interfaces"][k]["source_sha256"],
            "checked_at": manifest["interfaces"][k]["checked_at"],
            "schemas": compact(s, s["components"]["schemas"]),
        }
        if k in ["local", "open", "web"]:
            contract["operations"] = {
                f"{m.upper()} {p}": endpoint(s, p, m)
                for p, item in s["paths"].items()
                for m in item
                if m in METHODS
            }
            contract["security_schemes"] = compact(
                s, s.get("components", {}).get("securitySchemes", {})
            )
            contract["response_components"] = compact(
                s, s.get("components", {}).get("responses", {})
            )
        else:
            # AsyncAPI's HTML embeds parsed JSON. Keep the operation denominator
            # and fingerprint its schemas, without retaining repeated messages.
            if k == "mqtt":
                contract["operations"] = {
                    name: {
                        "action": v["action"],
                        "channel": v["channel"]["address"],
                        "bindings": compact(s, v.get("bindings", {})),
                    }
                    for name, v in s["operations"].items()
                }
            else:
                contract["operations"] = {
                    f"{m.upper()} {ch}": compact(s, v[m])
                    for ch, v in s["channels"].items()
                    for m in ["subscribe", "publish"]
                    if m in v
                }
        contracts[k] = contract
    for k in ["mqtt", "websocket"]:
        contracts[k]["schemas"] = {
            name: field_facts(node) for name, node in contracts[k]["schemas"].items()
        }
        for name, node in contracts[k]["operations"].items():
            contracts[k]["operations"][name] = (
                {x: node[x] for x in ["action", "channel", "bindings"] if x in node}
                if k == "mqtt"
                else {"bindings": node.get("bindings", {})}
            )
    # Add factual Local API hardware prerequisites and value sets defined in prose.
    for name, node in specs["local"]["components"]["schemas"].items():
        d = node.get("description", "")
        for label, key in [
            ("Available devices", "device_prerequisites"),
            ("Available values", "documented_values"),
        ]:
            m = re.search(label + r"[^`]*```\s*(.*?)```", d, re.S | re.I)
            if m:
                contracts["local"]["schemas"][name][key] = (
                    m.group(1).strip().splitlines()
                )
    contracts["local"]["schemas"]["battery"]["availability_note"] = (
        "Only reported for a radio thermostat; absence is not a zero battery reading."
    )
    contracts["local"]["schemas"]["coverage"]["availability_note"] = (
        "Only reported for a radio thermostat."
    )
    contracts["local"]["schemas"]["name"]["availability_note"] = (
        "IAQ name is not available on Aidoo Pro; at most 11 permitted characters."
    )
    contracts["local"]["schemas"]["iaq_mode_vent"]["availability_note"] = (
        "Only writable with iaq_vmc active; otherwise null."
    )
    catalog = {}
    for contract in contracts.values():
        for name, schema in list(contract["schemas"].items()):
            ident = hashlib.sha256(
                json.dumps(schema, sort_keys=True).encode()
            ).hexdigest()[:16]
            catalog[ident] = schema
            contract["schemas"][name] = ident
    return project({"schema_catalog": catalog, "interfaces": contracts})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, required=True)
    parser.add_argument(
        "--manifest", type=Path, default=Path(__file__).with_name("api-contract.json")
    )
    parser.add_argument("--output", type=Path)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    manifest = json.loads(args.manifest.read_text())
    specs = {
        name: load_source(
            args.source_dir / filename, manifest["interfaces"][name]["source_sha256"]
        )
        for name, filename in FILES.items()
    }
    generated = generate(specs, manifest)
    if args.check:
        if generated != manifest:
            raise SystemExit("generated contract differs from reviewed inventory")
        print("official source checksums and compact contract match")
    elif args.output:
        args.output.write_text(render(generated) + "\n")
    else:
        print(render(generated))


if __name__ == "__main__":
    main()
