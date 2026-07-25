#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
    echo "usage: $0 <directory> [output-file]" >&2
    exit 2
fi

readonly input_dir=$1
readonly script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
readonly analyzer="$script_dir/highlight-analyzer.py"
readonly output_file=${2:-"$script_dir/../analysis/batch-report.txt"}

if [[ ! -d $input_dir ]]; then
    echo "input directory does not exist: $input_dir" >&2
    exit 1
fi

mapfile -d '' files < <(find "$input_dir" -type f -name '*.al' -print0 | sort -z)
if (( ${#files[@]} == 0 )); then
    echo "no AL files found under $input_dir" >&2
    exit 1
fi

mkdir -p "$(dirname "$output_file")"
results=$(mktemp)
trap 'rm -f "$results"' EXIT

for file in "${files[@]}"; do
    python3 "$analyzer" "$file" --json --summary >> "$results"
done

python3 - "$results" "$input_dir" "${#files[@]}" > "$output_file" <<'PY'
import json
import sys
from collections import defaultdict
from pathlib import Path

results_path, input_dir, file_count = sys.argv[1:]
raw = Path(results_path).read_text(encoding="utf-8")
decoder = json.JSONDecoder()
offset = 0
totals = defaultdict(lambda: {"count": 0, "examples": set()})

while offset < len(raw):
    while offset < len(raw) and raw[offset].isspace():
        offset += 1
    if offset == len(raw):
        break
    result, offset = decoder.raw_decode(raw, offset)
    for capture, info in result["summary"].items():
        totals[capture]["count"] += info["count"]
        totals[capture]["examples"].update(info.get("examples", []))

print("Batch highlight analysis")
print(f"Input: {input_dir}")
print(f"Files: {file_count}")
print()
print("Capture | Count | Examples")
print("-" * 72)
for capture in sorted(totals):
    info = totals[capture]
    examples = sorted(info["examples"])[:5]
    print(f"{capture:28} | {info['count']:7} | {examples}")
PY

echo "wrote $output_file"
