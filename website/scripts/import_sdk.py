#!/usr/bin/env python3
"""Turn the SDK README's crate and example tables into data/sdk.json.

Usage:
    python3 scripts/import_sdk.py

Reads ../sdk/README.md in this repository. The SDK's `make test` already
checks those tables against sdk/crates/ and sdk/examples/, so the website's
index follows the same source instead of a second hand-kept list.
"""

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT.parent / "sdk" / "README.md"
OUT = ROOT / "data" / "sdk.json"
BLOB = "https://github.com/EBonura/PSoXide/tree/main/sdk/"


def table(text, heading):
    section = text.split(f"## {heading}", 1)[1].split("\n## ", 1)[0]
    rows = [l for l in section.splitlines() if l.startswith("|")][2:]
    return [[c.strip() for c in r.strip().strip("|").split("|")] for r in rows]


def name_of(cell):
    return re.search(r"`([^`]+)`", cell).group(1)


def absolute(md):
    # Relative README links point into sdk/; make them work from the website.
    return re.sub(r"\]\((?!https?://)([^)]+)\)", lambda m: f"]({BLOB}{m.group(1)})", md)


def main():
    text = SRC.read_text()
    crates = [{"name": name_of(c), "url": f"{BLOB}crates/{name_of(c)}", "purpose": absolute(p)}
              for c, p in table(text, "Crates")]
    examples = [{"name": name_of(e), "url": f"{BLOB}examples/{name_of(e)}", "shows": absolute(s)}
                for e, s in table(text, "Examples")]
    OUT.write_text(json.dumps({"source": "sdk/README.md", "crates": crates, "examples": examples},
                              indent=2, ensure_ascii=False) + "\n")
    print(f"{len(crates)} crates, {len(examples)} examples -> {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
