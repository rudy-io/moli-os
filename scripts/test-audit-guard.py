#!/usr/bin/env python3
"""Exercise the audit guard against small Cargo.lock fixtures, without a network."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


GUARD = Path(__file__).with_name("audit-guard.py")
REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"


def package(name, version, dependencies=(), source=REGISTRY):
    return (
        f"\n[[package]]\nname = {json.dumps(name)}\n"
        f"version = {json.dumps(version)}\nsource = {json.dumps(source)}\n"
        f"dependencies = {json.dumps(list(dependencies))}\n"
    )


def fixture(webpki="0.103.16", roots="1.0.10", old=True):
    text = "version = 4\n"
    text += package("rustls", "0.23.46", [f"rustls-webpki {webpki}"])
    text += package("rustls-webpki", webpki)
    text += package("moli-net", "0.1.0", [f"webpki-roots {roots}"], source="path")
    text += package("webpki-roots", roots)
    if old:
        text += package("rumqttc", "0.25.1", ["rustls-webpki 0.102.8"])
        text += package("rustls-webpki", "0.102.8")
    return text


class AuditGuardTests(unittest.TestCase):
    def run_guard(self, text):
        with tempfile.TemporaryDirectory(prefix="moli-audit-guard-") as directory:
            lock = Path(directory) / "Cargo.lock"
            lock.write_text(text, encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(GUARD), str(lock)],
                capture_output=True,
                text=True,
                check=False,
            )

    def assert_allowed(self, text):
        result = self.run_guard(text)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def assert_blocked(self, text, diagnostic):
        result = self.run_guard(text)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn(diagnostic, result.stderr)

    def test_patched_active_versions_with_reviewed_legacy_dependency(self):
        self.assert_allowed(fixture())

    def test_legacy_dependency_can_be_removed(self):
        self.assert_allowed(fixture(old=False))

    def test_active_webpki_regression_is_blocked_despite_legacy_exception(self):
        self.assert_blocked(fixture(webpki="0.103.15"), "0.103.15")

    def test_another_consumer_of_legacy_webpki_is_blocked(self):
        text = fixture() + package("new-client", "1.0.0", ["rustls-webpki 0.102.8"])
        self.assert_blocked(text, "0.102.8")

    def test_changed_rumqttc_requires_a_new_review(self):
        text = fixture().replace('name = "rumqttc"\nversion = "0.25.1"',
                                 'name = "rumqttc"\nversion = "0.25.2"')
        self.assert_blocked(text, "0.102.8")

    def test_unreviewed_source_cannot_reuse_the_legacy_exception(self):
        text = fixture().replace(
            'name = "rumqttc"\nversion = "0.25.1"\nsource = ' + json.dumps(REGISTRY),
            'name = "rumqttc"\nversion = "0.25.1"\nsource = "git+https://example.org/fork"',
        )
        self.assert_blocked(text, "0.102.8")

    def test_unversioned_legacy_dependency_is_resolved_when_unambiguous(self):
        text = "version = 4\n" + package("rustls-webpki", "0.102.8")
        text += package("rumqttc", "0.25.1", ["rustls-webpki"])
        self.assert_allowed(text)

    def test_ambiguous_unversioned_dependency_fails_closed(self):
        text = fixture().replace('"rustls-webpki 0.102.8"', '"rustls-webpki"')
        self.assert_blocked(text, "ambiguous")

    def test_source_qualified_cargo_dependency_is_resolved(self):
        text = fixture().replace('"rustls-webpki 0.102.8"',
                                 json.dumps(f"rustls-webpki 0.102.8 ({REGISTRY})"))
        self.assert_allowed(text)

    def test_future_stable_and_documented_alpha_fix_are_allowed(self):
        for version in ("0.103.17", "0.104.0", "0.104.0-alpha.8", "0.104.0-alpha.10"):
            with self.subTest(version=version):
                self.assert_allowed(fixture(webpki=version))

    def test_vulnerable_or_unreviewed_prerelease_fails_closed(self):
        for version in ("0.104.0-alpha.7", "0.105.0-beta.1"):
            with self.subTest(version=version):
                self.assert_blocked(fixture(webpki=version), version)

    def test_vulnerable_roots_are_blocked(self):
        self.assert_blocked(fixture(roots="1.0.9"), "1.0.9")

    def test_compatibility_roots_require_a_patched_parent(self):
        for version, allowed in (("1.0.10", True), ("1.0.9", False)):
            with self.subTest(parent=version):
                text = fixture(roots=version)
                text += package("webpki-roots", "0.26.11", [f"webpki-roots {version}"])
                if allowed:
                    self.assert_allowed(text)
                else:
                    self.assert_blocked(text, "1.0.9")

    def test_compatibility_roots_cannot_omit_the_parent(self):
        self.assert_blocked(fixture(roots="0.26.11"), "0.26.11")

    def test_compatibility_roots_cannot_point_to_an_unresolved_parent(self):
        text = fixture() + package("webpki-roots", "0.26.11", ["webpki-roots 1.0.11"])
        self.assert_blocked(text, "unresolved")


if __name__ == "__main__":
    unittest.main()
