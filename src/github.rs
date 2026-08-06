use async_trait::async_trait;
use reqwest::{Client, StatusCode, Url, header};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

use crate::domain::{GitHubUser, PullRequest, PullRequestRef};

const API_VERSION: &str = "2022-11-28";

#[derive(Debug, Error)]
pub enum GitHubError {
    #[error("GitHub request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("GitHub returned {status}: {message}")]
    Api { status: StatusCode, message: String },
    #[error("could not construct GitHub API URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
}

#[async_trait]
pub trait GitHubApi: Send + Sync {
    async fn current_user(&self) -> Result<GitHubUser, GitHubError>;
    async fn pull_request(&self, pull: &PullRequestRef) -> Result<PullRequest, GitHubError>;
    async fn approve(&self, pull: &PullRequestRef, body: Option<&str>) -> Result<(), GitHubError>;
    async fn comment(&self, pull: &PullRequestRef, body: &str) -> Result<(), GitHubError>;
}

#[derive(Clone)]
pub struct HttpGitHub {
    client: Client,
    api_url: Url,
}

impl HttpGitHub {
    /// Builds a GitHub REST client using a user access token.
    ///
    /// # Errors
    ///
    /// Returns [`GitHubError`] if the token cannot be encoded as an HTTP header or the client
    /// cannot be constructed.
    pub fn new(token: &str, mut api_url: Url) -> Result<Self, GitHubError> {
        if !api_url.path().ends_with('/') {
            api_url.set_path(&format!("{}/", api_url.path()));
        }
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
                GitHubError::Api {
                    status: StatusCode::BAD_REQUEST,
                    message: "GITHUB_TOKEN contains invalid header characters".into(),
                }
            })?,
        );
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            header::HeaderName::from_static("x-github-api-version"),
            header::HeaderValue::from_static(API_VERSION),
        );

        let client = Client::builder()
            .default_headers(headers)
            .user_agent(concat!("approve-bot/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { client, api_url })
    }

    fn endpoint(&self, path: &str) -> Result<Url, GitHubError> {
        Ok(self.api_url.join(path)?)
    }

    async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, GitHubError> {
        let response = ensure_success(response).await?;
        Ok(response.json().await?)
    }
}

#[derive(Serialize)]
struct ReviewRequest<'a> {
    event: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a str>,
}

#[derive(Serialize)]
struct CommentRequest<'a> {
    body: &'a str,
}

#[derive(Deserialize)]
struct ApiMessage {
    message: Option<String>,
}

#[async_trait]
impl GitHubApi for HttpGitHub {
    async fn current_user(&self) -> Result<GitHubUser, GitHubError> {
        let response = self.client.get(self.endpoint("user")?).send().await?;
        Self::decode(response).await
    }

    async fn pull_request(&self, pull: &PullRequestRef) -> Result<PullRequest, GitHubError> {
        let response = self
            .client
            .get(self.endpoint(&format!(
                "repos/{}/{}/pulls/{}",
                pull.owner, pull.repo, pull.number
            ))?)
            .send()
            .await?;
        Self::decode(response).await
    }

    async fn approve(&self, pull: &PullRequestRef, body: Option<&str>) -> Result<(), GitHubError> {
        let response = self
            .client
            .post(self.endpoint(&format!(
                "repos/{}/{}/pulls/{}/reviews",
                pull.owner, pull.repo, pull.number
            ))?)
            .json(&ReviewRequest {
                event: "APPROVE",
                body,
            })
            .send()
            .await?;
        ensure_success(response).await?;
        Ok(())
    }

    async fn comment(&self, pull: &PullRequestRef, body: &str) -> Result<(), GitHubError> {
        let response = self
            .client
            .post(self.endpoint(&format!(
                "repos/{}/{}/issues/{}/comments",
                pull.owner, pull.repo, pull.number
            ))?)
            .json(&CommentRequest { body })
            .send()
            .await?;
        ensure_success(response).await?;
        Ok(())
    }
}

async fn ensure_success(response: reqwest::Response) -> Result<reqwest::Response, GitHubError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<ApiMessage>(&body)
        .ok()
        .and_then(|payload| payload.message)
        .unwrap_or_else(|| body.chars().take(300).collect());
    Err(GitHubError::Api { status, message })
}

#[cfg(test)]
mod tests {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers};

    use super::*;

    fn pull_ref() -> PullRequestRef {
        PullRequestRef {
            owner: "acme".into(),
            repo: "rockets".into(),
            number: 42,
        }
    }

    fn client(server: &MockServer) -> HttpGitHub {
        HttpGitHub::new("secret-token", Url::parse(&server.uri()).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn fetches_the_authenticated_user_with_expected_headers() {
        let server = MockServer::start().await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/user"))
            .and(matchers::header("authorization", "Bearer secret-token"))
            .and(matchers::header("x-github-api-version", API_VERSION))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"login":"sam"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(client(&server).current_user().await.unwrap().login, "sam");
    }

    #[tokio::test]
    async fn fetches_pull_request_metadata() {
        let server = MockServer::start().await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/repos/acme/rockets/pulls/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 42,
                "state": "open",
                "draft": false,
                "html_url": "https://github.com/acme/rockets/pull/42",
                "title": "Add rockets",
                "user": {"login": "friend"}
            })))
            .mount(&server)
            .await;

        let pull = client(&server).pull_request(&pull_ref()).await.unwrap();
        assert_eq!(pull.title, "Add rockets");
        assert_eq!(pull.user.login, "friend");
    }

    #[tokio::test]
    async fn creates_an_approval_review() {
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/repos/acme/rockets/pulls/42/reviews"))
            .and(matchers::body_json(serde_json::json!({
                "event": "APPROVE",
                "body": "LGTM"
            })))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;

        client(&server)
            .approve(&pull_ref(), Some("LGTM"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn creates_an_issue_comment() {
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/repos/acme/rockets/issues/42/comments"))
            .and(matchers::body_json(
                serde_json::json!({"body":"nice commas"}),
            ))
            .respond_with(ResponseTemplate::new(201))
            .expect(1)
            .mount(&server)
            .await;

        client(&server)
            .comment(&pull_ref(), "nice commas")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn surfaces_github_api_messages() {
        let server = MockServer::start().await;
        Mock::given(matchers::path("/user"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({"message":"Bad credentials"})),
            )
            .mount(&server)
            .await;

        let error = client(&server).current_user().await.unwrap_err();
        assert!(error.to_string().contains("Bad credentials"));
    }
}
