# Client-Side Per-Command Output Throttle

**Date:** 2026-02-19
**Status:** Approved

## Problem

Long-running, screen-refreshing commands (`top`, `htop`, `watch`, `tail -f`) produce massive amounts of PTY output data. This data is sent as IoData messages to the daemon and stored in `stream.bin`, causing unbounded disk growth. A single `top` session running for 1 hour can produce 100-200MB of stream data, most of which is redundant screen redraws.

## Goal

Reduce disk usage and network overhead by throttling IoData output messages on the client side, per command. The first portion of output is sent at full speed; once a threshold is exceeded, sending rate drops to a low ceiling, keeping the daemon aware but dramatically reducing data volume.

## Design

### OutputThrottle State Machine

```
Normal    — command_bytes < THRESHOLD (2MB default)
            → send every IoData chunk at full speed

Throttled — command_bytes >= THRESHOLD
            → rate-limit sends to THROTTLE_RATE (10kB/s default)
            → chunks exceeding the rate budget are silently dropped

Reset     — new command detected (CommandComplete fired)
            → reset counters, return to Normal
```

### Component: `OutputThrottle`

New file: `crates/omnish-client/src/throttle.rs`

**Fields:**
- `command_bytes: u64` — cumulative bytes sent for the current command
- `last_send_time: Instant` — timestamp of last send (for token bucket refill)
- `throttle_bucket: f64` — token bucket remaining allowance in bytes
- `threshold_bytes: u64` — configurable threshold (default 2MB)
- `throttle_rate: f64` — bytes/sec rate limit once throttled (default 10240.0)

**Methods:**
- `fn new(threshold_bytes: u64, throttle_rate: f64) -> Self`
- `fn should_send(&mut self, chunk_len: usize) -> bool` — returns true if this chunk should be sent. In Normal phase, always true (and advances `command_bytes`). In Throttled phase, uses token bucket: refills at `throttle_rate` per elapsed second, deducts `chunk_len`. Returns true only if bucket has sufficient tokens.
- `fn record_sent(&mut self, n: usize)` — called after successful send; updates `command_bytes` and drains the token bucket.
- `fn reset(&mut self)` — resets `command_bytes`, `throttle_bucket`, and `last_send_time` for next command.

### Token Bucket Algorithm (Throttled phase)

```
elapsed = now - last_send_time
last_send_time = now
throttle_bucket += elapsed.as_secs_f64() * throttle_rate
throttle_bucket = min(throttle_bucket, throttle_rate)  // cap at 1 second burst
if throttle_bucket >= chunk_len:
    throttle_bucket -= chunk_len
    return true
else:
    return false
```

### Integration in main.rs

**Output IoData sending (1 call site):**

```rust
// Before:
send_or_buffer(rpc, msg, &pending_buffer).await;

// After:
if throttle.should_send(n) {
    send_or_buffer(rpc, msg, &pending_buffer).await;
    throttle.record_sent(n);
}
```

**Command boundary reset:**

After `command_tracker.feed_output()` returns completed commands, call `throttle.reset()`.

### Scope of Changes

**Changed:**
- `crates/omnish-client/src/main.rs` — create `OutputThrottle`, integrate in output path, reset on command complete

**New:**
- `crates/omnish-client/src/throttle.rs` — `OutputThrottle` struct and tests

**Unchanged:**
- Input IoData — user input volume is negligible, no throttling
- Daemon side — no changes; it stores whatever it receives
- `stream.bin` format — unchanged
- `CommandRecord` offset/length — daemon computes these from its own write position, unaffected

### Constants (hardcoded initially)

```rust
const THROTTLE_THRESHOLD_BYTES: u64 = 2 * 1024 * 1024;  // 2MB
const THROTTLE_RATE_BYTES_PER_SEC: f64 = 10_240.0;       // 10kB/s
```

### Estimated Impact

`top` running for 1 hour:
- Without throttle: ~100-200MB
- With throttle: ~2MB + ~36MB = ~38MB (2MB full speed + 10kB/s × 3600s)

`ls` (normal short command):
- No effect — output well under 2MB threshold
