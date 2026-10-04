#!/usr/bin/env python3
"""Verify committed app/core/package versions before making release artifacts."""
import argparse
import os
from pathlib import Path
import re
import tomllib

ROOT = Path(__file__).resolve().parents[1]

def read_text(path):
    try:
        return (ROOT / path).read_text()
    except FileNotFoundError:
        raise SystemExit(f"Version source missing: {path}")
    except OSError as error:
        raise SystemExit(f"Cannot read {path}: {error}")

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag", default="")
    parser.add_argument("--print-version", action="store_true")
    args = parser.parse_args()
    try:
        version = tomllib.loads(read_text("Cargo.toml"))["package"]["version"]
    except (KeyError, TypeError, tomllib.TOMLDecodeError):
        raise SystemExit("Cannot parse package version from Cargo.toml")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise SystemExit("Release version must be major.minor.patch")
    if args.tag and args.tag != "v" + version:
        raise SystemExit("Tag must match the committed Cargo/application version")
    for path, pattern in [
        ("app/harbour-plexfreq.pro", r"^VERSION\s*=\s*(\S+)\s*$"),
        ("app/rpm/harbour-plexfreq.spec", r"^Version:\s*(\S+)\s*$"),
        ("app/src/main.cpp", r'setApplicationVersion\("([^"\n]+)"\)'),
        ("CMakeLists.txt", r"project\(PlexFreq VERSION (\S+) LANGUAGES"),
    ]:
        match = re.search(pattern, read_text(path), re.MULTILINE)
        if not match or match[1] != version:
            raise SystemExit(f"Version mismatch in {path}")
    if args.print_version:
        print(version)
    else:
        print(f"Release version: {version}")
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
                output.write(f"version={version}\n")

if __name__ == "__main__":
    main()
