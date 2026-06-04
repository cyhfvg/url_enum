use std::time::Duration;

use url::Url;

#[derive(Debug, Clone, Copy)]
pub(super) struct RequestPacing {
    jitter: Duration,
}

impl RequestPacing {
    /// Signature: `fn new(jitter_ms) -> Self`
    ///
    /// Purpose: Creates request pacing configuration from a jitter bound.
    ///
    /// Parameters:
    /// - `jitter_ms`: Maximum deterministic delay, in milliseconds.
    ///
    /// Returns: A [`RequestPacing`] value.
    ///
    /// Notes: A zero jitter value disables pacing delays.
    pub(super) fn new(jitter_ms: u64) -> Self {
        Self {
            jitter: Duration::from_millis(jitter_ms),
        }
    }

    /// Signature: `fn delay_for(&self, word, url, request_index) -> Duration`
    ///
    /// Purpose: Computes the deterministic per-request delay.
    ///
    /// Parameters:
    /// - `word`: Candidate dictionary word.
    /// - `url`: URL being requested.
    /// - `request_index`: Zero-based request index within the candidate's
    ///   redirect chain.
    ///
    /// Returns: A delay in the inclusive range `0..=jitter`.
    ///
    /// Notes: Deterministic hashing keeps retries reproducible while still
    /// spreading request timing.
    fn delay_for(&self, word: &str, url: &Url, request_index: usize) -> Duration {
        if self.jitter.is_zero() {
            return Duration::ZERO;
        }

        let max_ms = self.jitter.as_millis().min(u64::MAX as u128) as u64;
        let hash = jitter_hash(word, url.as_str(), request_index);
        let delay_ms = if max_ms == u64::MAX {
            hash
        } else {
            hash % (max_ms + 1)
        };
        Duration::from_millis(delay_ms)
    }

    /// Signature: `async fn wait_before_request(&self, word, url, request_index)`
    ///
    /// Purpose: Sleeps for the computed jitter delay before sending a request.
    ///
    /// Parameters:
    /// - `word`: Candidate dictionary word.
    /// - `url`: URL about to be requested.
    /// - `request_index`: Zero-based request index within the candidate's
    ///   redirect chain.
    ///
    /// Returns: Nothing.
    ///
    /// Notes: The function returns immediately when jitter is disabled or the
    /// computed delay is zero.
    pub(super) async fn wait_before_request(&self, word: &str, url: &Url, request_index: usize) {
        let delay = self.delay_for(word, url, request_index);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
    }
}

/// Signature: `fn jitter_hash(word, url, request_index) -> u64`
///
/// Purpose: Hashes stable request identity fields into a jitter seed.
///
/// Parameters:
/// - `word`: Candidate dictionary word.
/// - `url`: URL string for the request.
/// - `request_index`: Zero-based request index within the redirect chain.
///
/// Returns: A 64-bit FNV-1a-style hash.
///
/// Notes: This avoids storing RNG state in the asynchronous request path.
fn jitter_hash(word: &str, url: &str, request_index: usize) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in word
        .as_bytes()
        .iter()
        .chain(url.as_bytes())
        .copied()
        .chain((request_index as u64).to_le_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Signature: `fn request_jitter_is_deterministic_and_bounded()`
    ///
    /// Purpose: Verifies jitter is stable for the same request and never exceeds
    /// the configured bound.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: Different request indexes may produce different delays.
    #[test]
    fn request_jitter_is_deterministic_and_bounded() {
        let pacing = RequestPacing::new(250);
        let url = Url::parse("https://example.test/admin").expect("valid URL");

        let first = pacing.delay_for("admin", &url, 0);
        let second = pacing.delay_for("admin", &url, 0);
        let next_request = pacing.delay_for("admin", &url, 1);

        assert_eq!(first, second);
        assert!(first <= Duration::from_millis(250));
        assert!(next_request <= Duration::from_millis(250));
    }

    /// Signature: `fn zero_request_jitter_has_no_delay()`
    ///
    /// Purpose: Verifies disabled jitter produces no delay.
    ///
    /// Parameters: None.
    ///
    /// Returns: Nothing; assertions define success.
    ///
    /// Notes: This keeps the default scanner path free of pacing overhead.
    #[test]
    fn zero_request_jitter_has_no_delay() {
        let pacing = RequestPacing::new(0);
        let url = Url::parse("https://example.test/admin").expect("valid URL");

        assert_eq!(pacing.delay_for("admin", &url, 0), Duration::ZERO);
    }
}
