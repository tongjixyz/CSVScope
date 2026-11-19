"""Deterministic CSV benchmark fixture with quoted commas, newlines and long text."""
import argparse
import csv
import json
import pathlib
import time

parser = argparse.ArgumentParser()
parser.add_argument("path", type=pathlib.Path)
parser.add_argument("--mib", type=int, default=1024)
parser.add_argument("--characters", type=int, default=16384)
args = parser.parse_args()
args.path.parent.mkdir(parents=True, exist_ok=True)
start = time.perf_counter()
row = 0
payload = ('通信测试 payload, "quoted"\n' * (args.characters // 22 + 1))[:args.characters]
with args.path.open("x", encoding="utf-8", newline="") as output:
    writer = csv.writer(output)
    writer.writerow([f"column_{c}" for c in range(12)])
    while output.tell() < args.mib * 1024 * 1024:
        writer.writerow([
            f"小区{row % 100}", (1, 5, 9, 7)[row % 4], f"00{row:018d}",
            f"2026-11-18 12:{(row // 60) % 60:02d}:{row % 60:02d}",
            f"{row} {payload}", *(f"value_{c}_{row % 200}" for c in range(5, 12)),
        ])
        row += 1
print(json.dumps({
    "status": "generated", "path": str(args.path.resolve()), "actualBytes": args.path.stat().st_size,
    "rows": row, "columns": 12, "payloadCharacters": args.characters,
    "features": ["UTF-8", "Chinese", "embedded newlines", "quoted commas and quotes", "unique long identifiers"],
    "generationSeconds": time.perf_counter() - start,
}, ensure_ascii=False))
