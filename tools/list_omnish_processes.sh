#!/bin/bash
# Script to list omnish processes with runtime
# Uses OMNISH_STARTED env var if available, falls back to process start time from /proc
# Usage: ./list_omnish_processes.sh

# Function to parse environment variables from process
get_env_var() {
    local pid=$1
    local var_name=$2
    
    # Method 1: Try ps command
    local env_ps=$(ps -p "$pid" -ww -o environ 2>/dev/null | tail -n +2 | tr ' ' '\n' | grep "^${var_name}=" | cut -d= -f2-)
    if [ -n "$env_ps" ]; then
        echo "$env_ps"
        return 0
    fi
    
    # Method 2: Try /proc/[pid]/environ (might fail due to permissions)
    if [ -r "/proc/$pid/environ" ]; then
        cat "/proc/$pid/environ" 2>/dev/null | tr '\0' '\n' | grep "^${var_name}=" | cut -d= -f2-
    fi
}

# Function to get process start time from /proc/[pid]/stat (as Unix timestamp)
get_proc_start_epoch() {
    local pid=$1
    local stat_file="/proc/$pid/stat"
    
    if [ ! -r "$stat_file" ]; then
        return 1
    fi
    
    # Read stat file
    local stat_content
    stat_content=$(cat "$stat_file" 2>/dev/null)
    if [ -z "$stat_content" ]; then
        return 1
    fi
    
    # Field 22 is starttime (clock ticks after system boot)
    local starttime
    starttime=$(echo "$stat_content" | awk '{print $22}')
    if [ -z "$starttime" ]; then
        return 1
    fi
    
    # Get system boot time (seconds since epoch)
    local btime
    btime=$(grep '^btime' /proc/stat | awk '{print $2}')
    if [ -z "$btime" ]; then
        return 1
    fi
    
    # Get clock ticks per second (Hertz)
    local hertz
    hertz=$(getconf CLK_TCK 2>/dev/null)
    if [ -z "$hertz" ] || [ "$hertz" -lt 1 ]; then
        hertz=100  # Fallback to typical value
    fi
    
    # Calculate start time in seconds since epoch
    # starttime is in clock ticks, convert to seconds and add to boot time
    local start_seconds
    
    # Try awk first (more commonly available)
    if command -v awk >/dev/null 2>&1; then
        start_seconds=$(awk -v b="$btime" -v s="$starttime" -v h="$hertz" 'BEGIN {printf "%.0f", b + s / h}')
    # Fallback to bc
    elif command -v bc >/dev/null 2>&1; then
        start_seconds=$(echo "$btime + $starttime / $hertz" | bc 2>/dev/null)
    else
        # Integer division as last resort
        start_seconds=$((btime + starttime / hertz))
    fi
    
    if [ -z "$start_seconds" ]; then
        return 1
    fi
    
    # Return as integer
    echo "${start_seconds%.*}"
}

# Function to calculate runtime from start epoch
calculate_runtime() {
    local start_epoch=$1
    local now_epoch=$(date +%s)
    local diff_seconds=$((now_epoch - start_epoch))

    local days=$((diff_seconds / 86400))
    local hours=$(((diff_seconds % 86400) / 3600))
    local minutes=$(((diff_seconds % 3600) / 60))
    local seconds=$((diff_seconds % 60))

    if [ $days -gt 0 ]; then
        echo "${days}d ${hours}h ${minutes}m"
    elif [ $hours -gt 0 ]; then
        echo "${hours}h ${minutes}m ${seconds}s"
    else
        echo "${minutes}m ${seconds}s"
    fi
}

# Get all omnish-related processes (excluding bash shells)
ps aux | grep -E "omnish" | grep -v "bashrc" | grep -v "grep" | grep -v "list_omnish" | awk '{print $2}' | while read pid; do
    # Get basic process info
    line=$(ps -p "$pid" -o pid,cmd --no-headers 2>/dev/null)
    if [ -z "$line" ]; then
        continue
    fi

    proc_pid=$(echo "$line" | awk '{print $1}')
    cmd=$(echo "$line" | awk '{for(i=2;i<=NF;i++) printf "%s ", $i; print ""}')

    # Extract process name from command
    proc_name=$(echo "$cmd" | awk '{print $1}' | xargs basename 2>/dev/null)
    if [ -z "$proc_name" ]; then
        proc_name=$(echo "$cmd" | awk '{print $1}')
    fi

    # Get OMNISH_STARTED environment variable
    started_at=$(get_env_var "$pid" "OMNISH_STARTED")
    
    # Calculate runtime
    runtime="unknown"
    start_epoch=""
    
    # First try: use OMNISH_STARTED env var
    if [ -n "$started_at" ]; then
        # Try to parse as Unix timestamp (seconds since epoch)
        start_epoch=$(date -d "@$started_at" +%s 2>/dev/null)
        if [ $? -ne 0 ] || [ -z "$start_epoch" ]; then
            # Try parsing as RFC3339 or other date format
            start_epoch=$(date -d "$started_at" +%s 2>/dev/null)
        fi
    fi
    
    # Second try: fall back to process start time from /proc
    if [ -z "$start_epoch" ]; then
        start_epoch=$(get_proc_start_epoch "$pid")
    fi
    
    # Calculate runtime if we have a start epoch
    if [ -n "$start_epoch" ]; then
        runtime=$(calculate_runtime "$start_epoch")
    fi

    echo "$pid  $proc_name  $runtime"
done
