# ternary-retry

Retry logic with ternary outcomes {+1=Success, 0=Retryable, −1=PermanentFail} and integrated circuit breaker for GPU kernel execution.

## Background

Distributed systems must handle partial failure. A GPU kernel may succeed, fail transiently (GPU busy, memory pressure, timeout), or fail permanently (invalid parameters, unsupported operation). The **retry pattern** handles transient failures by re-executing the operation with exponential backoff. The **circuit breaker pattern** (Nygard, 2007) prevents cascading failures by stopping retries when a service is demonstrably unhealthy.

The `ternary-retry` crate unifies both patterns with a ternary outcome space:
- **+1 (Success)**: The operation completed. No retry needed.
- **0 (Retryable)**: Transient failure. Retry with backoff.
- **−1 (PermanentFail)**: Irrecoverable failure. Stop immediately; do not retry.

This ternary classification is critical for GPU workloads. A kernel timeout (retryable) should be retried with backoff; an invalid CUDA kernel (permanent) should fail immediately to avoid wasting GPU cycles. Binary success/failure conflation wastes resources retrying operations that can never succeed, while lumping permanent failures with transient ones obscures the root cause.

The circuit breaker adds a system-level protection layer: after N consecutive failures, the circuit opens (−1 state), blocking all attempts until a recovery timeout expires. It then enters half-open (0) state, allowing a single probe attempt. Success closes the circuit (+1); failure reopens it. This ternary circuit state {Closed, HalfOpen, Open} mirrors the outcome ternary space.

## How It Works

### Architecture

Two independent components:

**`RetryPolicy`**: Tracks per-attempt outcomes and manages backoff.
- `max_retries`: Maximum retry attempts before giving up
- `base_delay_us`: Initial backoff delay in microseconds
- `max_delay_us`: Cap on exponential growth
- Counters: `successes`, `retries`, `permanent_fails`

**`CircuitBreaker`**: Protects against cascading failures.
- `failure_threshold`: Consecutive failures before opening
- `recovery_timeout_us`: Time before allowing a probe
- State machine: Closed → Open → HalfOpen → (Closed or Open)

### RetryPolicy Operations

- **record(outcome, latency)**: Log an attempt's outcome and latency. Update counters.
- **should_retry(attempt)**: Returns `true` if `attempt < max_retries` AND no permanent failures recorded. Once a PermanentFail is seen, all subsequent retry checks return `false`.
- **backoff_us(attempt)**: Exponential backoff with jitter:
  ```
  delay = base_delay << attempt + jitter
  jitter = (delay / 10) * ((attempt * 7 + 13) % 10) / 10
  result = min(delay, max_delay)
  ```
  The deterministic jitter (based on attempt number) provides variability without requiring an RNG.

### CircuitBreaker Operations

- **record_outcome(outcome, now_us)**:
  - Success → reset consecutive failures, close circuit
  - Retryable → increment failures; if ≥ threshold, open circuit
  - PermanentFail → immediately open circuit
- **allow(now_us)**:
  - Closed → allow (true)
  - Open → check if recovery timeout elapsed; if yes, transition to HalfOpen and allow
  - HalfOpen → allow (one probe attempt)

### State Machine

```
        Success
 Closed ◄─────── HalfOpen
   │                │
   │ failures       │ failure
   │ ≥ threshold    │
   ▼                ▼
  Open ───────────► Open
   (timeout elapsed)
```

## Experimental Results

All 8 unit tests pass:

| Test | Result | Observation |
|------|--------|-------------|
| `test_record_success` | ✅ | Single success: `successes() = 1`, `success_rate() = 1.0` |
| `test_should_retry` | ✅ | `should_retry(0)` = true, `should_retry(2)` = true, `should_retry(3)` = false (max_retries=3) |
| `test_backoff_grows` | ✅ | `backoff(2) > backoff(1)` — exponential growth confirmed |
| `test_permanent_fail_stops` | ✅ | After recording PermanentFail, `should_retry(0)` = false |
| `test_circuit_closed` | ✅ | After success, circuit stays Closed; `allow()` = true |
| `test_circuit_opens` | ✅ | 3 consecutive Retryable failures: circuit → Open; `allow()` = false |
| `test_circuit_half_open` | ✅ | After recovery timeout (1001 μs), `allow()` returns true; state = HalfOpen |
| `test_circuit_recovery` | ✅ | HalfOpen + success → Closed. Full recovery cycle works. |

The `test_permanent_fail_stops` result is key: a single PermanentFail (−1) outcome prevents any further retries, even at attempt 0. This prevents wasting resources on irrecoverable errors.

The `test_circuit_recovery` test demonstrates the full cycle: failures open the circuit → timeout allows a probe → success closes it. This is the classic circuit breaker pattern, now with ternary state semantics.

## Impact of Ternary {-1, 0, +1}

The ternary outcome space provides **actionable failure classification**:

- **+1 (Success)**: Operation succeeded. Proceed normally.
- **0 (Retryable)**: Transient failure. The system should try again, possibly after a delay. This is the "maybe" state—uncertain but hopeful.
- **−1 (PermanentFail)**: Irrecoverable failure. Immediate escalation required. No retry; alert or compensate.

Binary retry (success/fail) conflates 0 and −1, leading to either over-retrying permanent failures or under-retrying transient ones. The ternary distinction enables precise resource allocation: retry budget is spent only on outcomes that have a chance of succeeding.

## Use Cases

1. **GPU Kernel Execution**: Dispatch a CUDA kernel. If it returns success → done. If timeout (retryable) → retry with backoff. If launch error (permanent) → fail immediately and log the error. The ternary classification prevents wasting GPU time retrying invalid kernels.

2. **Microservice RPC Calls**: Wrap HTTP/gRPC calls with RetryPolicy + CircuitBreaker. 200 OK → +1. 503 Service Unavailable → 0 (retry). 400 Bad Request → −1 (permanent). The circuit breaker opens after consecutive 503s, protecting downstream services.

3. **Database Transaction Retry**: Optimistic concurrency control: successful commit → +1. Serialization failure → 0 (retry). Constraint violation → −1 (permanent, fix the data). Avoids retrying transactions that will always fail due to data integrity issues.

4. **File I/O with Network Storage**: Read from NFS/S3: success → +1. Network timeout → 0. File not found → −1. Prevents retrying reads on deleted files while gracefully handling transient network issues.

5. **Payment Processing**: Charge a credit card: approved → +1. Bank timeout → 0 (retry). Card declined → −1 (permanent). The ternary classification prevents recharging an already-declined card while retrying genuinely ambiguous bank timeouts.

## Open Questions

1. **Backoff Jitter Distribution**: The current deterministic jitter is predictable and may cause synchronized retries across multiple instances. Would random jitter (requiring an RNG) produce better thundering-herd avoidance?

2. **Adaptive Failure Thresholds**: The circuit breaker's `failure_threshold` is fixed. In practice, the optimal threshold depends on the error rate and latency distribution. Should the threshold adapt based on observed success rate?

3. **Multi-Level Circuit Breakers**: For nested service calls (A calls B calls C), should each level have its own circuit breaker? How should ternary outcomes propagate through the call stack—should a PermanentFail at C be reported as PermanentFail at A, or should it be reclassified?

## Connection to Oxide Stack

Within the five-layer Oxide ternary architecture:

- **Layer 1 (Ternary Genome)**: The outcome space {+1, 0, −1} maps directly to genome bases, encoding execution results as genetic information. Organisms that consistently produce −1 outcomes have lower fitness.
- **Layer 2 (Cellular Computation)**: Each retry attempt is a cell-level computation. The circuit breaker is a cell-level safety mechanism preventing a malfunctioning cell from draining resources.
- **Layer 3 (Organism Behavior)**: The retry policy governs an organism's behavioral response to environmental feedback. Success (+1) reinforces the behavior; retryable failure (0) triggers persistence; permanent failure (−1) triggers abandonment and strategy change.
- **Layer 4 (Population Dynamics)**: At the population level, aggregate retry rates measure environmental hostility. High retry rates indicate harsh environments; high permanent failure rates indicate fundamental strategy-environment mismatch.
- **Layer 5 (Ecosystem)**: The circuit breaker acts as an ecosystem-level protection mechanism, preventing individual failures from cascading through the entire system. When one component opens its circuit, dependent components experience reduced load, creating natural backpressure.
