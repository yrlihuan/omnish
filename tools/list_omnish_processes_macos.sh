#!/bin/bash
# macOS-specific script to list omnish processes with runtime
# Uses ps command and lsof for process information
# Usage: ./list_omnish_processes_macos.sh

echo "Platform: macOS (Darwin)"
echo "PID  Process  Runtime"
echo "---  -------  -------"

# Find omnish processes using pgrep
pids=$(pgrep -fi "omnish" 2>/dev/null | grep -v "$$")

if [ -z "$pids" ]; then
    echo "No omnish processes found"
    exit 0
fi

for pid in $pids; do
    if [ -n "$pid" ] && [ "$pid" -gt 0 ]; then
        # Get full command path using lsof
        cmd_path=$(lsof -p "$pid" 2>/dev/null | grep ' txt ' | head -1 | awk '{print $9}')
        proc_name=$(echo "$cmd_path" | xargs basename 2>/dev/null)
        
        # Skip non-omnish processes
        if echo "$proc_name" | grep -qv "omnish"; then
            continue
        fi
        
        # Get runtime using ps etime
        runtime=$(ps -p "$pid" -o etime= 2>/dev/null | tr -d ' ')
        if [ -z "$runtime" ]; then
            runtime="unknown"
        fi
        
        echo "$pid  $proc_name  $runtime"
    fi
done