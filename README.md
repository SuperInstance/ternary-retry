# ternary-retry

Retry logic for GPU kernel execution with ternary outcomes and circuit breaking.

## Why This Exists

GPU kernels don't just succeed or fail. There's a third outcome: **retryable failure** — the kernel hit a transient resource contention, a timeout, or a scheduling hiccup. Treating this like a permanent failure wastes work; treating it like success is wrong. Ternary retry captures `Success (+1)`, `Retryable (0)`, and `PermanentFail (-1)`, then applies exponential backoff only to the retryable cases.

This crate also includes a **circuit breaker** with ternary states: `Closed` (normal), `HalfOpen` (testing recovery), `Open` (blocked). This prevents cascading retries from hammering a failing GPU.

## Architecture

### Core Types

- **`Outcome`** — Ternary: `Success (+1)`, `Retryable (0)`, `PermanentFail (-1)`.
- **`RetryPolicy`** — Configurable max retries, base delay, max delay cap. Tracks all attempts.
- **`CircuitBreaker`** — Ternary-state circuit breaker with failure threshold and recovery timeout.
- **`CircuitState`** — `Closed (+1)`, `HalfOpen (0)`, `Open (-1)`.

### Retry Policy

Exponential backoff with jitter: `delay = min(base * 2^attempt, max_delay)`. Only `Retryable` outcomes trigger retries; `PermanentFail` stops immediately.

### Circuit Breaker

- **Closed**: All requests pass through. If failures exceed threshold, transition to Open.
- **Open**: All requests rejected. After recovery timeout, transition to HalfOpen.
- **HalfOpen**: Allow one request. If it succeeds, close the circuit. If it fails, reopen.

## Usage

```rust
use ternary_retry::{RetryPolicy, Outcome, CircuitBreaker, CircuitState};

let mut policy = RetryPolicy::new(3, 1000, 10_000); // max 3 retries, 1ms base, 10ms cap

policy.record(Outcome::Retryable, 500);  // transient failure, 500µs latency
policy.record(Outcome::Retryable, 1200); // retry with backoff
policy.record(Outcome::Success, 200);    // third time's the charm

assert_eq!(policy.should_retry(3), false); // max reached
let rate = policy.success_rate();

// Circuit breaker
let mut cb = CircuitBreaker::new(5, 1_000_000); // 5 failures → open, 1s recovery
for _ in 0..5 {
    cb.record_outcome(Outcome::PermanentFail, 0);
}
assert_eq!(cb.state(), CircuitState::Open);
assert_eq!(cb.allow(0), false); // blocked
```

## API Reference

### RetryPolicy

| Method | Returns | Description |
|--------|---------|-------------|
| `new(max_retries, base_delay_us, max_delay_us)` | `RetryPolicy` | Configure retry behavior |
| `record(outcome, latency_us)` | `()` | Record an attempt |
| `should_retry(attempt)` | `bool` | Should we retry at this attempt number |
| `backoff_us(attempt)` | `u64` | Backoff delay for given attempt |
| `success_rate()` | `f64` | Fraction of successes |
| `successes()` / `retries()` / `permanent_fails()` | `u64` | Outcome counts |

### CircuitBreaker

| Method | Returns | Description |
|--------|---------|-------------|
| `new(failure_threshold, recovery_timeout_us)` | `CircuitBreaker` | Configure breaker |
| `record_outcome(outcome, now_us)` | `()` | Feed an outcome |
| `allow(now_us)` | `bool` | Is a request allowed right now |
| `state()` | `CircuitState` | Current ternary state |
| `total_opens()` | `u64` | How many times the circuit opened |

## The Deeper Idea

The ternary outcome model is the **minimal failure taxonomy**. In distributed systems, you often see five or more error categories (timeout, rate-limited, permission denied, internal error, network error). But for retry decisions, you only need three buckets: "keep going" (+1), "try again" (0), "give up" (-1). All specific error types map into one of these three. This is the retry pattern reduced to its irreducible form.

## Related Crates

- **ternary-gc** — garbage collection with ternary marking
- **ternary-resilience** — resilience patterns with ternary network edges
- **ternary-backpressure** — backpressure with ternary pressure signals
