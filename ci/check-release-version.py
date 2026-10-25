"""Refuse a release tag that disagrees with Cargo.toml."""
import sys
import tomllib
from pathlib import Path


def check(tag, manifest):
    version = tomllib.loads(manifest)["package"]["version"]
    if tag != f"v{version}":
        raise ValueError(f"Release tag {tag} must be v{version}")


if __name__ == "__main__":
    check(sys.argv[1], Path("Cargo.toml").read_text())
