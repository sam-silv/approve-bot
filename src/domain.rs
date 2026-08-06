use std::collections::HashSet;

use regex::Regex;
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct PullRequestRef {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl std::fmt::Display for PullRequestRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}#{}", self.owner, self.repo, self.number)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct PullRequest {
    pub number: u64,
    pub state: String,
    pub draft: bool,
    pub html_url: String,
    pub title: String,
    pub user: GitHubUser,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct GitHubUser {
    pub login: String,
}

#[must_use]
pub fn parse_pull_request_urls(text: &str, expected_host: &str) -> Vec<PullRequestRef> {
    let mut seen = HashSet::new();
    text.split_whitespace()
        .filter_map(|word| parse_pull_request_url(word, expected_host))
        .filter(|pull| seen.insert(pull.clone()))
        .collect()
}

fn parse_pull_request_url(candidate: &str, expected_host: &str) -> Option<PullRequestRef> {
    let candidate = candidate.trim_matches(|character: char| {
        matches!(
            character,
            '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | '.' | '!' | '?' | '\'' | '"'
        )
    });
    let url = Url::parse(candidate).ok()?;
    if url.scheme() != "https" || url.host_str()? != expected_host {
        return None;
    }

    let segments: Vec<_> = url.path_segments()?.collect();
    if segments.len() < 4 || segments[2] != "pull" {
        return None;
    }

    let name = Regex::new(r"^[A-Za-z0-9_.-]+$").expect("static repository name regex");
    let owner = segments[0];
    let repo = segments[1].strip_suffix(".git").unwrap_or(segments[1]);
    if !name.is_match(owner) || !name.is_match(repo) {
        return None;
    }

    let number = segments[3].parse::<u64>().ok()?;
    if number == 0 {
        return None;
    }

    Some(PullRequestRef {
        owner: owner.to_owned(),
        repo: repo.to_owned(),
        number,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_pull_request_url() {
        assert_eq!(
            parse_pull_request_urls("https://github.com/acme/rockets/pull/42", "github.com"),
            vec![PullRequestRef {
                owner: "acme".into(),
                repo: "rockets".into(),
                number: 42,
            }]
        );
    }

    #[test]
    fn parses_urls_wrapped_by_slack_and_punctuation() {
        assert_eq!(
            parse_pull_request_urls(
                "please do <https://github.com/acme/api/pull/7>!",
                "github.com"
            )[0]
            .number,
            7
        );
    }

    #[test]
    fn accepts_pr_subpages_and_query_strings() {
        assert_eq!(
            parse_pull_request_urls(
                "https://github.com/acme/api/pull/9/files?diff=split",
                "github.com"
            )[0]
            .number,
            9
        );
    }

    #[test]
    fn extracts_multiple_unique_urls_in_order() {
        let pulls = parse_pull_request_urls(
            "https://github.com/a/one/pull/1 https://github.com/b/two/pull/2 https://github.com/a/one/pull/1",
            "github.com",
        );
        assert_eq!(pulls.len(), 2);
        assert_eq!(pulls[0].owner, "a");
        assert_eq!(pulls[1].owner, "b");
    }

    #[test]
    fn rejects_http_urls() {
        assert!(parse_pull_request_urls("http://github.com/a/b/pull/1", "github.com").is_empty());
    }

    #[test]
    fn rejects_lookalike_hosts() {
        assert!(
            parse_pull_request_urls("https://github.com.evil.example/a/b/pull/1", "github.com")
                .is_empty()
        );
    }

    #[test]
    fn rejects_issue_urls() {
        assert!(
            parse_pull_request_urls("https://github.com/a/b/issues/1", "github.com").is_empty()
        );
    }

    #[test]
    fn rejects_zero_and_non_numeric_pull_numbers() {
        assert!(parse_pull_request_urls("https://github.com/a/b/pull/0", "github.com").is_empty());
        assert!(
            parse_pull_request_urls("https://github.com/a/b/pull/nope", "github.com").is_empty()
        );
    }

    #[test]
    fn supports_github_enterprise_hosts() {
        assert_eq!(
            parse_pull_request_urls("https://git.example.test/a/b/pull/3", "git.example.test")[0]
                .number,
            3
        );
    }
}
