//! The one choke point for Tuta traffic: bounded concurrency and throttle backoff.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Semaphore;
use tokio::time::{sleep_until, Instant};
use tutasdk::bindings::rest_client::{
    HttpMethod, RestClient, RestClientError, RestClientOptions, RestResponse,
};

const MAX_IN_FLIGHT: usize = 4;
const BACKOFF_FIRST: Duration = Duration::from_secs(2);
const BACKOFF_LAST: Duration = Duration::from_secs(60);
const SERVER_WAIT_MAX: Duration = Duration::from_secs(300);

/// A client that lets four requests through at once and pauses all after a 429 or 503.
///
/// It never retries a request, so a rejected login stays rejected.
pub struct Governor {
    inner: Arc<dyn RestClient>,
    permits: Semaphore,
    paused_until: Mutex<Option<Instant>>,
    throttles: AtomicU32,
}

impl Governor {
    /// Wraps a client.
    pub fn new(inner: Arc<dyn RestClient>) -> Self {
        Self {
            inner,
            permits: Semaphore::new(MAX_IN_FLIGHT),
            paused_until: Mutex::new(None),
            throttles: AtomicU32::new(0),
        }
    }

    async fn wait_out_pause(&self) {
        loop {
            let until = *self
                .paused_until
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match until {
                Some(until) if until > Instant::now() => sleep_until(until).await,
                _ => return,
            }
        }
    }

    fn pause_for(&self, wait: Duration) {
        let until = Instant::now() + wait;
        let mut paused = self
            .paused_until
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if paused.is_none_or(|current| current < until) {
            *paused = Some(until);
        }
    }

    fn wait_after_throttle(&self, response: &RestResponse) -> Duration {
        let advertised = ["retry-after", "suspension-time"]
            .iter()
            .find_map(|name| response.headers.get(*name))
            .and_then(|value| value.trim().parse::<u64>().ok());
        if let Some(seconds) = advertised {
            self.throttles.store(0, Ordering::Relaxed);
            return Duration::from_secs(seconds).clamp(BACKOFF_FIRST, SERVER_WAIT_MAX);
        }
        let previous = self.throttles.fetch_add(1, Ordering::Relaxed);
        BACKOFF_FIRST
            .saturating_mul(1 << previous.min(5))
            .min(BACKOFF_LAST)
    }
}

#[async_trait]
impl RestClient for Governor {
    async fn request_binary(
        &self,
        url: String,
        method: HttpMethod,
        options: RestClientOptions,
    ) -> Result<RestResponse, RestClientError> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| RestClientError::Unknown)?;
        self.wait_out_pause().await;
        let response = self.inner.request_binary(url, method, options).await?;
        match response.status {
            429 | 503 => {
                let wait = self.wait_after_throttle(&response);
                log::warn!("Tuta answered {}: pausing for {wait:?}", response.status);
                self.pause_for(wait);
            }
            200..300 => self.throttles.store(0, Ordering::Relaxed),
            _ => {}
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::AtomicUsize;

    use super::*;

    struct Fake {
        statuses: Mutex<Vec<u32>>,
        headers: HashMap<String, String>,
        calls: AtomicUsize,
        running: AtomicUsize,
        peak: AtomicUsize,
    }

    impl Fake {
        fn new(statuses: &[u32], headers: &[(&str, &str)]) -> Arc<Self> {
            Arc::new(Self {
                statuses: Mutex::new(statuses.iter().rev().copied().collect()),
                headers: headers
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect(),
                calls: AtomicUsize::new(0),
                running: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl RestClient for Fake {
        async fn request_binary(
            &self,
            _url: String,
            _method: HttpMethod,
            _options: RestClientOptions,
        ) -> Result<RestResponse, RestClientError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(running, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10)).await;
            self.running.fetch_sub(1, Ordering::SeqCst);
            let status = self.statuses.lock().unwrap().pop().unwrap_or(200);
            Ok(RestResponse {
                status,
                headers: self.headers.clone(),
                body: None,
            })
        }
    }

    async fn get(client: &Governor) -> RestResponse {
        let options = RestClientOptions {
            headers: HashMap::new(),
            body: None,
            suspension_behavior: None,
        };
        client
            .request_binary("http://x".to_owned(), HttpMethod::GET, options)
            .await
            .unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn no_more_than_four_requests_run_at_once() {
        let fake = Fake::new(&[], &[]);
        let governor = Arc::new(Governor::new(fake.clone()));
        let tasks: Vec<_> = (0..12)
            .map(|_| {
                let governor = governor.clone();
                tokio::spawn(async move { get(&governor).await })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(fake.calls.load(Ordering::SeqCst), 12);
        assert_eq!(fake.peak.load(Ordering::SeqCst), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn a_throttle_without_a_header_backs_off_from_two_seconds_and_doubles() {
        let fake = Fake::new(&[429, 429, 429, 200], &[]);
        let governor = Governor::new(fake);
        get(&governor).await;
        for expected in [2, 4, 8] {
            let before = Instant::now();
            get(&governor).await;
            let waited = Instant::now() - before;
            assert!(waited >= Duration::from_secs(expected));
            assert!(waited < Duration::from_secs(expected * 2));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_default_backoff_stops_at_sixty_seconds() {
        let governor = Governor::new(Fake::new(&[], &[]));
        let throttled = RestResponse {
            status: 429,
            headers: HashMap::new(),
            body: None,
        };
        let waits: Vec<_> = (0..8)
            .map(|_| governor.wait_after_throttle(&throttled).as_secs())
            .collect();
        assert_eq!(waits, [2, 4, 8, 16, 32, 60, 60, 60]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_servers_wait_is_clamped_between_two_and_three_hundred_seconds() {
        let governor = Governor::new(Fake::new(&[], &[]));
        let mut response = RestResponse {
            status: 503,
            headers: HashMap::new(),
            body: None,
        };
        for (header, value, expected) in [
            ("retry-after", "0", 2),
            ("retry-after", "30", 30),
            ("suspension-time", "9999", 300),
        ] {
            response.headers = HashMap::from([(header.to_owned(), value.to_owned())]);
            assert_eq!(governor.wait_after_throttle(&response).as_secs(), expected);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_rejected_request_is_never_retried() {
        for status in [401, 440, 470, 472] {
            let fake = Fake::new(&[status], &[]);
            let governor = Governor::new(fake.clone());
            assert_eq!(get(&governor).await.status, status);
            assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        }
    }
}
