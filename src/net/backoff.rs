//! Exponential backoff with jitter (1 s → 60 s cap), architecture §3.

use std::time::Duration;

pub const BASE: Duration = Duration::from_secs(1);
pub const CAP: Duration = Duration::from_secs(60);

/// Delay before reconnect attempt `attempt` (1-based). `jitter` in `[0, 1)`
/// adds up to 25 % on top of the exponential delay, still capped.
pub fn delay(attempt: u32, jitter: f64) -> Duration {
    let exp = BASE.as_secs_f64() * 2f64.powi(attempt.saturating_sub(1).min(16) as i32);
    let with_jitter = exp * (1.0 + 0.25 * jitter.clamp(0.0, 1.0));
    Duration::from_secs_f64(with_jitter.min(CAP.as_secs_f64()))
}

/// Cheap jitter source without pulling in a RNG crate.
pub fn jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    f64::from(nanos % 1000) / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_exponentially_and_caps() {
        assert_eq!(delay(1, 0.0), Duration::from_secs(1));
        assert_eq!(delay(2, 0.0), Duration::from_secs(2));
        assert_eq!(delay(4, 0.0), Duration::from_secs(8));
        assert_eq!(delay(10, 0.0), CAP);
        assert_eq!(delay(1000, 0.99), CAP);
    }

    #[test]
    fn jitter_is_bounded() {
        let d = delay(3, 0.999);
        assert!(d >= Duration::from_secs(4) && d <= Duration::from_secs(5));
        assert!((0.0..1.0).contains(&jitter()));
    }
}
