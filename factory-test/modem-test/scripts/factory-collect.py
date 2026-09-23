#!/usr/bin/env python3
"""Collate factory-test RTT captures into one CSV.

Parses the tagged lines documented in docs/factory-test.md. Nothing here
knows what any step measures: steps, keys and units come from the log, so new
checks show up in the output without touching this script.

Usage:
    ./factory-collect.py board1.log board2.log ... > results.csv

Each row is one measurement or check. Boards are identified by the MCU unique
id the run prints, so captures do not have to be named carefully.
"""

import argparse
import csv
import re
import sys

# Strip the logger prefix: "[factory:INFO] - "
PREFIX = re.compile(r"^\[[^\]]*\]\s*-\s*")

FIELDS = [
    "board",
    "capture",
    "step",
    "step_name",
    "kind",
    "key",
    "value",
    "unit",
    "lo",
    "hi",
    "verdict",
]


def parse(path):
    """Yield rows for one capture, plus a (board, verdict, failed) summary."""
    board, result, failed = "unknown", "NO RESULT", ""
    step_names, rows = {}, []

    with open(path, encoding="utf-8", errors="replace") as handle:
        for raw in handle:
            line = PREFIX.sub("", raw.strip())
            fields = line.split()
            if not fields:
                continue
            tag = fields[0]

            # A run restarts on reset: keep only the last one in the capture
            if tag == "STEP" and len(fields) >= 4 and fields[3] == "START":
                if fields[1] == "1":
                    board, result, failed = "unknown", "NO RESULT", ""
                    step_names, rows = {}, []
                step_names[fields[1]] = fields[2]
                continue

            if tag == "STEP" and len(fields) >= 5:
                step_names[fields[1]] = fields[2]
                rows.append(
                    dict(
                        step=fields[1],
                        kind="STEP",
                        key="elapsed",
                        value=fields[4],
                        unit="ms",
                        lo="",
                        hi="",
                        verdict=fields[3],
                    )
                )
            elif tag == "MEAS" and len(fields) >= 8:
                rows.append(
                    dict(
                        step=fields[1],
                        kind="MEAS",
                        key=fields[2],
                        value=fields[3],
                        unit=fields[4],
                        lo=fields[5],
                        hi=fields[6],
                        verdict=fields[7],
                    )
                )
            elif tag == "CHECK" and len(fields) >= 4:
                rows.append(
                    dict(
                        step=fields[1],
                        kind="CHECK",
                        key=fields[2],
                        value="",
                        unit="",
                        lo="",
                        hi="",
                        verdict=fields[3],
                    )
                )
            elif tag == "INFO" and len(fields) >= 3:
                text = line.split(None, 3)[3] if len(fields) > 3 else ""
                if fields[2] == "uid":
                    board = text
                rows.append(
                    dict(
                        step=fields[1],
                        kind="INFO",
                        key=fields[2],
                        value=text,
                        unit="",
                        lo="",
                        hi="",
                        verdict="INFO",
                    )
                )
            elif line.startswith("FACTORY TEST:"):
                verdict = line[len("FACTORY TEST:") :].split()
                result = verdict[0] if verdict else "?"
                if "steps" in verdict:
                    failed = verdict[verdict.index("steps") + 1]

    for row in rows:
        row["board"] = board
        row["capture"] = path
        row["step_name"] = step_names.get(row["step"], "")
    return rows, (board, result, failed)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("captures", nargs="+", help="RTT capture files")
    args = parser.parse_args()

    writer = csv.DictWriter(sys.stdout, fieldnames=FIELDS)
    writer.writeheader()

    summaries = []
    for path in args.captures:
        rows, summary = parse(path)
        writer.writerows(rows)
        summaries.append((path, *summary))

    print(file=sys.stderr)
    for path, board, result, failed in summaries:
        detail = f" (steps {failed})" if failed else ""
        print(f"{path}: {board} {result}{detail}", file=sys.stderr)


if __name__ == "__main__":
    main()
