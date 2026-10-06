#!/usr/bin/env python3
"""Require the named Phase 1 artifacts from the language spec, not just a glob."""

import argparse
import re
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("corpus", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    spec = (root / "docs/language.md").read_text()
    _, heading, section = spec.partition("## 5. Phase 1 example suite\n")
    if not heading:
        parser.error("language spec is missing the Phase 1 example suite")
    section = section.split("\n## 6.", 1)[0]
    names = re.findall(r"^\d+\. `([^`]+\.siox)`$", section, re.MULTILINE)
    if not names or len(names) != len(set(names)):
        parser.error("Phase 1 example list is empty or contains duplicate artifacts")
    missing = [name for name in names if not (args.corpus / name).is_file()]
    if missing:
        parser.error("missing required Phase 1 examples: " + ", ".join(missing))
    print(f"{len(names)} specified Phase 1 examples present")


if __name__ == "__main__":
    main()
