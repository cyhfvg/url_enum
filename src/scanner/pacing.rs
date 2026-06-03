use std::time::Duration;

use url::Url;

#[derive(Debug, Clone, Copy)]
pub(super) struct RequestPacing {
    jitter: Duration,
}

impl RequestPacing {
    pub(super) fn new(jitter_ms: u64) -> Self {
        Self {
            jitter: Duration::from_millis(jitter_ms),
        }
    }

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

    pub(super) async fn wait_before_request(&self, word: &str, url: &Url, request_index: usize) {
        let delay = self.delay_for(word, url, request_index);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
    }
}

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

    #[test]
    fn zero_request_jitter_has_no_delay() {
        let pacing = RequestPacing::new(0);
        let url = Url::parse("https://example.test/admin").expect("valid URL");

        assert_eq!(pacing.delay_for("admin", &url, 0), Duration::ZERO);
    }
}
