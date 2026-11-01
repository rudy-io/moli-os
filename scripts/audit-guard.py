#!/usr/bin/env python3
"""Bound the RUSTSEC-2026-0382 exception, which cargo-audit ignores by ID.

Run with Python 3.11+: python3 scripts/audit-guard.py [Cargo.lock]
Only rumqttc 0.25.1's unused WebPKI error type may retain 0.102.8.
Other copies must be patched, as must the roots covered by RUSTSEC-2026-0381.
Stable releases and the documented 0.104.0-alpha.8+ line are supported;
other prereleases fail closed and require review. No network access is used.
"""

import argparse
from pathlib import Path
import re
import sys
import tomllib


REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
NUMBER = r"(0|[1-9][0-9]*)"
BUILD = r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"


def stable_version(version):
    match = re.fullmatch(rf"{NUMBER}\.{NUMBER}\.{NUMBER}{BUILD}", version)
    return tuple(map(int, match.groups())) if match else None


def patched_webpki(version):
    stable = stable_version(version)
    if stable is not None:
        return stable >= (0, 103, 16)
    alpha = re.fullmatch(rf"0\.104\.0-alpha\.{NUMBER}{BUILD}", version)
    return alpha is not None and int(alpha[1]) >= 8


def patched_roots(version):
    stable = stable_version(version)
    return stable is not None and stable >= (1, 0, 10)


def resolve(dependency, packages):
    """Cargo omits versions/sources when the dependency name is unambiguous."""
    match = re.fullmatch(r"([^ ]+)(?: ([^ ()]+))?(?: \(([^)]+)\))?", dependency)
    if match is None:
        raise ValueError(f"malformed dependency: {dependency}")
    name, version, source = match.groups()
    matches = [
        index for index, package in enumerate(packages)
        if package["name"] == name
        and (version is None or package["version"] == version)
        and (source is None or package.get("source") == source)
    ]
    if len(matches) != 1:
        reason = "unresolved" if not matches else "ambiguous"
        raise ValueError(f"{reason} dependency: {dependency}; review the lockfile")
    return matches[0]


def check(lock):
    packages = lock.get("package", [])
    incoming = [set() for _ in packages]
    outgoing = [set() for _ in packages]
    for index, package in enumerate(packages):
        for dependency in package.get("dependencies", []):
            if dependency.split(" ", 1)[0] in {"rustls-webpki", "webpki-roots"}:
                target = resolve(dependency, packages)
                incoming[target].add(index)
                outgoing[index].add(target)

    for index, package in enumerate(packages):
        name, version = package["name"], package["version"]
        if name == "rustls-webpki":
            if version == "0.102.8":
                parents = [packages[parent] for parent in incoming[index]]
                # The review covers the published crates.io sources only.
                if not (
                    package.get("source") == REGISTRY
                    and len(parents) == 1
                    and parents[0]["name"] == "rumqttc"
                    and parents[0]["version"] == "0.25.1"
                    and parents[0].get("source") == REGISTRY
                ):
                    raise ValueError(
                        "rustls-webpki 0.102.8 must have only the reviewed "
                        "crates.io rumqttc 0.25.1 as its direct consumer"
                    )
            elif not patched_webpki(version):
                raise ValueError(
                    f"rustls-webpki {version} is vulnerable or unreviewed; "
                    "review RUSTSEC-2026-0382 before changing this guard"
                )
        elif name == "webpki-roots":
            if version == "0.26.11":
                parents = [
                    packages[target] for target in outgoing[index]
                    if packages[target]["name"] == "webpki-roots"
                ]
                if len(parents) != 1 or not patched_roots(parents[0]["version"]):
                    raise ValueError(
                        "webpki-roots 0.26.11 must resolve exactly one patched "
                        "webpki-roots parent (>= 1.0.10)"
                    )
            elif not patched_roots(version):
                raise ValueError(
                    f"webpki-roots {version} is vulnerable or unreviewed; "
                    "review RUSTSEC-2026-0381 before changing this guard"
                )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("lockfile", nargs="?", type=Path, default=Path("Cargo.lock"))
    args = parser.parse_args()
    try:
        with args.lockfile.open("rb") as lockfile:
            check(tomllib.load(lockfile))
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"audit guard: {error}", file=sys.stderr)
        return 1
    print("audit guard: TLS dependency exception remains scoped")
    return 0


if __name__ == "__main__":
    sys.exit(main())
