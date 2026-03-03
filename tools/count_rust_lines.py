#!/usr/bin/env python3
"""Count Rust code lines, separating production code from test code."""

import os
import re
from pathlib import Path


def count_rust_lines(file_path: Path) -> tuple[int, int]:
    """Count total, production, and test lines for a single file."""
    content = file_path.read_text()
    lines = content.split('\n')

    total_lines = len(lines)
    test_lines = 0
    prod_lines = 0

    # Track whether we're inside a test module
    in_test_mod = False
    test_mod_indent = 0

    # Track test function depth
    in_test_fn = False
    test_fn_indent = 0

    for line in lines:
        stripped = line.strip()
        current_indent = len(line) - len(line.lstrip())

        # Check for test module: #[cfg(test)] mod tests {
        if re.match(r'#\[cfg\(test\)\]\s*mod\s+\w+\s*\{', stripped):
            in_test_mod = True
            test_mod_indent = current_indent
            continue

        # Check for #[cfg(test)] mod block (inline tests)
        if re.match(r'#\[cfg\(test\)\]\s*\{', stripped):
            in_test_mod = True
            test_mod_indent = current_indent
            continue

        # Check for test function: #[test] or #[tokio::test]
        if stripped.startswith('#[') and ('test' in stripped and 'cfg' not in stripped):
            in_test_fn = True
            test_fn_indent = current_indent
            test_lines += 1
            continue

        # Check for end of test function
        if in_test_fn:
            test_lines += 1
            # End of function: closing brace at same or less indentation
            if stripped == '}' and current_indent <= test_fn_indent:
                in_test_fn = False
                test_fn_indent = 0
            continue

        # Check for end of test module
        if in_test_mod:
            test_lines += 1
            if stripped == '}' and current_indent <= test_mod_indent:
                in_test_mod = False
                test_mod_indent = 0
            continue

        prod_lines += 1

    return total_lines, prod_lines, test_lines


def main():
    project_root = Path('.')
    crates_dir = project_root / 'crates'

    total_lines = 0
    total_prod_lines = 0
    total_test_lines = 0

    print("Rust Code Line Count")
    print("=" * 60)
    print()

    # Process each crate
    for crate in sorted(crates_dir.iterdir()):
        if not crate.is_dir():
            continue

        src_dir = crate / 'src'
        if not src_dir.exists():
            continue

        crate_name = crate.name
        crate_lines = 0
        crate_prod_lines = 0
        crate_test_lines = 0

        # Find all .rs files in the crate
        for rs_file in sorted(src_dir.rglob('*.rs')):
            total, prod, test = count_rust_lines(rs_file)
            crate_lines += total
            crate_prod_lines += prod
            crate_test_lines += test

        print(f"{crate_name:30} {crate_lines:6} lines (prod: {crate_prod_lines:5}, test: {crate_test_lines:5})")

        total_lines += crate_lines
        total_prod_lines += crate_prod_lines
        total_test_lines += crate_test_lines

    print()
    print("-" * 60)
    print(f"{'TOTAL':30} {total_lines:6} lines (prod: {total_prod_lines:5}, test: {total_test_lines:5})")


if __name__ == '__main__':
    main()
