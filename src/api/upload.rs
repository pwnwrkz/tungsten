use anyhow::{Context, Result, bail};
use reqwest::{Client, multipart};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::api::roblox::*;
use crate::log;

pub struct RobloxClient {
    client: Client,
    api_key: String,
    /// The earliest point in time we're allowed to send another request.
    rate_limit_reset: Mutex<Option<Instant>>,
    /// Set to `true` after any non-retryable error so in-flight tasks can
    /// bail out immediately instead of hammering the API further.
    fatally_failed: AtomicBool,
}

impl RobloxClient {
    pub fn new(api_key: String) -> Self {
        // Build a shared client with a connection pool so every upload reuses
        // the same TCP connections instead of opening fresh ones each time.
        let client = Client::builder()
            .pool_max_idle_per_host(8)
            .build()
            .expect("failed to build reqwest client");

        RobloxClient {
            client,
            api_key,
            rate_limit_reset: Mutex::new(None),
            fatally_failed: AtomicBool::new(false),
        }
    }

    pub async fn upload(&self, name: &str, data: Vec<u8>, creator: Creator) -> Result<u64> {
        let request_json = serde_json::to_string(&UploadRequest {
            asset_type: "Decal".to_string(),
            display_name: name.to_string(),
            description: "Uploaded by Tungsten".to_string(),
            creation_context: CreationContext { creator },
        })
        .context("Failed to serialize upload request")?;

        // `data` and `name` are moved into the closure via Arc/clone only when
        // the closure is actually invoked (≤ MAX_RETRIES times), keeping the
        // hot path allocation-free on the first attempt.
        let response = self
            .send_with_retry(|client| {
                let form = multipart::Form::new()
                    .text("request", request_json.clone())
                    .part(
                        "fileContent",
                        multipart::Part::bytes(data.clone())
                            .file_name(name.to_owned())
                            .mime_str("image/png")
                            .unwrap(),
                    );

                client
                    .post("https://apis.roblox.com/assets/v1/assets")
                    .header("x-api-key", &self.api_key)
                    .multipart(form)
            })
            .await?;

        let operation: Operation = response
            .json()
            .await
            .context("Failed to parse upload response")?;

        self.poll_operation(&operation.operation_id).await
    }

    async fn poll_operation(&self, operation_id: &str) -> Result<u64> {
        const MAX_POLLS: u32 = 10;
        let mut delay = Duration::from_secs(1);

        for _ in 0..MAX_POLLS {
            tokio::time::sleep(delay).await;

            let response = self
                .send_with_retry(|client| {
                    client
                        .get(format!(
                            "https://apis.roblox.com/assets/v1/operations/{}",
                            operation_id
                        ))
                        .header("x-api-key", &self.api_key)
                })
                .await?;

            let operation: Operation = response
                .json()
                .await
                .context("Failed to parse operation response")?;

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

            delay *= 2;
        }

        bail!(
            "Upload timed out after {} attempts\n  \
             Hint: The asset may still be processing, check your Roblox inventory",
            MAX_POLLS
        )
    }

    async fn send_with_retry<F>(&self, make_req: F) -> Result<reqwest::Response>
    where
        F: Fn(&Client) -> reqwest::RequestBuilder,
    {
        if self.fatally_failed.load(Ordering::Acquire) {
            bail!("A previous request failed fatally, aborting");
        }

        const MAX_RETRIES: u8 = 5;
        let mut attempt: u8 = 0;

        loop {
            // Respect any active rate-limit window before firing the request.
            {
                let reset = self.rate_limit_reset.lock().await;
                if let Some(reset_at) = *reset {
                    let now = Instant::now();
                    if reset_at > now {
                        let wait = reset_at - now;
                        drop(reset);
                        tokio::time::sleep(wait).await;
                    }
                }
            }

            let response = make_req(&self.client)
                .send()
                .await
                .context("Failed to send request")?;

            match response.status() {
                reqwest::StatusCode::OK => return Ok(response),

                reqwest::StatusCode::TOO_MANY_REQUESTS if attempt < MAX_RETRIES => {
                    // Prefer the server-supplied reset header; fall back to
                    // exponential back-off if it's missing or unparseable.
                    let wait = response
                        .headers()
                        .get("x-ratelimit-reset")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(Duration::from_secs)
                        .unwrap_or_else(|| Duration::from_secs(1u64 << attempt));

                    log!(warn, "Rate limited, retrying in {:.2}s", wait.as_secs_f64());

                    *self.rate_limit_reset.lock().await = Some(Instant::now() + wait);
                    tokio::time::sleep(wait).await;
                    attempt += 1;
                }

                status => {
                    let body = response.text().await.unwrap_or_default();
                    self.fatally_failed.store(true, Ordering::Release);
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
}
