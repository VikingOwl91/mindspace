#!/usr/bin/env python3
"""Run a read-only document query on a trusted local PDF or DOCX."""
import argparse
import json
from pathlib import Path
import sys

from formats import extract
from query import answer


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("file", type=Path)
    parser.add_argument("--profile", choices=["general", "paper", "bestiary"], default="general")
    operations = parser.add_subparsers(dest="operation", required=True)
    for command, argument, typ in (
        ("search", "text", str), ("section", "heading_or_path", str), ("procedure", "heading", str),
        ("sample-size", "experiment", int), ("cohort-counts", "experiment", int),
        ("resistance", "damage_type", str),
    ):
        operations.add_parser(command).add_argument("value", metavar=argument.upper(), type=typ)
    args = parser.parse_args()
    try:
        result = answer(extract(args.file), args.profile, args.operation, args.value)
    except (OSError, ValueError) as err:
        print(json.dumps({"error": str(err)}), file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2, ensure_ascii=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
