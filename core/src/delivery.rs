//! Dispatch lease deadlines and retry backoff policy.

use crate::config::DeliveryConfig;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
/// In-memory timing policy derived from validated delivery configuration.
pub(crate) struct DeliveryPolicy {
    /// Duration for which a sent command waits for acknowledgement.
    acknowledgement_timeout: Duration,
    /// Initial delay applied after a transient delivery failure.
    retry_initial: Duration,
    /// Upper bound for exponential transient retry delays.
    retry_max: Duration,
}

impl DeliveryPolicy {
    /// Converts validated configuration millisecond values into duration policy.
    pub(crate) fn from_config(config: &DeliveryConfig) -> Self {
        Self {
            acknowledgement_timeout: Duration::from_millis(config.acknowledgement_timeout_ms),
            retry_initial: Duration::from_millis(config.retry_initial_ms),
            retry_max: Duration::from_millis(config.retry_max_ms),
        }
    }

    /// Returns the lease duration for a sent but unacknowledged command.
    pub(crate) fn acknowledgement_timeout(&self) -> Duration {
        self.acknowledgement_timeout
    }

    /// Computes bounded exponential backoff for a persisted attempt count.
    pub(crate) fn retry_delay(&self, attempts: i64) -> Duration {
        let exponent = u32::try_from(attempts.saturating_sub(1))
            .unwrap_or(u32::MAX)
            .min(63);
        let multiplier = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
        Duration::from_millis(
            millis(self.retry_initial)
                .saturating_mul(multiplier)
                .min(millis(self.retry_max)),
        )
    }

    /// Returns a persisted Unix-millisecond deadline after `duration`.
    pub(crate) fn deadline_after(&self, duration: Duration) -> i64 {
        now_unix_ms().saturating_add(i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
    }
}

/// Returns current time as a saturating Unix-millisecond timestamp.
pub(crate) fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

/// Converts a duration to milliseconds without overflowing `u64`.
fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
/// Unit tests for deterministic delivery timing calculations.
mod tests {
    use super::*;

    /// Builds a short bounded policy appropriate for arithmetic assertions.
    fn policy() -> DeliveryPolicy {
        DeliveryPolicy::from_config(&DeliveryConfig {
            acknowledgement_timeout_ms: 50,
            retry_initial_ms: 10,
            retry_max_ms: 25,
        })
    }

    #[test]
    /// Applies exponential retry growth while enforcing the configured maximum.
    fn retry_delay_is_bounded_exponential_backoff() {
        assert_eq!(policy().retry_delay(1), Duration::from_millis(10));
        assert_eq!(policy().retry_delay(2), Duration::from_millis(20));
        assert_eq!(policy().retry_delay(3), Duration::from_millis(25));
        assert_eq!(policy().retry_delay(10), Duration::from_millis(25));
    }
}
