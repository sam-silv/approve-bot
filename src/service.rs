use std::{collections::HashSet, sync::Arc};

use rand::{Rng, prelude::IndexedRandom};
use tracing::{error, info, warn};

use crate::{
    domain::{GitHubUser, PullRequestRef, parse_pull_request_urls},
    github::GitHubApi,
    slack::SlackApi,
};

#[derive(Clone, Debug)]
pub struct IncomingMessage {
    pub user: String,
    pub channel: String,
    pub text: String,
    pub message_ts: String,
    pub thread_ts: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ServiceSettings {
    pub github_web_host: String,
    pub allowed_slack_users: HashSet<String>,
    pub allow_all_users: bool,
    pub approval_review_body: Option<String>,
    pub dry_run: bool,
}

pub trait CommentPolicy: Send + Sync {
    fn choose(&self) -> Option<String>;
}

pub struct RandomCommentPolicy {
    comments: Vec<String>,
    probability: f64,
}

impl RandomCommentPolicy {
    #[must_use]
    pub fn new(comments: Vec<String>, probability: f64) -> Self {
        Self {
            comments,
            probability,
        }
    }
}

impl CommentPolicy for RandomCommentPolicy {
    fn choose(&self) -> Option<String> {
        let mut rng = rand::rng();
        if self.comments.is_empty() || !rng.random_bool(self.probability) {
            return None;
        }
        self.comments.choose(&mut rng).cloned()
    }
}

pub struct ApprovalService {
    github: Arc<dyn GitHubApi>,
    slack: Arc<dyn SlackApi>,
    comments: Arc<dyn CommentPolicy>,
    settings: ServiceSettings,
}

impl ApprovalService {
    pub fn new(
        github: Arc<dyn GitHubApi>,
        slack: Arc<dyn SlackApi>,
        comments: Arc<dyn CommentPolicy>,
        settings: ServiceSettings,
    ) -> Self {
        Self {
            github,
            slack,
            comments,
            settings,
        }
    }

    pub async fn process(&self, message: IncomingMessage) {
        let response = self.process_inner(&message).await;
        let thread_ts = message.thread_ts.as_deref().or(Some(&message.message_ts));
        if let Err(error) = self
            .slack
            .post_message(&message.channel, thread_ts, &response)
            .await
        {
            error!(%error, channel = %message.channel, "failed to report approval result to Slack");
        }
    }

    async fn process_inner(&self, message: &IncomingMessage) -> String {
        if !self.settings.allow_all_users
            && !self.settings.allowed_slack_users.contains(&message.user)
        {
            warn!(slack_user = %message.user, "rejected approval request from unauthorized user");
            return ":no_entry: You are not authorized to ask me for approvals.".into();
        }

        let pulls = parse_pull_request_urls(&message.text, &self.settings.github_web_host);
        if pulls.is_empty() {
            return format!(
                ":link: Send me a full `https://{}/owner/repo/pull/123` URL.",
                self.settings.github_web_host
            );
        }

        let approver = match self.github.current_user().await {
            Ok(user) => user,
            Err(error) => {
                error!(%error, "could not identify authenticated GitHub user");
                return ":x: GitHub authentication failed. Check the bot logs and token.".into();
            }
        };

        let mut results = Vec::with_capacity(pulls.len());
        for pull in pulls {
            results.push(self.process_pull(&approver, &pull).await);
        }
        results.join("\n")
    }

    async fn process_pull(&self, approver: &GitHubUser, reference: &PullRequestRef) -> String {
        let pull = match self.github.pull_request(reference).await {
            Ok(pull) => pull,
            Err(error) => {
                warn!(%error, pull_request = %reference, "could not fetch pull request");
                return format!(":x: `{reference}` — could not fetch this PR.");
            }
        };

        if pull.state != "open" {
            return format!(":no_entry_sign: `{reference}` — PR is {}.", pull.state);
        }
        if pull.draft {
            return format!(":construction: `{reference}` — still a draft; not approving.");
        }
        if pull.user.login.eq_ignore_ascii_case(&approver.login) {
            return format!(":no_entry_sign: `{reference}` — GitHub does not allow self-approval.");
        }
        if self.settings.dry_run {
            return format!(":test_tube: `{reference}` — would approve (dry run). ");
        }

        let mut comment_warning = false;
        if let Some(comment) = self.comments.choose() {
            if let Err(error) = self.github.comment(reference, &comment).await {
                comment_warning = true;
                warn!(%error, pull_request = %reference, "optional comment failed; continuing with approval");
            }
        }

        match self
            .github
            .approve(reference, self.settings.approval_review_body.as_deref())
            .await
        {
            Ok(()) => {
                info!(pull_request = %reference, github_user = %approver.login, "pull request approved");
                if comment_warning {
                    format!(
                        ":white_check_mark: `{reference}` — approved (optional comment failed)."
                    )
                } else {
                    format!(":white_check_mark: `{reference}` — approved. Profit.")
                }
            }
            Err(error) => {
                warn!(%error, pull_request = %reference, "approval failed");
                format!(":x: `{reference}` — GitHub rejected the approval.")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use crate::{
        domain::PullRequest,
        github::GitHubError,
        slack::{SlackApi, SlackError},
    };

    use super::*;

    #[derive(Default)]
    #[allow(clippy::struct_excessive_bools)]
    struct MockGitHub {
        pulls: Mutex<Vec<PullRequestRef>>,
        approvals: Mutex<Vec<(PullRequestRef, Option<String>)>>,
        comments: Mutex<Vec<(PullRequestRef, String)>>,
        pull_state: Mutex<Option<PullRequest>>,
        fail_user: bool,
        fail_fetch: bool,
        fail_approve: bool,
        fail_comment: bool,
    }

    #[async_trait]
    impl GitHubApi for MockGitHub {
        async fn current_user(&self) -> Result<GitHubUser, GitHubError> {
            if self.fail_user {
                return Err(api_error());
            }
            Ok(GitHubUser {
                login: "sam".into(),
            })
        }

        async fn pull_request(&self, pull: &PullRequestRef) -> Result<PullRequest, GitHubError> {
            self.pulls.lock().unwrap().push(pull.clone());
            if self.fail_fetch {
                return Err(api_error());
            }
            Ok(self
                .pull_state
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(open_pull))
        }

        async fn approve(
            &self,
            pull: &PullRequestRef,
            body: Option<&str>,
        ) -> Result<(), GitHubError> {
            self.approvals
                .lock()
                .unwrap()
                .push((pull.clone(), body.map(str::to_owned)));
            if self.fail_approve {
                Err(api_error())
            } else {
                Ok(())
            }
        }

        async fn comment(&self, pull: &PullRequestRef, body: &str) -> Result<(), GitHubError> {
            self.comments
                .lock()
                .unwrap()
                .push((pull.clone(), body.to_owned()));
            if self.fail_comment {
                Err(api_error())
            } else {
                Ok(())
            }
        }
    }

    #[derive(Default)]
    struct MockSlack {
        messages: Mutex<Vec<(String, Option<String>, String)>>,
    }

    #[async_trait]
    impl SlackApi for MockSlack {
        async fn post_message(
            &self,
            channel: &str,
            thread_ts: Option<&str>,
            text: &str,
        ) -> Result<(), SlackError> {
            self.messages.lock().unwrap().push((
                channel.into(),
                thread_ts.map(str::to_owned),
                text.into(),
            ));
            Ok(())
        }
    }

    struct FixedComment(Option<String>);

    impl CommentPolicy for FixedComment {
        fn choose(&self) -> Option<String> {
            self.0.clone()
        }
    }

    fn api_error() -> GitHubError {
        GitHubError::Api {
            status: reqwest::StatusCode::BAD_REQUEST,
            message: "nope".into(),
        }
    }

    fn open_pull() -> PullRequest {
        PullRequest {
            number: 1,
            state: "open".into(),
            draft: false,
            html_url: "https://github.com/acme/api/pull/1".into(),
            title: "A good change".into(),
            user: GitHubUser {
                login: "friend".into(),
            },
        }
    }

    fn message(text: &str) -> IncomingMessage {
        IncomingMessage {
            user: "U_ALLOWED".into(),
            channel: "D1".into(),
            text: text.into(),
            message_ts: "100.1".into(),
            thread_ts: None,
        }
    }

    fn service(
        github: Arc<MockGitHub>,
        slack: Arc<MockSlack>,
        comment: Option<&str>,
        dry_run: bool,
    ) -> ApprovalService {
        ApprovalService::new(
            github,
            slack,
            Arc::new(FixedComment(comment.map(str::to_owned))),
            ServiceSettings {
                github_web_host: "github.com".into(),
                allowed_slack_users: HashSet::from(["U_ALLOWED".into()]),
                allow_all_users: false,
                approval_review_body: Some("LGTM from OOO-land".into()),
                dry_run,
            },
        )
    }

    fn last_message(slack: &MockSlack) -> String {
        slack.messages.lock().unwrap().last().unwrap().2.clone()
    }

    #[tokio::test]
    async fn approves_a_valid_pr_and_replies_in_a_thread() {
        let github = Arc::new(MockGitHub::default());
        let slack = Arc::new(MockSlack::default());
        service(github.clone(), slack.clone(), None, false)
            .process(message("https://github.com/acme/api/pull/1"))
            .await;
        assert_eq!(github.approvals.lock().unwrap().len(), 1);
        assert_eq!(
            github.approvals.lock().unwrap()[0].1.as_deref(),
            Some("LGTM from OOO-land")
        );
        assert!(last_message(&slack).contains("approved. Profit"));
        assert_eq!(
            slack.messages.lock().unwrap()[0].1.as_deref(),
            Some("100.1")
        );
    }

    #[tokio::test]
    async fn rejects_unauthorized_slack_users_before_calling_github() {
        let github = Arc::new(MockGitHub::default());
        let slack = Arc::new(MockSlack::default());
        let mut incoming = message("https://github.com/acme/api/pull/1");
        incoming.user = "U_INTRUDER".into();
        service(github.clone(), slack.clone(), None, false)
            .process(incoming)
            .await;
        assert!(github.pulls.lock().unwrap().is_empty());
        assert!(last_message(&slack).contains("not authorized"));
    }

    #[tokio::test]
    async fn explains_when_no_pr_url_is_present() {
        let github = Arc::new(MockGitHub::default());
        let slack = Arc::new(MockSlack::default());
        service(github.clone(), slack.clone(), None, false)
            .process(message("hey please approve the thing"))
            .await;
        assert!(github.pulls.lock().unwrap().is_empty());
        assert!(last_message(&slack).contains("full `https://github.com"));
    }

    #[tokio::test]
    async fn refuses_draft_pull_requests() {
        let github = Arc::new(MockGitHub::default());
        let mut pull = open_pull();
        pull.draft = true;
        *github.pull_state.lock().unwrap() = Some(pull);
        let slack = Arc::new(MockSlack::default());
        service(github.clone(), slack.clone(), None, false)
            .process(message("https://github.com/acme/api/pull/1"))
            .await;
        assert!(github.approvals.lock().unwrap().is_empty());
        assert!(last_message(&slack).contains("still a draft"));
    }

    #[tokio::test]
    async fn refuses_closed_pull_requests() {
        let github = Arc::new(MockGitHub::default());
        let mut pull = open_pull();
        pull.state = "closed".into();
        *github.pull_state.lock().unwrap() = Some(pull);
        let slack = Arc::new(MockSlack::default());
        service(github.clone(), slack.clone(), None, false)
            .process(message("https://github.com/acme/api/pull/1"))
            .await;
        assert!(github.approvals.lock().unwrap().is_empty());
        assert!(last_message(&slack).contains("PR is closed"));
    }

    #[tokio::test]
    async fn refuses_self_approval_case_insensitively() {
        let github = Arc::new(MockGitHub::default());
        let mut pull = open_pull();
        pull.user.login = "SAM".into();
        *github.pull_state.lock().unwrap() = Some(pull);
        let slack = Arc::new(MockSlack::default());
        service(github.clone(), slack.clone(), None, false)
            .process(message("https://github.com/acme/api/pull/1"))
            .await;
        assert!(github.approvals.lock().unwrap().is_empty());
        assert!(last_message(&slack).contains("self-approval"));
    }

    #[tokio::test]
    async fn posts_optional_comment_before_approval() {
        let github = Arc::new(MockGitHub::default());
        let slack = Arc::new(MockSlack::default());
        service(github.clone(), slack, Some("nice commas"), false)
            .process(message("https://github.com/acme/api/pull/1"))
            .await;
        assert_eq!(github.comments.lock().unwrap()[0].1, "nice commas");
        assert_eq!(github.approvals.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn comment_failure_does_not_prevent_approval() {
        let github = Arc::new(MockGitHub {
            fail_comment: true,
            ..MockGitHub::default()
        });
        let slack = Arc::new(MockSlack::default());
        service(github.clone(), slack.clone(), Some("ship it"), false)
            .process(message("https://github.com/acme/api/pull/1"))
            .await;
        assert_eq!(github.approvals.lock().unwrap().len(), 1);
        assert!(last_message(&slack).contains("optional comment failed"));
    }

    #[tokio::test]
    async fn dry_run_never_comments_or_approves() {
        let github = Arc::new(MockGitHub::default());
        let slack = Arc::new(MockSlack::default());
        service(github.clone(), slack.clone(), Some("ship it"), true)
            .process(message("https://github.com/acme/api/pull/1"))
            .await;
        assert!(github.comments.lock().unwrap().is_empty());
        assert!(github.approvals.lock().unwrap().is_empty());
        assert!(last_message(&slack).contains("dry run"));
    }

    #[tokio::test]
    async fn processes_multiple_prs_from_one_message() {
        let github = Arc::new(MockGitHub::default());
        let slack = Arc::new(MockSlack::default());
        service(github.clone(), slack.clone(), None, false)
            .process(message(
                "https://github.com/a/one/pull/1 https://github.com/b/two/pull/2",
            ))
            .await;
        assert_eq!(github.approvals.lock().unwrap().len(), 2);
        assert_eq!(last_message(&slack).lines().count(), 2);
    }

    #[tokio::test]
    async fn reports_authentication_and_approval_failures() {
        let github = Arc::new(MockGitHub {
            fail_user: true,
            ..MockGitHub::default()
        });
        let slack = Arc::new(MockSlack::default());
        service(github, slack.clone(), None, false)
            .process(message("https://github.com/a/b/pull/1"))
            .await;
        assert!(last_message(&slack).contains("authentication failed"));

        let github = Arc::new(MockGitHub {
            fail_approve: true,
            ..MockGitHub::default()
        });
        service(github, slack.clone(), None, false)
            .process(message("https://github.com/a/b/pull/1"))
            .await;
        assert!(last_message(&slack).contains("rejected the approval"));
    }
}
