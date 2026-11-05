"""Small synthetic sources exercise regeneration without network or credentials."""

import hashlib
import json
import tempfile
import unittest
from pathlib import Path

from generate_contract import load_source, project


class GenerationTests(unittest.TestCase):
    def test_source_bytes_must_match_reviewed_checksum(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source.json"
            source.write_text('{"info":{"version":"test"}}')
            digest = hashlib.sha256(source.read_bytes()).hexdigest()
            self.assertEqual(load_source(source, digest)["info"]["version"], "test")
            source.write_text('{"info":{"version":"changed"}}')
            with self.assertRaisesRegex(ValueError, "checksum"):
                load_source(source, digest)

    def test_embedded_asyncapi_is_json_not_executable_code(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source.html"
            source.write_text(
                '<script>const schema = {"info":{"version":"test"}}; run();</script>'
            )
            digest = hashlib.sha256(source.read_bytes()).hexdigest()
            self.assertEqual(load_source(source, digest)["info"]["version"], "test")

    def test_projection_keeps_local_contract_and_global_denominators(self):
        interfaces = {
            name: {
                "schemas": {"battery": "local", "DeviceRPCRequest": "remote"},
                "operations": {"GET /": {"parameters": []}},
                "messages": {"unneeded": "copy"},
                "channels": {"unneeded": "copy"},
            }
            for name in ["local", "mqtt"]
        }
        interfaces["local"]["schemas"] = {"battery": "local"}
        result = project(
            {
                "schema_catalog": {
                    "local": {"type": "integer"},
                    "remote": {"type": "object"},
                },
                "interfaces": interfaces,
            }
        )
        self.assertEqual(
            result["interfaces"]["mqtt"]["schemas"], interfaces["mqtt"]["schemas"]
        )
        self.assertEqual(
            result["interfaces"]["local"]["operations"],
            interfaces["local"]["operations"],
        )
        self.assertEqual(result["schema_catalog"]["local"], {"type": "integer"})
        self.assertNotIn("remote", result["schema_catalog"])
        self.assertNotIn("messages", result["interfaces"]["mqtt"])
        self.assertNotIn("channels", result["interfaces"]["mqtt"])


if __name__ == "__main__":
    unittest.main()
