use std::sync::Arc;

use anyhow::Context;
use approve_bot::{
    config::Settings,
    github::HttpGitHub,
    service::{ApprovalService, RandomCommentPolicy, ServiceSettings},
    slack::{HttpSlack, SignatureVerifier},
    web::{WebState, router},
};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("approve_bot=info")),
        )
        .json()
        .init();

    let settings = Settings::from_env().context("invalid configuration")?;
    let github = Arc::new(
        HttpGitHub::new(&settings.github_token, settings.github_api_url.clone())
            .context("could not initialize GitHub client")?,
    );
    let slack = Arc::new(HttpSlack::new(&settings.slack_bot_token));
    let comments = Arc::new(RandomCommentPolicy::new(
        settings.approval_comments.clone(),
        settings.comment_probability,
    ));
    let service = Arc::new(ApprovalService::new(
        github,
        slack,
        comments,
        ServiceSettings {
            github_web_host: settings.github_web_host.clone(),
            allowed_slack_users: settings.allowed_slack_users.clone(),
            allow_all_users: settings.allow_all_users,
            approval_review_body: settings.approval_review_body.clone(),
            dry_run: settings.dry_run,
        },
    ));
    let app = router(WebState::new(
        SignatureVerifier::new(&settings.slack_signing_secret),
        service,
        settings.allow_channel_messages,
    ));
    let listener = TcpListener::bind(settings.listen_addr)
        .await
        .with_context(|| format!("could not bind to {}", settings.listen_addr))?;
    info!(address = %settings.listen_addr, dry_run = settings.dry_run, "approve-bot listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP server failed")
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
