use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

const MAX_SIGNATURE_AGE_SECONDS: i64 = 60 * 5;

#[derive(Clone)]
pub struct SignatureVerifier {
    signing_secret: String,
}

#[derive(Debug, Error, PartialEq)]
pub enum SignatureError {
    #[error("missing Slack signature header")]
    MissingSignature,
    #[error("missing or invalid Slack timestamp header")]
    InvalidTimestamp,
    #[error("Slack request timestamp is outside the five-minute window")]
    Stale,
    #[error("Slack request signature is invalid")]
    InvalidSignature,
}

impl SignatureVerifier {
    pub fn new(signing_secret: impl Into<String>) -> Self {
        Self {
            signing_secret: signing_secret.into(),
        }
    }

    /// Verifies Slack's timestamped HMAC signature over the exact request body.
    ///
    /// # Errors
    ///
    /// Returns [`SignatureError`] when headers are missing, the request is stale, or the
    /// signature does not match.
    pub fn verify(
        &self,
        timestamp: Option<&str>,
        signature: Option<&str>,
        body: &[u8],
    ) -> Result<(), SignatureError> {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| SignatureError::InvalidTimestamp)?
            .as_secs();
        let now = i64::try_from(seconds).map_err(|_| SignatureError::InvalidTimestamp)?;
        self.verify_at(timestamp, signature, body, now)
    }

    fn verify_at(
        &self,
        timestamp: Option<&str>,
        signature: Option<&str>,
        body: &[u8],
        now: i64,
    ) -> Result<(), SignatureError> {
        let timestamp_text = timestamp.ok_or(SignatureError::InvalidTimestamp)?;
        let timestamp = timestamp_text
            .parse::<i64>()
            .map_err(|_| SignatureError::InvalidTimestamp)?;
        if now.abs_diff(timestamp) > MAX_SIGNATURE_AGE_SECONDS as u64 {
            return Err(SignatureError::Stale);
        }
        let signature = signature.ok_or(SignatureError::MissingSignature)?;
        let provided = signature
            .strip_prefix("v0=")
            .and_then(|value| hex::decode(value).ok())
            .ok_or(SignatureError::InvalidSignature)?;

        let mut mac = HmacSha256::new_from_slice(self.signing_secret.as_bytes())
            .expect("HMAC accepts keys of any length");
        mac.update(b"v0:");
        mac.update(timestamp_text.as_bytes());
        mac.update(b":");
        mac.update(body);
        mac.verify_slice(&provided)
            .map_err(|_| SignatureError::InvalidSignature)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct EventEnvelope {
    #[serde(rename = "type")]
    pub kind: String,
    pub challenge: Option<String>,
    pub event_id: Option<String>,
    pub event: Option<SlackEvent>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SlackEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub user: Option<String>,
    pub text: Option<String>,
    pub channel: Option<String>,
    pub channel_type: Option<String>,
    pub ts: Option<String>,
    pub thread_ts: Option<String>,
    pub bot_id: Option<String>,
    pub subtype: Option<String>,
}

#[derive(Debug, Error)]
pub enum SlackError {
    #[error("Slack request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("Slack API rejected the message: {0}")]
    Api(String),
}

#[async_trait]
pub trait SlackApi: Send + Sync {
    async fn post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<(), SlackError>;
}

#[derive(Clone)]
pub struct HttpSlack {
    client: Client,
    bot_token: String,
    api_url: String,
}

impl HttpSlack {
    pub fn new(bot_token: impl Into<String>) -> Self {
        Self::with_api_url(bot_token, "https://slack.com/api/chat.postMessage")
    }

    pub fn with_api_url(bot_token: impl Into<String>, api_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            bot_token: bot_token.into(),
            api_url: api_url.into(),
        }
    }
}

#[derive(Serialize)]
struct PostMessageRequest<'a> {
    channel: &'a str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_ts: Option<&'a str>,
    unfurl_links: bool,
}

#[derive(Deserialize)]
struct SlackResponse {
    ok: bool,
    error: Option<String>,
}

#[async_trait]
impl SlackApi for HttpSlack {
    async fn post_message(
        &self,
        channel: &str,
        thread_ts: Option<&str>,
        text: &str,
    ) -> Result<(), SlackError> {
        let response = self
            .client
            .post(&self.api_url)
            .bearer_auth(&self.bot_token)
            .json(&PostMessageRequest {
                channel,
                text,
                thread_ts,
                unfurl_links: false,
            })
            .send()
            .await?
            .error_for_status()?
            .json::<SlackResponse>()
            .await?;
        if response.ok {
            Ok(())
        } else {
            Err(SlackError::Api(
                response.error.unwrap_or_else(|| "unknown_error".into()),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers};

    use super::*;

    fn signature(secret: &str, timestamp: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("v0:{timestamp}:").as_bytes());
        mac.update(body);
        format!("v0={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn verifies_a_valid_signature() {
        let body = br#"{"type":"event_callback"}"#;
        let sig = signature("secret", "1000", body);
        assert_eq!(
            SignatureVerifier::new("secret").verify_at(Some("1000"), Some(&sig), body, 1000),
            Ok(())
        );
    }

    #[test]
    fn rejects_tampered_bodies() {
        let sig = signature("secret", "1000", b"original");
        assert_eq!(
            SignatureVerifier::new("secret").verify_at(Some("1000"), Some(&sig), b"tampered", 1000),
            Err(SignatureError::InvalidSignature)
        );
    }

    #[test]
    fn rejects_old_and_future_requests() {
        let sig = signature("secret", "1000", b"body");
        let verifier = SignatureVerifier::new("secret");
        assert_eq!(
            verifier.verify_at(Some("1000"), Some(&sig), b"body", 1301),
            Err(SignatureError::Stale)
        );
        assert_eq!(
            verifier.verify_at(Some("1000"), Some(&sig), b"body", 699),
            Err(SignatureError::Stale)
        );
    }

    #[test]
    fn reports_missing_and_malformed_headers() {
        let verifier = SignatureVerifier::new("secret");
        assert_eq!(
            verifier.verify_at(None, Some("v0=00"), b"", 0),
            Err(SignatureError::InvalidTimestamp)
        );
        assert_eq!(
            verifier.verify_at(Some("0"), None, b"", 0),
            Err(SignatureError::MissingSignature)
        );
        assert_eq!(
            verifier.verify_at(Some("nope"), Some("v0=00"), b"", 0),
            Err(SignatureError::InvalidTimestamp)
        );
    }

    #[tokio::test]
    async fn posts_a_threaded_slack_message() {
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::header("authorization", "Bearer xoxb-test"))
            .and(matchers::body_json(serde_json::json!({
                "channel": "C1",
                "text": "approved",
                "thread_ts": "123.4",
                "unfurl_links": false
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok":true})))
            .expect(1)
            .mount(&server)
            .await;
        let slack = HttpSlack::with_api_url("xoxb-test", server.uri());
        slack
            .post_message("C1", Some("123.4"), "approved")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn surfaces_slack_api_errors() {
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ok":false,"error":"not_in_channel"})),
            )
            .mount(&server)
            .await;
        let error = HttpSlack::with_api_url("token", server.uri())
            .post_message("C1", None, "hello")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not_in_channel"));
    }
}
