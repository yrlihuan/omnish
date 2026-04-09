#!/bin/bash
# List sections of a module doc split by ## headings, showing line ranges.
#
# Usage:
#   bash split_doc_sections.sh <file>
#
# Example:
#   bash docs/implementation/split_doc_sections.sh docs/implementation/omnish-pty.md

set -euo pipefail

if [[ $# -lt 1 || ! -f "$1" ]]; then
  echo "Usage: $0 <file>" >&2
  exit 1
fi

FILE="$1"

# Collect all ## heading line numbers and titles
mapfile -t HEADING_LINES < <(grep -n '^## ' "$FILE" | sed 's/^\([0-9]*\):## /\1\t/')
TOTAL_LINES=$(wc -l < "$FILE")

# Build arrays of start_line, end_line, title
STARTS=()
ENDS=()
TITLES=()
COUNT=${#HEADING_LINES[@]}

for i in "${!HEADING_LINES[@]}"; do
  line="${HEADING_LINES[$i]}"
  start="${line%%	*}"
  title="${line#*	}"
  STARTS+=("$start")
  TITLES+=("$title")

  # Previous section ends at current heading - 1
  if [[ $i -gt 0 ]]; then
    ENDS+=("$((start - 1))")
  fi
done

# Last section ends at file end
if [[ $COUNT -gt 0 ]]; then
  ENDS+=("$TOTAL_LINES")
fi

# Also capture the preamble (lines before first ##) as section 0
if [[ $COUNT -gt 0 && ${STARTS[0]} -gt 1 ]]; then
  HAS_PREAMBLE=1
  PREAMBLE_END=$((STARTS[0] - 1))
else
  HAS_PREAMBLE=0
fi

if [[ $HAS_PREAMBLE -eq 1 ]]; then
  echo "  0: [L1-L${PREAMBLE_END}] (preamble)"
fi
for i in "${!TITLES[@]}"; do
  idx=$((i + 1))
  echo "  ${idx}: [L${STARTS[$i]}-L${ENDS[$i]}] ${TITLES[$i]}"
done
echo ""
echo "Total: $COUNT sections"
