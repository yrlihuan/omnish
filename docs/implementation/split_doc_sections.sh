#!/bin/bash
# Split a module doc into sections by ## headings.
#
# Usage:
#   bash split_doc_sections.sh <file> list          # list all sections with line ranges
#   bash split_doc_sections.sh <file> get <name>    # print a section's content
#   bash split_doc_sections.sh <file> get <number>  # print section by 1-based index
#
# Examples:
#   bash docs/implementation/split_doc_sections.sh docs/implementation/omnish-pty.md list
#   bash docs/implementation/split_doc_sections.sh docs/implementation/omnish-pty.md get "重要数据结构"
#   bash docs/implementation/split_doc_sections.sh docs/implementation/omnish-pty.md get 3

set -euo pipefail

usage() {
  echo "Usage:"
  echo "  $0 <file> list"
  echo "  $0 <file> get <section-name-or-number>"
  exit 1
}

[[ $# -lt 2 ]] && usage

FILE="$1"
ACTION="$2"

if [[ ! -f "$FILE" ]]; then
  echo "Error: file not found: $FILE" >&2
  exit 1
fi

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
  PREAMBLE_END=0
fi

case "$ACTION" in
  list)
    if [[ $HAS_PREAMBLE -eq 1 ]]; then
      echo "  0: [L1-L${PREAMBLE_END}] (preamble)"
    fi
    for i in "${!TITLES[@]}"; do
      idx=$((i + 1))
      echo "  ${idx}: [L${STARTS[$i]}-L${ENDS[$i]}] ${TITLES[$i]}"
    done
    echo ""
    echo "Total: $COUNT sections"
    ;;

  get)
    [[ $# -lt 3 ]] && { echo "Error: 'get' requires a section name or number" >&2; usage; }
    TARGET="$3"

    # Check if target is a number
    if [[ "$TARGET" =~ ^[0-9]+$ ]]; then
      IDX="$TARGET"
      if [[ "$IDX" -eq 0 ]]; then
        if [[ $HAS_PREAMBLE -eq 1 ]]; then
          sed -n "1,${PREAMBLE_END}p" "$FILE"
          exit 0
        else
          echo "Error: no preamble in this file" >&2
          exit 1
        fi
      fi
      ARR_IDX=$((IDX - 1))
      if [[ $ARR_IDX -lt 0 || $ARR_IDX -ge $COUNT ]]; then
        echo "Error: section number $IDX out of range (1-$COUNT)" >&2
        exit 1
      fi
      sed -n "${STARTS[$ARR_IDX]},${ENDS[$ARR_IDX]}p" "$FILE"
    else
      # Search by name (substring match)
      FOUND=0
      for i in "${!TITLES[@]}"; do
        if [[ "${TITLES[$i]}" == *"$TARGET"* ]]; then
          sed -n "${STARTS[$i]},${ENDS[$i]}p" "$FILE"
          FOUND=1
          break
        fi
      done
      if [[ $FOUND -eq 0 ]]; then
        echo "Error: no section matching '$TARGET'" >&2
        echo "Available sections:" >&2
        for i in "${!TITLES[@]}"; do
          echo "  $((i+1)): ${TITLES[$i]}" >&2
        done
        exit 1
      fi
    fi
    ;;

  *)
    echo "Error: unknown action '$ACTION'" >&2
    usage
    ;;
esac
