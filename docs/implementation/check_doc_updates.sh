#!/bin/bash
# Check which implementation docs need updating by comparing
# each doc's last edit commit against its corresponding module's commits.
#
# Usage: bash docs/implementation/check_doc_updates.sh

for doc in omnish-store omnish-pty omnish-llm omnish-context omnish-protocol omnish-tracker omnish-common omnish-transport omnish-client omnish-daemon; do
  doc_path="docs/implementation/${doc}.md"
  last_commit=$(git log -1 --format="%h %ci" -- "$doc_path" 2>/dev/null || echo "never")
  last_hash=$(git log -1 --format="%h" -- "$doc_path" 2>/dev/null || echo "")
  crate_path="crates/${doc}/src"
  if [ -n "$last_hash" ]; then
    changes=$(git log --oneline "${last_hash}..HEAD" -- "$crate_path" 2>/dev/null | wc -l)
    recent=$(git log --oneline "${last_hash}..HEAD" -- "$crate_path" 2>/dev/null)
  else
    changes=0
    recent=""
  fi
  echo "=== $doc ==="
  echo "  doc last edit: $last_commit"
  echo "  module changes since: $changes"
  if [ -n "$recent" ]; then
    echo "$recent" | sed 's/^/    /'
  fi
  echo ""
done
