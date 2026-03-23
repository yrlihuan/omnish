#!/usr/bin/env python3
"""Find duplicate code blocks in Rust source files.

Scans all .rs files under crates/, extracts normalized line sequences,
and reports blocks of N+ consecutive lines that appear in multiple locations.

Usage:
    python3 tools/find_duplicates.py [--min-lines N] [--path DIR]
"""

import argparse
import re
from collections import defaultdict
from pathlib import Path


def normalize_line(line: str) -> str:
    """Normalize a line for comparison: strip comments, whitespace, collapse spaces."""
    # Remove line comments
    s = re.sub(r'//.*$', '', line)
    # Strip and collapse whitespace
    s = s.strip()
    s = re.sub(r'\s+', ' ', s)
    return s


def is_meaningful(line: str) -> bool:
    """Check if a normalized line is meaningful for duplicate detection."""
    if not line:
        return False
    # Skip trivial lines
    trivial = {'{', '}', '};', '),', ')', '(', '},', '});', 'Ok(())', 'None',
               'Some(', '_ => {}', '_ => {', 'else {', '} else {', '} else',
               '#[test]', '#[cfg(test)]', 'use super::*;', 'fn main() {',
               'return;', 'break;', 'continue;'}
    if line in trivial:
        return False
    # Skip pure use/mod statements (too generic)
    if re.match(r'^(use |mod |pub mod |pub use )', line):
        return False
    return True


def find_duplicates(root: Path, min_lines: int = 6):
    """Find duplicate code blocks across Rust files."""
    # Collect all .rs files
    rs_files = sorted(root.rglob('*.rs'))
    if not rs_files:
        print(f"No .rs files found under {root}")
        return

    # For each file, build normalized line list (skipping test modules)
    file_lines: dict[Path, list[tuple[int, str]]] = {}  # path -> [(orig_lineno, normalized)]

    for path in rs_files:
        try:
            content = path.read_text()
        except Exception:
            continue

        lines = content.split('\n')
        normalized = []
        in_test_mod = False
        brace_depth = 0

        for i, line in enumerate(lines, 1):
            stripped = line.strip()

            # Track test module to skip test code
            if re.match(r'#\[cfg\(test\)\]', stripped):
                in_test_mod = True
                continue
            if in_test_mod:
                if stripped.startswith('mod ') and '{' in stripped:
                    brace_depth = 1
                    continue
                if brace_depth > 0:
                    brace_depth += stripped.count('{') - stripped.count('}')
                    if brace_depth <= 0:
                        in_test_mod = False
                        brace_depth = 0
                    continue
                # Single-line after #[cfg(test)] without mod block
                if not stripped.startswith('mod '):
                    in_test_mod = False

            norm = normalize_line(line)
            if is_meaningful(norm):
                normalized.append((i, norm))

        if normalized:
            file_lines[path] = normalized

    # Build hash of N-line windows -> list of (file, start_line)
    block_locations: dict[tuple[str, ...], list[tuple[Path, int]]] = defaultdict(list)

    for path, lines in file_lines.items():
        norms = [n for _, n in lines]
        line_nums = [ln for ln, _ in lines]
        for i in range(len(norms) - min_lines + 1):
            block = tuple(norms[i:i + min_lines])
            block_locations[block].append((path, line_nums[i]))

    # Filter to blocks appearing in 2+ distinct files (or 2+ distant locations in same file)
    duplicates: list[tuple[tuple[str, ...], list[tuple[Path, int]]]] = []
    seen_groups: set[frozenset[tuple[str, int]]] = set()

    for block, locations in block_locations.items():
        if len(locations) < 2:
            continue

        # Deduplicate overlapping windows: group by file, keep only non-overlapping
        by_file: dict[Path, list[int]] = defaultdict(list)
        for path, line in locations:
            by_file[path].append(line)

        # Must appear in 2+ files, or 2+ non-adjacent spots in same file
        distinct_files = len(by_file)
        if distinct_files < 2:
            # Check if same file has non-adjacent occurrences
            for path, line_nums_list in by_file.items():
                sorted_lines = sorted(line_nums_list)
                if len(sorted_lines) >= 2 and (sorted_lines[-1] - sorted_lines[0]) > min_lines * 2:
                    break
            else:
                continue

        # Dedup: create a signature for this group
        sig = frozenset((str(p), ln) for p, ln in locations)
        if sig in seen_groups:
            continue
        seen_groups.add(sig)

        duplicates.append((block, locations))

    # Sort by block length (try to find maximal duplicates) then by count
    # Extend blocks greedily to find maximal length
    extended_duplicates = []
    for block, locations in duplicates:
        # Try to extend this block
        max_extra = 50  # don't search forever
        best_len = min_lines

        for extra in range(1, max_extra + 1):
            extended_len = min_lines + extra
            # Check if all locations still match with extended length
            all_match = True
            extended_block = None

            for path, start_line in locations:
                lines = file_lines.get(path, [])
                norms = [n for _, n in lines]
                line_nums = [ln for ln, _ in lines]

                # Find the index where this location starts
                try:
                    idx = next(i for i, ln in enumerate(line_nums) if ln == start_line)
                except StopIteration:
                    all_match = False
                    break

                if idx + extended_len > len(norms):
                    all_match = False
                    break

                candidate = tuple(norms[idx:idx + extended_len])
                if extended_block is None:
                    extended_block = candidate
                elif candidate != extended_block:
                    all_match = False
                    break

            if all_match and extended_block:
                best_len = extended_len
            else:
                break

        extended_duplicates.append((best_len, block, locations))

    # Deduplicate: if a shorter block's locations are a subset of a longer block's, skip it
    extended_duplicates.sort(key=lambda x: -x[0])  # longest first

    reported: list[tuple[int, list[tuple[Path, int]], tuple[str, ...]]] = []
    for length, block, locations in extended_duplicates:
        loc_set = {(str(p), ln) for p, ln in locations}
        # Check if this is subsumed by an already-reported longer block
        subsumed = False
        for rep_len, rep_locs, _ in reported:
            if rep_len >= length:
                rep_set = {(str(p), ln) for p, ln in rep_locs}
                # Check if every location in current is "near" a reported location
                if all(
                    any(sp == rp and abs(sl - rl) < rep_len for rp, rl in rep_set)
                    for sp, sl in loc_set
                ):
                    subsumed = True
                    break
        if not subsumed:
            reported.append((length, locations, block))

    if not reported:
        print("No significant duplicate code blocks found.")
        return

    # Print results
    print(f"Found {len(reported)} duplicate code groups:\n")
    for i, (length, locations, block) in enumerate(reported, 1):
        # Deduplicate locations (keep first per file or distant ones)
        unique_locs = []
        seen_file_lines: set[tuple[str, int]] = set()
        for path, line in sorted(locations, key=lambda x: (str(x[0]), x[1])):
            key = (str(path), line)
            if key not in seen_file_lines:
                seen_file_lines.add(key)
                unique_locs.append((path, line))

        if len(unique_locs) < 2:
            continue

        print(f"── Group {i}: {length} similar lines, {len(unique_locs)} locations ──")
        for path, line in unique_locs[:5]:  # show at most 5 locations
            rel = path.relative_to(root) if path.is_relative_to(root) else path
            print(f"  {rel}:{line}")
        if len(unique_locs) > 5:
            print(f"  ... and {len(unique_locs) - 5} more")

        # Show the block content (first few lines)
        preview_lines = min(10, length)
        print(f"  Content ({length} lines, showing {preview_lines}):")
        for line in block[:preview_lines]:
            print(f"    {line}")
        if length > preview_lines:
            print(f"    ...")
        print()


def main():
    parser = argparse.ArgumentParser(description='Find duplicate code blocks in Rust files')
    parser.add_argument('--min-lines', type=int, default=6,
                        help='Minimum consecutive lines to consider as duplicate (default: 6)')
    parser.add_argument('--path', type=str, default='crates',
                        help='Directory to scan (default: crates)')
    args = parser.parse_args()

    root = Path(args.path)
    if not root.exists():
        print(f"Error: {root} does not exist")
        return

    find_duplicates(root, args.min_lines)


if __name__ == '__main__':
    main()
