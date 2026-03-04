#!/usr/bin/env bash
# Dump all thread stack traces of the omnish daemon process.
# Usage: ./tools/dump-daemon-stacks.sh [PID]
#   If PID is omitted, auto-detects the omnishd process.

set -euo pipefail

pid="${1:-}"

if [ -z "$pid" ]; then
    pid=$(pgrep -x omnish-daemon 2>/dev/null || true)
    if [ -z "$pid" ]; then
        echo "error: omnishd process not found. Pass PID as argument or ensure omnishd is running." >&2
        exit 1
    fi
    # If multiple matches, take the first
    pid=$(echo "$pid" | head -1)
fi

if ! kill -0 "$pid" 2>/dev/null; then
    echo "error: process $pid does not exist or is not accessible." >&2
    exit 1
fi

echo "=== omnishd stack dump (pid=$pid) at $(date -Iseconds) ==="
echo ""

if command -v gdb >/dev/null 2>&1; then
    gdb -batch -ex "set pagination off" -ex "thread apply all bt" -p "$pid" 2>/dev/null
elif command -v eu-stack >/dev/null 2>&1; then
    eu-stack -p "$pid"
elif [ -d "/proc/$pid/task" ]; then
    echo "(no gdb/eu-stack; falling back to /proc stack traces)"
    echo ""
    for tid_dir in /proc/"$pid"/task/*/; do
        tid=$(basename "$tid_dir")
        comm=$(cat "$tid_dir/comm" 2>/dev/null || echo "?")
        echo "--- Thread $tid ($comm) ---"
        cat "$tid_dir/stack" 2>/dev/null || echo "(cannot read stack)"
        echo ""
    done
else
    echo "error: no stack dump tool available (need gdb, eu-stack, or /proc filesystem)." >&2
    exit 1
fi
