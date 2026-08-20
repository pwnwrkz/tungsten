use anyhow::{Context, Result, bail};
use bytes::Bytes;
use reqwest::{Client, StatusCode, multipart};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

use crate::api::sync::roblox::*;
use crate::core::assets::asset::AssetKind;
use crate::log;

pub struct RobloxClient {
    client: Client,
    /// Earliest unix-millis timestamp at which we're allowed to send again.
    /// Lock-free: compare-and-store instead of holding a mutex per request.
    rate_limit_reset: AtomicU64,
    /// Set to `true` after any non-retryable error so in-flight tasks can
    /// bail out immediately instead of hammering the API further.
    fatally_failed: AtomicBool,
    /// Wakes in-flight sleeps (poll backoff, rate-limit waits) when a fatal
    /// error occurs so they abort instead of sleeping pointlessly.
    cancel: Notify,
}

/// Everything the uploader needs to know about a single asset.
pub struct UploadParams {
    /// File name used in the multipart form.
    pub file_name: String,
    /// Display name sent in the JSON body.
    pub display_name: String,
    /// Description sent in the JSON body.
    pub description: String,
    /// Raw bytes of the (possibly converted) asset. `Bytes` so retries clone
    /// the payload in O(1) — a reference-count bump — instead of copying the
    /// whole file on every attempt.
    pub data: Bytes,
    /// Asset kind — determines API type string and MIME type.
    pub kind: AssetKind,
    /// Creator to upload under.
    pub creator: Creator,
    /// Optional override for the asset type string sent to the API.
    /// If set, this overrides the value derived from `kind`.
    pub asset_type_override: Option<String>,
}

impl RobloxClient {
    pub fn new(api_key: String) -> Self {
        let mut default_headers = reqwest::header::HeaderMap::new();
        default_headers.insert(
            "x-api-key",
            reqwest::header::HeaderValue::from_str(&api_key).expect("API key must be a valid header value"),
        );

        let client = Client::builder()
            // Keep at least as many idle connections as the upload concurrency
            // limit (default 10) to avoid connection thrashing.
            .pool_max_idle_per_host(10)
            // Set the API key once instead of per-request.
            .default_headers(default_headers)
            .build()
            .expect("failed to build reqwest client");

        RobloxClient {
            client,
            rate_limit_reset: AtomicU64::new(0),
            fatally_failed: AtomicBool::new(false),
            cancel: Notify::new(),
        }
    }

    /// Upload an asset and return its Roblox asset ID.
    pub async fn upload(&self, params: UploadParams) -> Result<u64> {
        let request_json = serde_json::to_string(&UploadRequest {
            asset_type: params
                .asset_type_override
                .clone()
                .unwrap_or_else(|| params.kind.api_type().to_string()),
            display_name: params.display_name.clone(),
            description: params.description.clone(),
            creation_context: CreationContext {
                creator: params.creator,
            },
        })
        .context("Failed to serialize upload request")?;

        let mime = params.kind.mime();

        log!(
            debug,
            "Uploading {} ({} bytes, {})",
            params.display_name,
            params.data.len(),
            mime
        );

        let response = self
            .send_with_retry(|client| {
                let form = multipart::Form::new()
                    .text("request", request_json.clone())
                    .part(
                        "fileContent",
                        // `Bytes` streams straight into the body without a copy.
                        multipart::Part::stream(params.data.clone())
                            .file_name(params.file_name.clone())
                            .mime_str(mime)
                            .unwrap(),
                    );

                client
                    .post("https://apis.roblox.com/assets/v1/assets")
                    .multipart(form)
            })
            .await?;

        let operation: Operation = parse_json_response(response, "upload response").await?;

        log!(
            debug,
            "Upload accepted for {}, operation {}",
            params.display_name,
            operation.operation_id
        );

        let asset_id = self.poll_operation(&operation.operation_id).await?;
        log!(
            debug,
            "Uploaded {} -> asset {}",
            params.display_name,
            asset_id
        );
        Ok(asset_id)
    }

    async fn poll_operation(&self, operation_id: &str) -> Result<u64> {
        const MAX_POLLS: u32 = 10;
        let mut delay = Duration::from_secs(1);

        for attempt in 0..MAX_POLLS {
            if self.fatally_failed.load(Ordering::Acquire) {
                bail!("A previous request failed fatally, aborting");
            }

            log!(
                debug,
                "Polling operation {} (attempt {}/{})",
                operation_id,
                attempt + 1,
                MAX_POLLS
            );

            let response = self
                .send_with_retry(|client| {
                    client.get(format!(
                        "https://apis.roblox.com/assets/v1/operations/{}",
                        operation_id
                    ))
                })
                .await?;

            let operation: Operation = parse_json_response(response, "operation response").await?;

            if operation.done {
                return match operation.response {
                    Some(result) => Ok(result
                        .asset_id
                        .parse()
                        .context("Failed to parse asset ID")?),
                    None => bail!(
                        "Operation completed but no asset ID was returned\n  \
                         Hint: This is likely a Roblox API issue, try again"
                    ),
                };
            }

            self.sleep_or_cancel(delay).await;
            delay = (delay * 2).min(Duration::from_secs(30));
        }

        bail!(
            "Upload timed out after {} poll attempts\n  \
             Hint: The asset may still be processing, check your Roblox inventory",
            MAX_POLLS
        )
    }

    async fn send_with_retry<F>(&self, make_req: F) -> Result<reqwest::Response>
    where
        F: Fn(&Client) -> reqwest::RequestBuilder,
    {
        const MAX_RETRIES: u8 = 5;
        let mut attempt: u8 = 0;

        loop {
            if self.fatally_failed.load(Ordering::Acquire) {
                bail!("A previous request failed fatally, aborting");
            }

            // Respect any active rate-limit window (lock-free).
            self.wait_for_rate_limit().await;

            let response = match make_req(&self.client).send().await {
                Ok(r) => r,
                Err(e) => {
                    // Transient network errors (timeouts, connection resets)
                    // are retried with backoff; anything else is fatal.
                    if attempt >= MAX_RETRIES || (!e.is_timeout() && !e.is_connect()) {
                        return Err(e).context("Failed to send request");
                    }
                    let wait = backoff(attempt);
                    log!(
                        warn,
                        "Network error ({}), retrying in {:.2}s",
                        e,
                        wait.as_secs_f64()
                    );
                    self.sleep_or_cancel(wait).await;
                    attempt += 1;
                    continue;
                }
            };

            log!(debug, "API request returned {}", response.status());

            match response.status() {
                StatusCode::OK => return Ok(response),

                StatusCode::TOO_MANY_REQUESTS if attempt < MAX_RETRIES => {
                    let wait = rate_limit_wait(&response, attempt);
                    log!(warn, "Rate limited, retrying in {:.2}s", wait.as_secs_f64());
                    self.rate_limit_reset.store(
                        unix_millis() + wait.as_millis() as u64,
                        Ordering::Release,
                    );
                    self.sleep_or_cancel(wait).await;
                    attempt += 1;
                }

                // 5xx server errors are transient — retry with backoff.
                s if s.is_server_error() && attempt < MAX_RETRIES => {
                    let wait = backoff(attempt);
                    log!(warn, "Server error {}, retrying in {:.2}s", s, wait.as_secs_f64());
                    self.sleep_or_cancel(wait).await;
                    attempt += 1;
                }

                // Anything else (4xx client errors, or retries exhausted) is
                // fatal — retrying a 401/403/404 will never succeed.
                status => {
                    let body = response.text().await.unwrap_or_default();
                    self.fatally_failed.store(true, Ordering::Release);
                    self.cancel.notify_waiters();
                    bail!(
                        "Request failed with status {}\n  Response: {}\n  \
                         Hint: Check your API key and creator ID",
                        status,
                        body
                    );
                }
            }
        }
    }

    /// Sleep until any active rate-limit window has passed.
    async fn wait_for_rate_limit(&self) {
        let now = unix_millis();
        let reset = self.rate_limit_reset.load(Ordering::Acquire);
        if reset > now {
            self.sleep_or_cancel(Duration::from_millis(reset - now)).await;
        }
    }

    /// Sleep, but abort early when a fatal failure cancels us. The caller
    /// re-checks `fatally_failed` at the top of its loop afterwards.
    async fn sleep_or_cancel(&self, duration: Duration) {
        tokio::select! {
            _ = tokio::time::sleep(duration) => {}
            _ = self.cancel.notified() => {}
        }
    }
}

/// Exponential backoff for a retry attempt: 1s, 2s, 4s, 8s, 16s.
fn backoff(attempt: u8) -> Duration {
    Duration::from_secs(1u64 << attempt)
}

/// Wait duration for a 429, honoring the `x-ratelimit-reset` header when
/// present, falling back to exponential backoff.
fn rate_limit_wait(response: &reqwest::Response, attempt: u8) -> Duration {
    let base = response
        .headers()
        .get("x-ratelimit-reset")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| backoff(attempt));
    jitter(base)
}

/// Apply ±10% random jitter so concurrent tasks don't all retry at the exact
/// same millisecond (thundering herd). Seeded from the clock — good enough to
/// stagger retries.
fn jitter(d: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.subsec_nanos())
        .unwrap_or(0);
    let spread = nanos % 200; // 0..200
    let factor = 0.9 + (spread as f64) / 1000.0; // 0.9..1.1
    d.mul_f64(factor)
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.as_millis() as u64)
        .unwrap_or(0)
}

/// Parse the response body as JSON, giving a clear error if Roblox returned a
/// non-JSON body (e.g. an HTML error page from Cloudflare) instead of failing
/// cryptically inside `serde_json`.
async fn parse_json_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    what: &str,
) -> Result<T> {
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    if !content_type.contains("json") {
        let body = response.text().await.unwrap_or_default();
        bail!(
            "Expected JSON from Roblox for {what} but got content-type \"{}\" (status {})\n  Response: {}",
            content_type,
            status,
            body.chars().take(500).collect::<String>()
        );
    }

    response
        .json()
        .await
        .with_context(|| format!("Failed to parse {what}"))
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use http::Response as HttpResponse;

    #[test]
    fn backoff_doubles_per_attempt() {
        assert_eq!(backoff(0), Duration::from_secs(1));
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(4), Duration::from_secs(16));
    }

    #[test]
    fn jitter_stays_within_ten_percent() {
        for _ in 0..200 {
            let d = Duration::from_secs(10);
            let j = jitter(d);
            let ms = j.as_millis();
            assert!(
                (9000..=11000).contains(&ms),
                "jittered wait {}ms out of ±10% band for 10s",
                ms
            );
        }
    }

    #[test]
    fn rate_limit_wait_falls_back_to_backoff_without_header() {
        // A bare 429 response with no reset header falls back to backoff
        // (2^2 = 4s), jittered ±10% => 3600..4400ms.
        let response = reqwest::Response::from(HttpResponse::builder().status(429).body("").unwrap());
        let wait = rate_limit_wait(&response, 2);
        let ms = wait.as_millis();
        assert!((3600..=4400).contains(&ms), "got {}ms", ms);
    }

    #[test]
    fn rate_limit_wait_honors_reset_header() {
        // When the reset header is present it wins over backoff.
        let response = reqwest::Response::from(
            HttpResponse::builder()
                .status(429)
                .header("x-ratelimit-reset", "120")
                .body("")
                .unwrap(),
        );
        let wait = rate_limit_wait(&response, 0);
        // ±10% of 120s => 108..132s.
        let secs = wait.as_secs();
        assert!((108..=132).contains(&secs), "got {}s", secs);
    }
}
