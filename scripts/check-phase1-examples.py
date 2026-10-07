#!/usr/bin/env python3
"""Require the named Phase 1 artifacts in the corpus, not just a glob.

The list is section 5, "Phase 1 example suite", of the language spec
(siox-paper's docs/language.md); keep the two in step.
"""

import argparse
from pathlib import Path

PHASE1_EXAMPLES = [
    "basic_mux.siox",
    "register.siox",
    "counter.siox",
    "fsm.siox",
    "enum_event_monitor.siox",
    "packet_struct_event.siox",
    "stream_bus.siox",
    "producer_consumer.siox",
    "external_entity_stub.siox",
    "attribute_usage.siox",
    "counter_test.siox",
    "fsm_test.siox",
]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("corpus", type=Path)
    args = parser.parse_args()
    missing = [name for name in PHASE1_EXAMPLES if not (args.corpus / name).is_file()]
    if missing:
        parser.error("missing required Phase 1 examples: " + ", ".join(missing))
    print(f"{len(PHASE1_EXAMPLES)} specified Phase 1 examples present")


if __name__ == "__main__":
    main()
