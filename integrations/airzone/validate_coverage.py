#!/usr/bin/env python3
"""Check the Airzone coverage inventory offline; never contact a gateway.

This checks inventory integrity, source/test references and honest statuses.
It does not execute Rust tests or certify behavior on physical equipment.
"""

import copy
import hashlib
import json
from pathlib import Path
import sys
import unittest

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
COUNTS = {
    "local": (12, 127),
    "open": (24, 607),
    "web": (13, 607),
    "mqtt": (3, 119),
    "websocket": (4, 128),
}
STATUSES = {"implemented", "partial", "not_implemented"}
FIELD_STATUSES = {"typed_read", "typed_read_write"}


def read(name):
    return json.loads((HERE / name).read_text())


def walk(value):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from walk(child)
    elif isinstance(value, list):
        for child in value:
            yield from walk(child)


def validate(contract, matrix, registry):
    """Raise ValueError on drift, omitted operations or unsupported claims."""

    def require(condition, message):
        if not condition:
            raise ValueError(message)

    require(set(contract["interfaces"]) == set(COUNTS), "interface inventory differs")
    require(set(matrix["interfaces"]) == set(COUNTS), "interface status missing")
    catalog = contract["schema_catalog"]
    for ident, schema in catalog.items():
        digest = hashlib.sha256(
            json.dumps(schema, sort_keys=True).encode()
        ).hexdigest()[:16]
        require(ident == digest, "schema fingerprint mismatch: " + ident)
    expected = set()
    for name, interface in contract["interfaces"].items():
        require(
            (len(interface["operations"]), len(interface["schemas"])) == COUNTS[name],
            "documented operation/schema denominator changed: " + name,
        )
        require(len(interface["source_sha256"]) == 64, "missing source checksum")
        require(
            interface["source"].startswith("https://developers.airzonecloud.com/"),
            "source is not the official developer portal",
        )
        # Only the native Local API facts are vendored. Other interfaces retain
        # their schema-name/fingerprint inventory, regenerated from pinned sources.
        if name == "local":
            require(
                all(key in catalog for key in interface["schemas"].values()),
                "schema facts missing: " + name,
            )
        schemas = [
            catalog[key] for key in interface["schemas"].values() if key in catalog
        ]
        for value in [interface, *schemas]:
            for node in walk(value):
                ref = node.get("$ref", "")
                if ref.startswith("#/components/schemas/"):
                    require(
                        ref.rsplit("/", 1)[1] in interface["schemas"],
                        "unresolved documented schema: " + ref,
                    )
                if ref.startswith("#/components/responses/"):
                    require(
                        ref.rsplit("/", 1)[1]
                        in interface.get("response_components", {}),
                        "unresolved documented response: " + ref,
                    )
        expected.update(name + ":" + op for op in interface["operations"])
    expected.update(
        "mqtt-rpc:" + name
        for name in contract["interfaces"]["mqtt"]["schemas"]
        if name.endswith("RPCRequest") and name != "RPCRequest"
    )
    rows = matrix["operations"]
    require(len(rows) == len({row["id"] for row in rows}), "duplicate operation")
    require(
        {row["id"] for row in rows} == expected,
        "missing or extra operation/RPC variant",
    )
    for row in rows:
        require(row["implementation"] in STATUSES, "invalid operation status")
        require(
            row.get("transport_implementation") in STATUSES, "invalid transport status"
        )
        require(
            bool(row.get("hardware_validation")), "hardware validation must be separate"
        )
        require(
            all(v["implementation"] in STATUSES for v in row["variants"]),
            "invalid variant status",
        )
        if row["implementation"] == "implemented":
            require(
                all(
                    bool(row.get(key))
                    for key in ["rust", "service_model", "ui", "test_evidence"]
                ),
                "implemented operation lacks end-to-end evidence",
            )
            require(
                not row.get("ui_gap"), "implemented operation has an application gap"
            )
            require(
                all(v["implementation"] == "implemented" for v in row["variants"]),
                "implemented operation contains an unimplemented variant",
            )
        if row["implementation"] == "not_implemented":
            require(
                not row["rust"], "unimplemented operation claims Rust implementation"
            )
    actual_fields = {field["key"]: field for field in registry["fields"]}
    fields = []
    for field in matrix["local_fields"]:
        profile = field.get("evidence_profile")
        if profile is not None:
            require(
                profile in matrix.get("field_evidence", {}),
                "unknown field evidence profile",
            )
            field = {**matrix["field_evidence"][profile], **field}
        fields.append(field)
    require(len(fields) == len(actual_fields) == 145, "local field denominator changed")
    require(
        {field["key"] for field in fields} == set(actual_fields), "local field omitted"
    )
    for field in fields:
        require(field["implementation"] in FIELD_STATUSES, "invalid field status")
        require(
            all(
                bool(field.get(key))
                for key in ["rust", "service_model", "ui", "test_evidence"]
            ),
            "modeled field lacks end-to-end evidence",
        )
        source = actual_fields[field["key"]]
        for key in ["schema", "wire", "aliases", "write_scopes"]:
            require(
                field[key] == source.get(key, []),
                "field contract drift: " + field["key"],
            )
        require(field["read_scopes"] == source["scopes"], "field scope drift")
        require(bool(field["hardware_validation"]), "field hardware validation missing")
    parameter_names = {
        field["schema"].split(".")[0]
        for field in fields
        if field["source_kind"] == "named_schema"
    }
    require(len(parameter_names) == 118, "documented parameter schema omitted")
    require(
        registry["source_sha256"] == contract["interfaces"]["local"]["source_sha256"],
        "runtime registry and official inventory use different Local API sources",
    )
    for row in rows + fields:
        for key in ["rust", "service_model", "ui", "test_evidence"]:
            require(
                isinstance(row.get(key, []), list), "evidence must list source paths"
            )
            for reference in row.get(key, []):
                file, _, symbol = reference.partition("#")
                path = ROOT / file
                require(path.is_file(), "evidence file missing: " + file)
                if symbol:
                    require(
                        symbol in path.read_text(),
                        "evidence symbol missing: " + reference,
                    )
    return {
        "operation_rows": len(rows),
        "local_fields": len(fields),
        "transport_complete": sum(
            row["transport_implementation"] == "implemented" for row in rows
        ),
        "application_complete": sum(
            row["implementation"] == "implemented" for row in rows
        ),
        "global_complete": all(row["implementation"] == "implemented" for row in rows),
    }


class InventoryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.contract = read("api-contract.json")
        cls.matrix = read("coverage-matrix.json")
        cls.registry = read("coverage.json")

    def check(self, matrix=None, contract=None):
        return validate(contract or self.contract, matrix or self.matrix, self.registry)

    def test_current_inventory(self):
        result = self.check()
        self.assertEqual(result["operation_rows"], 79)
        self.assertFalse(result["global_complete"])

    def test_omitted_cloud_operation_is_not_hidden(self):
        matrix = copy.deepcopy(self.matrix)
        matrix["operations"] = [
            r for r in matrix["operations"] if r["id"] != "web:GET /user/"
        ]
        with self.assertRaisesRegex(ValueError, "missing or extra"):
            self.check(matrix=matrix)

    def test_missing_field_is_rejected(self):
        matrix = copy.deepcopy(self.matrix)
        matrix["local_fields"].pop()
        with self.assertRaisesRegex(ValueError, "denominator"):
            self.check(matrix=matrix)

    def test_partial_variant_cannot_be_called_implemented(self):
        matrix = copy.deepcopy(self.matrix)
        row = next(r for r in matrix["operations"] if r["id"] == "local:PUT /hvac")
        row["implementation"] = "implemented"
        row["variants"][0]["implementation"] = "not_implemented"
        with self.assertRaisesRegex(ValueError, "unimplemented variant"):
            self.check(matrix=matrix)

    def test_source_facts_cannot_change_without_fingerprint(self):
        contract = copy.deepcopy(self.contract)
        ident = contract["interfaces"]["local"]["schemas"]["battery"]
        contract["schema_catalog"][ident]["type"] = "string"
        with self.assertRaisesRegex(ValueError, "fingerprint"):
            self.check(contract=contract)

    def test_missing_test_evidence_is_rejected(self):
        matrix = copy.deepcopy(self.matrix)
        row = next(
            r for r in matrix["operations"] if r["implementation"] == "implemented"
        )
        row["test_evidence"] = [
            "crates/moli-airzone/src/api_tests.rs#test_that_does_not_exist"
        ]
        with self.assertRaisesRegex(ValueError, "symbol missing"):
            self.check(matrix=matrix)

    def test_missing_application_or_service_evidence_is_rejected(self):
        for key in ["ui", "service_model"]:
            matrix = copy.deepcopy(self.matrix)
            row = next(
                r for r in matrix["operations"] if r["implementation"] == "implemented"
            )
            row[key] = []
            with self.assertRaisesRegex(ValueError, "end-to-end evidence"):
                self.check(matrix=matrix)

    def test_unknown_field_status_is_rejected(self):
        matrix = copy.deepcopy(self.matrix)
        matrix["local_fields"][0]["implementation"] = "unverified_magic"
        with self.assertRaisesRegex(ValueError, "invalid field status"):
            self.check(matrix=matrix)

    def test_fields_without_test_evidence_are_rejected(self):
        matrix = copy.deepcopy(self.matrix)
        for field in matrix["local_fields"]:
            field["test_evidence"] = []
        with self.assertRaisesRegex(ValueError, "end-to-end evidence"):
            self.check(matrix=matrix)

    def test_nonexistent_ui_evidence_is_rejected(self):
        matrix = copy.deepcopy(self.matrix)
        matrix["local_fields"][0]["ui"] = ["web/src/missing.svelte"]
        with self.assertRaisesRegex(ValueError, "evidence file missing"):
            self.check(matrix=matrix)

    def test_unknown_field_evidence_profile_is_rejected(self):
        matrix = copy.deepcopy(self.matrix)
        matrix["local_fields"][0]["evidence_profile"] = "missing-profile"
        with self.assertRaisesRegex(ValueError, "evidence profile"):
            self.check(matrix=matrix)


if __name__ == "__main__":
    if "--test" in sys.argv:
        unittest.main(argv=[sys.argv[0]], verbosity=2)
    else:
        print(
            json.dumps(
                validate(
                    read("api-contract.json"),
                    read("coverage-matrix.json"),
                    read("coverage.json"),
                ),
                indent=2,
            )
        )
