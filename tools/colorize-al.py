#!/usr/bin/env python3
"""
Displays AL files with tree-sitter syntax highlighting in the terminal.

Usage:
    python colorize-al.py <al_file>

"""

import subprocess
import re
import sys
import os
from pathlib import Path
from typing import Dict, List

COLORS = {
    "@keyword": "\033[38;5;39m",           # Bright blue
    "@keyword.control": "\033[38;5;33m",   # Blue

    "@type": "\033[38;5;44m",              # Cyan
    "@type.builtin": "\033[38;5;44m",      # Cyan

    "@function": "\033[38;5;220m",         # Yellow
    "@function.call": "\033[38;5;220m",    # Yellow
    "@function.method.call": "\033[38;5;178m",  # Gold
    "@function.definition": "\033[38;5;214m",   # Orange-yellow

    "@variable": "\033[38;5;153m",         # Light blue
    "@variable.declaration": "\033[38;5;117m",  # Lighter blue
    "@variable.parameter": "\033[38;5;208m",    # Orange

    "@string": "\033[38;5;173m",           # Orange-brown
    "@number": "\033[38;5;149m",           # Light green
    "@comment": "\033[38;5;65m",           # Gray-green
    "@operator": "\033[38;5;252m",         # Light gray
    "@punctuation": "\033[38;5;245m",      # Medium gray
    "@punctuation.bracket": "\033[38;5;245m",
    "@punctuation.delimiter": "\033[38;5;245m",

    "@constant": "\033[38;5;141m",         # Purple
    "@constant.builtin": "\033[38;5;141m", # Purple

    "@property": "\033[38;5;116m",         # Light cyan
    "@attribute": "\033[38;5;150m",        # Light green
    "@title": "\033[1;97m",                # Bold bright white
    "default": "\033[0m",
}

RESET = "\033[0m"

SCRIPT_DIR = Path(__file__).parent
GRAMMAR_ROOT = SCRIPT_DIR.parent
QUERIES_DIR = GRAMMAR_ROOT / "queries"
HIGHLIGHTS_SCM = QUERIES_DIR / "highlights.scm"


def run_tree_sitter_query(file_path: str) -> str:
    """Run tree-sitter query and return captures."""
    result = subprocess.run(
        ["tree-sitter", "query", str(HIGHLIGHTS_SCM), file_path, "--captures"],
        capture_output=True,
        text=True,
        cwd=GRAMMAR_ROOT
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or "tree-sitter query failed")
    return result.stdout


def parse_captures(query_output: str) -> List[Dict]:
    """Parse tree-sitter query capture output."""
    captures = []

    pattern = r'pattern:\s*\d+,\s*capture:\s*\d+\s*-\s*(\w+(?:\.\w+)*),\s*start:\s*\((\d+),\s*(\d+)\),\s*end:\s*\((\d+),\s*(\d+)\)'

    for match in re.finditer(pattern, query_output):
        captures.append({
            "capture": f"@{match.group(1)}",
            "start_row": int(match.group(2)),
            "start_col": int(match.group(3)),
            "end_row": int(match.group(4)),
            "end_col": int(match.group(5)),
        })

    return captures


def colorize_line(line: str, line_num: int, captures: List[Dict]) -> str:
    """Apply colors to a single line based on captures."""
    line_captures = [
        c for c in captures
        if c["start_row"] == line_num and c["end_row"] == line_num
    ]

    if not line_captures:
        return line

    line_captures.sort(key=lambda c: c["start_col"], reverse=True)

    seen_positions = set()
    unique_captures = []
    for cap in reversed(line_captures):
        pos_key = (cap["start_col"], cap["end_col"])
        if pos_key not in seen_positions:
            seen_positions.add(pos_key)
            unique_captures.append(cap)
    unique_captures.reverse()

    result = line
    for cap in unique_captures:
        color = COLORS.get(cap["capture"], COLORS.get("default", ""))
        start = cap["start_col"]
        end = cap["end_col"]

        if start < len(result) and end <= len(result):
            result = result[:start] + color + result[start:end] + RESET + result[end:]

    return result


def print_legend():
    """Print a color legend."""
    print("\n=== Color Legend ===")
    categories = {
        "Keywords": ["@keyword", "@keyword.control"],
        "Types": ["@type", "@type.builtin"],
        "Functions": ["@function", "@function.call", "@function.method.call"],
        "Variables": ["@variable", "@variable.parameter"],
        "Literals": ["@string", "@number", "@constant.builtin"],
        "Other": ["@property", "@attribute", "@operator", "@punctuation"],
    }

    for category, captures in categories.items():
        samples = []
        for cap in captures:
            color = COLORS.get(cap, "")
            samples.append(f"{color}{cap}{RESET}")
        print(f"  {category}: {', '.join(samples)}")
    print()


def main():
    if len(sys.argv) < 2:
        print("Usage: python colorize-al.py <al_file>")
        sys.exit(1)

    file_path = sys.argv[1]

    if not os.path.exists(file_path):
        print(f"Error: File not found: {file_path}")
        sys.exit(1)

    with open(file_path, 'r') as f:
        lines = f.readlines()

    try:
        query_output = run_tree_sitter_query(file_path)
    except RuntimeError as error:
        print(f"Error: {error}", file=sys.stderr)
        sys.exit(1)
    captures = parse_captures(query_output)

    print(f"\n=== Syntax Highlighted: {file_path} ===\n")

    for line_num, line in enumerate(lines):
        line = line.rstrip('\n\r')
        colorized = colorize_line(line, line_num, captures)
        print(f"{line_num + 1:4} | {colorized}")

    print_legend()

    capture_types = {}
    for cap in captures:
        capture_types[cap["capture"]] = capture_types.get(cap["capture"], 0) + 1

    print("=== Capture Statistics ===")
    for cap_type, count in sorted(capture_types.items()):
        color = COLORS.get(cap_type, "")
        print(f"  {color}{cap_type:25}{RESET}: {count}")


if __name__ == "__main__":
    main()
