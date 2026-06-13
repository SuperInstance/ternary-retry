# ternary-retry

Retry policy engine for GPU kernel execution with **ternary outcome classification**: every attempt resolves to exactly one of `+1` (success), `0` (retryable transient failure), or `-1` (permanent failure). Built on exponential backoff with jitter and circuit-breaker protection.

## Why It Matters

GPU kernel launches fail in qualitatively different ways. A transient ECC error is retryable; a CUDA illegal-memory-access is not. Binary retry policies conflate these, wasting GPU time retrying doomed kernels or aborting on recoverable errors. Ternary classification gives the scheduler enough information to make the right decision without human inspection:

| Outcome | Code | Action |
|---------|------|--------|
| Success | `+1` | Proceed |
| Retryable | `0` | Backoff, retry |
| PermanentFail | `-1` | Abort, do not retry |

The circuit breaker adds a second safety layer: if retryable failures stack up past a threshold, the breaker trips to `Open` and blocks all further attempts for a recovery period, preventing cascading failures from overwhelming an already-degraded GPU.

## How It Works

### Exponential Backoff with Jitter

The delay before attempt *n* is:

```
delay(n) = min(base · 2ⁿ + jitter(n), max_delay)
```

where `jitter(n) = (base · 2ⁿ / 10) · ((7n + 13) mod 10) / 10`.

The jitter is deterministic-per-attempt (no RNG dependency) yet varies enough across attempts to decorrelate concurrent retries. The `min` cap prevents overflow-driven delays.

**Complexity:** O(1) per backoff computation.

### Circuit Breaker State Machine

The breaker follows a three-state model mapped to ternary values:

```
        successes →           failures ≥ threshold →
    ┌──────────────────┐   ┌────────────────────┐
    │    Closed (+1)   │←──│    Open (-1)       │
    │  (normal traffic) │   │  (all calls blocked)│
    └──────────────────┘   └────────────────────┘
             ↑                       │
             │   probe success       │ recovery_timeout elapsed
             │                       ↓
             │              ┌─────────────────┐
             └──────────────│  HalfOpen (0)   │
               probe fail → │  (single probe)  │
                            └─────────────────┘
```

- **Closed (+1):** Requests flow normally. Consecutive failures increment a counter.
- **Open (-1):** All requests blocked. After `recovery_timeout_us`, transitions to HalfOpen.
- **HalfOpen (0):** Exactly one probe request is allowed. Success → Closed; failure → Open.

**Complexity:** O(1) per decision. Space: O(1) state.

### Success Rate Metric

```
success_rate = successes / (successes + retries + permanent_fails)
```

Undefined (returns 1.0) when no attempts have been recorded.

## Quick Start

```rust
use ternary_retry::{RetryPolicy, Outcome, CircuitBreaker};

let mut policy = RetryPolicy::new(max_retries: 3, base_delay_us: 1000, max_delay_us: 100_000);

// Simulate kernel launches
policy.record(Outcome::Retryable, latency_us: 50);
policy.record(Outcome::Retryable, latency_us: 120);
policy.record(Outcome::Success,   latency_us: 45);

assert!(policy.success_rate() > 0.0);
assert!(!policy.should_retry(3)); // exhausted
```

### Circuit Breaker

```rust
let mut cb = CircuitBreaker::new(failure_threshold: 3, recovery_timeout_us: 5_000);

for _ in 0..3 { cb.record_outcome(Outcome::Retryable, now_us: 0); }
assert!(!cb.allow(100)); // Open — blocked

assert!(cb.allow(5_100)); // HalfOpen — probe allowed
cb.record_outcome(Outcome::Success, 5_200); // → Closed
```

## API

### `RetryPolicy`

| Method | Returns | Description |
|--------|---------|-------------|
| `new(max_retries, base_delay_us, max_delay_us)` | `Self` | Configure backoff bounds |
| `record(outcome, latency_us)` | `()` | Log an attempt |
| `should_retry(attempt)` | `bool` | Whether attempt *n* is within bounds |
| `backoff_us(attempt)` | `u64` | Delay in microseconds for attempt *n* |
| `success_rate()` | `f64` | Fraction of attempts that succeeded |

### `CircuitBreaker`

| Method | Returns | Description |
|--------|---------|-------------|
| `new(failure_threshold, recovery_timeout_us)` | `Self` | Configure trip/recovery |
| `record_outcome(outcome, now_us)` | `()` | Feed result to state machine |
| `allow(now_us)` | `bool` | Whether a request should proceed |
| `state()` | `CircuitState` | Current: Closed / HalfOpen / Open |
| `total_opens()` | `u64` | Lifetime count of trips |

## Architecture Notes

This crate encodes the **γ + η = C** invariant from the SuperInstance ecosystem: the ternary outcome `{+1, 0, -1}` is the *generation* signal (γ), the circuit-breaker state `{+1, 0, -1}` is the *entropy* signal (η), and together they determine the system's *conservation* behavior (C) — whether to proceed, probe, or halt. The circuit breaker is the conservation enforcement layer over the retry policy's generative attempts.

## References

- **Exponential backoff with jitter:** AWS Architecture Blog, "Exponential Backoff and Jitter" (2015)
- **Circuit breaker pattern:** Michael Nygard, *Release It!* (2007), Chapter 5
- **Ternary logic in fault tolerance:** Kleene, *Introduction to Metamathematics* (1952), §64

## License

MIT
