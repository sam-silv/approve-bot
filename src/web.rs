use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::json;
use tracing::{debug, warn};

use crate::{
    service::{ApprovalService, IncomingMessage},
    slack::{EventEnvelope, SignatureVerifier, SlackEvent},
};

const EVENT_TTL: Duration = Duration::from_secs(60 * 10);

#[derive(Clone)]
pub struct WebState {
    verifier: SignatureVerifier,
    service: Arc<ApprovalService>,
    replay_guard: Arc<ReplayGuard>,
    allow_channel_messages: bool,
}

impl WebState {
    #[must_use]
    pub fn new(
        verifier: SignatureVerifier,
        service: Arc<ApprovalService>,
        allow_channel_messages: bool,
    ) -> Self {
        Self {
            verifier,
            service,
            replay_guard: Arc::new(ReplayGuard::default()),
            allow_channel_messages,
        }
    }
}

#[derive(Default)]
struct ReplayGuard {
    events: Mutex<HashMap<String, Instant>>,
}

impl ReplayGuard {
    fn accept_at(&self, event_id: &str, now: Instant) -> bool {
        let mut events = self.events.lock().expect("replay guard lock poisoned");
        events.retain(|_, seen_at| now.saturating_duration_since(*seen_at) < EVENT_TTL);
        if events.contains_key(event_id) {
            return false;
        }
        events.insert(event_id.to_owned(), now);
        true
    }
}

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/slack/events", post(slack_events))
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "approve-bot",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn slack_events(State(state): State<WebState>, headers: HeaderMap, body: Bytes) -> Response {
    let timestamp = headers
        .get("x-slack-request-timestamp")
        .and_then(|value| value.to_str().ok());
    let signature = headers
        .get("x-slack-signature")
        .and_then(|value| value.to_str().ok());
    if let Err(error) = state.verifier.verify(timestamp, signature, &body) {
        warn!(%error, "rejected unsigned Slack request");
        return (StatusCode::UNAUTHORIZED, "invalid Slack signature").into_response();
    }

    let envelope = match serde_json::from_slice::<EventEnvelope>(&body) {
        Ok(envelope) => envelope,
        Err(error) => {
            warn!(%error, "invalid Slack event payload");
            return (StatusCode::BAD_REQUEST, "invalid Slack event payload").into_response();
        }
    };

    if envelope.kind == "url_verification" {
        return match envelope.challenge {
            Some(challenge) => Json(json!({ "challenge": challenge })).into_response(),
            None => (StatusCode::BAD_REQUEST, "missing challenge").into_response(),
        };
    }

    if envelope.kind != "event_callback" {
        return StatusCode::OK.into_response();
    }
    let Some(event_id) = envelope.event_id else {
        return (StatusCode::BAD_REQUEST, "missing event_id").into_response();
    };
    if !state.replay_guard.accept_at(&event_id, Instant::now()) {
        debug!(%event_id, "ignored duplicate Slack delivery");
        return StatusCode::OK.into_response();
    }
    let Some(message) = envelope
        .event
        .and_then(|event| event_to_message(event, state.allow_channel_messages))
    else {
        return StatusCode::OK.into_response();
    };

    let service = state.service.clone();
    tokio::spawn(async move {
        service.process(message).await;
    });
    StatusCode::OK.into_response()
}

fn event_to_message(event: SlackEvent, allow_channel_messages: bool) -> Option<IncomingMessage> {
    if event.bot_id.is_some() || event.subtype.is_some() {
        return None;
    }
    let is_direct_message = event.kind == "message" && event.channel_type.as_deref() == Some("im");
    let is_allowed_channel_event = allow_channel_messages
        && (event.kind == "app_mention"
            || (event.kind == "message" && event.channel_type.as_deref() != Some("im")));
    if !is_direct_message && !is_allowed_channel_event {
        return None;
    }

    Some(IncomingMessage {
        user: event.user?,
        channel: event.channel?,
        text: event.text?,
        message_ts: event.ts?,
        thread_ts: event.thread_ts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, channel_type: Option<&str>) -> SlackEvent {
        SlackEvent {
            kind: kind.into(),
            user: Some("U1".into()),
            text: Some("https://github.com/a/b/pull/1".into()),
            channel: Some("D1".into()),
            channel_type: channel_type.map(str::to_owned),
            ts: Some("123.4".into()),
            thread_ts: None,
            bot_id: None,
            subtype: None,
        }
    }

    #[test]
    fn accepts_direct_messages_by_default() {
        assert!(event_to_message(event("message", Some("im")), false).is_some());
    }

    #[test]
    fn ignores_channel_messages_by_default() {
        assert!(event_to_message(event("message", Some("channel")), false).is_none());
        assert!(event_to_message(event("app_mention", Some("channel")), false).is_none());
    }

    #[test]
    fn accepts_mentions_when_channel_messages_are_enabled() {
        assert!(event_to_message(event("app_mention", Some("channel")), true).is_some());
    }

    #[test]
    fn ignores_bot_messages_and_subtypes() {
        let mut bot_event = event("message", Some("im"));
        bot_event.bot_id = Some("B1".into());
        assert!(event_to_message(bot_event, false).is_none());

        let mut edited = event("message", Some("im"));
        edited.subtype = Some("message_changed".into());
        assert!(event_to_message(edited, false).is_none());
    }

    #[test]
    fn preserves_existing_thread_timestamp() {
        let mut threaded = event("message", Some("im"));
        threaded.thread_ts = Some("100.0".into());
        assert_eq!(
            event_to_message(threaded, false)
                .unwrap()
                .thread_ts
                .as_deref(),
            Some("100.0")
        );
    }

    #[test]
    fn replay_guard_rejects_duplicates_until_ttl_expires() {
        let guard = ReplayGuard::default();
        let start = Instant::now();
        assert!(guard.accept_at("Ev1", start));
        assert!(!guard.accept_at("Ev1", start + Duration::from_secs(60)));
        assert!(guard.accept_at("Ev1", start + EVENT_TTL));
    }
}
