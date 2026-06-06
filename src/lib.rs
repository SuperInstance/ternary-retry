//! # ternary-retry
//!
//! Retry policy for GPU kernel execution with ternary outcome.

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome { Success = 1, Retryable = 0, PermanentFail = -1 }

#[derive(Debug, Clone)]
pub struct Attempt {
    pub outcome: Outcome,
    pub latency_us: u64,
}

pub struct RetryPolicy {
    max_retries: u32,
    base_delay_us: u64,
    max_delay_us: u64,
    attempts: Vec<Attempt>,
    successes: u64,
    retries: u64,
    permanent_fails: u64,
}

impl RetryPolicy {
    pub fn new(max_retries: u32, base_delay_us: u64, max_delay_us: u64) -> Self {
        Self { max_retries, base_delay_us, max_delay_us, attempts: Vec::new(), successes: 0, retries: 0, permanent_fails: 0 }
    }

    pub fn record(&mut self, outcome: Outcome, latency_us: u64) {
        match outcome {
            Outcome::Success => self.successes += 1,
            Outcome::Retryable => self.retries += 1,
            Outcome::PermanentFail => self.permanent_fails += 1,
        }
        self.attempts.push(Attempt { outcome, latency_us });
    }

    /// Should we retry given the current attempt count?
    pub fn should_retry(&self, attempt: u32) -> bool {
        attempt < self.max_retries
            && self.permanent_fails == 0 // don't retry after permanent failure
    }

    /// Exponential backoff with jitter.
    pub fn backoff_us(&self, attempt: u32) -> u64 {
        let exp = self.base_delay_us.checked_shl(attempt).unwrap_or(self.max_delay_us);
        let jitter = (exp / 10).max(1);
        let delay = exp + (jitter * ((attempt as u64 * 7 + 13) % 10)) / 10;
        delay.min(self.max_delay_us)
    }

    pub fn success_rate(&self) -> f64 {
        let total = self.successes + self.retries + self.permanent_fails;
        if total == 0 { return 1.0; }
        self.successes as f64 / total as f64
    }

    pub fn attempt_count(&self) -> usize { self.attempts.len() }
    pub fn successes(&self) -> u64 { self.successes }
    pub fn retries(&self) -> u64 { self.retries }
    pub fn permanent_fails(&self) -> u64 { self.permanent_fails }
}

pub struct CircuitBreaker {
    failure_threshold: u32,
    recovery_timeout_us: u64,
    consecutive_failures: u32,
    state: CircuitState,
    opened_at: Option<u64>,
    total_opens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState { Closed = 1, HalfOpen = 0, Open = -1 }

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, recovery_timeout_us: u64) -> Self {
        Self { failure_threshold, recovery_timeout_us, consecutive_failures: 0, state: CircuitState::Closed, opened_at: None, total_opens: 0 }
    }

    pub fn record_outcome(&mut self, outcome: Outcome, now_us: u64) {
        match outcome {
            Outcome::Success => {
                self.consecutive_failures = 0;
                self.state = CircuitState::Closed;
            }
            Outcome::Retryable => {
                self.consecutive_failures += 1;
                if self.consecutive_failures >= self.failure_threshold {
                    self.state = CircuitState::Open;
                    self.opened_at = Some(now_us);
                    self.total_opens += 1;
                }
            }
            Outcome::PermanentFail => {
                self.state = CircuitState::Open;
                self.opened_at = Some(now_us);
                self.total_opens += 1;
            }
        }
    }

    pub fn allow(&mut self, now_us: u64) -> bool {
        if self.state == CircuitState::Open {
            if let Some(opened) = self.opened_at {
                if now_us >= opened + self.recovery_timeout_us {
                    self.state = CircuitState::HalfOpen;
                    return true; // allow one probe
                }
            }
            return false;
        }
        true // Closed or HalfOpen
    }

    pub fn state(&self) -> CircuitState { self.state }
    pub fn total_opens(&self) -> u64 { self.total_opens }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_success() {
        let mut rp = RetryPolicy::new(3, 1000, 10000);
        rp.record(Outcome::Success, 50);
        assert_eq!(rp.successes(), 1);
        assert_eq!(rp.success_rate(), 1.0);
    }

    #[test]
    fn test_should_retry() {
        let rp = RetryPolicy::new(3, 1000, 10000);
        assert!(rp.should_retry(0));
        assert!(rp.should_retry(2));
        assert!(!rp.should_retry(3));
    }

    #[test]
    fn test_backoff_grows() {
        let rp = RetryPolicy::new(5, 1000, 100000);
        assert!(rp.backoff_us(2) > rp.backoff_us(1));
    }

    #[test]
    fn test_permanent_fail_stops() {
        let mut rp = RetryPolicy::new(3, 1000, 10000);
        rp.record(Outcome::PermanentFail, 50);
        assert!(!rp.should_retry(0));
    }

    #[test]
    fn test_circuit_closed() {
        let mut cb = CircuitBreaker::new(3, 5000);
        cb.record_outcome(Outcome::Success, 0);
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.allow(100));
    }

    #[test]
    fn test_circuit_opens() {
        let mut cb = CircuitBreaker::new(3, 5000);
        cb.record_outcome(Outcome::Retryable, 0);
        cb.record_outcome(Outcome::Retryable, 0);
        cb.record_outcome(Outcome::Retryable, 0);
        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.allow(100));
    }

    #[test]
    fn test_circuit_half_open() {
        let mut cb = CircuitBreaker::new(2, 1000);
        cb.record_outcome(Outcome::Retryable, 0);
        cb.record_outcome(Outcome::Retryable, 0);
        assert!(cb.allow(1001)); // past recovery timeout
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn test_circuit_recovery() {
        let mut cb = CircuitBreaker::new(2, 1000);
        cb.record_outcome(Outcome::Retryable, 0);
        cb.record_outcome(Outcome::Retryable, 0);
        cb.allow(1001); // half-open
        cb.record_outcome(Outcome::Success, 1100); // success closes
        assert_eq!(cb.state(), CircuitState::Closed);
    }
}
