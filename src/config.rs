use std::{collections::HashSet, net::SocketAddr};

use thiserror::Error;
use url::Url;

#[derive(Clone)]
pub struct Settings {
    pub listen_addr: SocketAddr,
    pub slack_signing_secret: String,
    pub slack_bot_token: String,
    pub github_token: String,
    pub github_api_url: Url,
    pub github_web_host: String,
    pub allowed_slack_users: HashSet<String>,
    pub allow_all_users: bool,
    pub allow_channel_messages: bool,
    pub approval_comments: Vec<String>,
    pub comment_probability: f64,
    pub approval_review_body: Option<String>,
    pub dry_run: bool,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Settings")
            .field("listen_addr", &self.listen_addr)
            .field("slack_signing_secret", &"[REDACTED]")
            .field("slack_bot_token", &"[REDACTED]")
            .field("github_token", &"[REDACTED]")
            .field("github_api_url", &self.github_api_url)
            .field("github_web_host", &self.github_web_host)
            .field("allowed_slack_users", &self.allowed_slack_users)
            .field("allow_all_users", &self.allow_all_users)
            .field("allow_channel_messages", &self.allow_channel_messages)
            .field("approval_comments", &self.approval_comments)
            .field("comment_probability", &self.comment_probability)
            .field("approval_review_body", &self.approval_review_body)
            .field("dry_run", &self.dry_run)
            .finish()
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    Missing(&'static str),
    #[error("invalid value for {name}: {message}")]
    Invalid { name: &'static str, message: String },
    #[error("ALLOWED_SLACK_USER_IDS must contain at least one user unless ALLOW_ALL_USERS=true")]
    EmptyAllowlist,
}

impl Settings {
    /// Loads and validates settings from the process environment.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a required variable is missing or a value is invalid.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_pairs(std::env::vars())
    }

    /// Loads and validates settings from key-value pairs.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a required key is missing or a value is invalid.
    pub fn from_pairs<I, K, V>(pairs: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let values: std::collections::HashMap<String, String> = pairs
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();

        let required = |name: &'static str| {
            values
                .get(name)
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .ok_or(ConfigError::Missing(name))
        };
        let optional = |name: &str| values.get(name).map(String::as_str);

        let listen_addr = optional("LISTEN_ADDR")
            .unwrap_or("0.0.0.0:3000")
            .parse::<SocketAddr>()
            .map_err(|error| invalid("LISTEN_ADDR", error))?;
        let github_api_url =
            Url::parse(optional("GITHUB_API_URL").unwrap_or("https://api.github.com/"))
                .map_err(|error| invalid("GITHUB_API_URL", error))?;
        if github_api_url.scheme() != "https"
            && github_api_url.host_str() != Some("127.0.0.1")
            && github_api_url.host_str() != Some("localhost")
        {
            return Err(ConfigError::Invalid {
                name: "GITHUB_API_URL",
                message: "must use HTTPS (except localhost for development)".into(),
            });
        }

        let allow_all_users = parse_bool(optional("ALLOW_ALL_USERS"), false, "ALLOW_ALL_USERS")?;
        let allowed_slack_users = optional("ALLOWED_SLACK_USER_IDS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        if allowed_slack_users.is_empty() && !allow_all_users {
            return Err(ConfigError::EmptyAllowlist);
        }

        let comment_probability = optional("COMMENT_PROBABILITY")
            .unwrap_or("0")
            .parse::<f64>()
            .map_err(|error| invalid("COMMENT_PROBABILITY", error))?;
        if !(0.0..=1.0).contains(&comment_probability) {
            return Err(ConfigError::Invalid {
                name: "COMMENT_PROBABILITY",
                message: "must be between 0 and 1".into(),
            });
        }

        let approval_comments = optional("APPROVAL_COMMENTS")
            .unwrap_or_default()
            .split('|')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect();

        Ok(Self {
            listen_addr,
            slack_signing_secret: required("SLACK_SIGNING_SECRET")?,
            slack_bot_token: required("SLACK_BOT_TOKEN")?,
            github_token: required("GITHUB_TOKEN")?,
            github_api_url,
            github_web_host: optional("GITHUB_WEB_HOST")
                .unwrap_or("github.com")
                .to_owned(),
            allowed_slack_users,
            allow_all_users,
            allow_channel_messages: parse_bool(
                optional("ALLOW_CHANNEL_MESSAGES"),
                false,
                "ALLOW_CHANNEL_MESSAGES",
            )?,
            approval_comments,
            comment_probability,
            approval_review_body: optional("APPROVAL_REVIEW_BODY")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            dry_run: parse_bool(optional("DRY_RUN"), false, "DRY_RUN")?,
        })
    }
}

fn parse_bool(value: Option<&str>, default: bool, name: &'static str) -> Result<bool, ConfigError> {
    match value {
        None => Ok(default),
        Some("true" | "1" | "yes" | "on") => Ok(true),
        Some("false" | "0" | "no" | "off") => Ok(false),
        Some(other) => Err(ConfigError::Invalid {
            name,
            message: format!("expected a boolean, got {other:?}"),
        }),
    }
}

fn invalid(name: &'static str, error: impl std::fmt::Display) -> ConfigError {
    ConfigError::Invalid {
        name,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn required() -> Vec<(&'static str, &'static str)> {
        vec![
            ("SLACK_SIGNING_SECRET", "secret"),
            ("SLACK_BOT_TOKEN", "xoxb-token"),
            ("GITHUB_TOKEN", "github-token"),
            ("ALLOWED_SLACK_USER_IDS", "U1,U2"),
        ]
    }

    #[test]
    fn loads_defaults_and_trims_allowlist() {
        let settings = Settings::from_pairs(required()).unwrap();
        assert_eq!(settings.listen_addr.to_string(), "0.0.0.0:3000");
        assert_eq!(settings.allowed_slack_users.len(), 2);
        assert!(!settings.allow_all_users);
        assert!(settings.comment_probability.abs() < f64::EPSILON);
    }

    #[test]
    fn requires_each_secret() {
        for missing in ["SLACK_SIGNING_SECRET", "SLACK_BOT_TOKEN", "GITHUB_TOKEN"] {
            let pairs = required()
                .into_iter()
                .filter(|(name, _)| *name != missing)
                .collect::<Vec<_>>();
            assert!(matches!(
                Settings::from_pairs(pairs),
                Err(ConfigError::Missing(name)) if name == missing
            ));
        }
    }

    #[test]
    fn empty_allowlist_is_rejected_by_default() {
        let pairs = required()
            .into_iter()
            .filter(|(name, _)| *name != "ALLOWED_SLACK_USER_IDS");
        assert!(matches!(
            Settings::from_pairs(pairs),
            Err(ConfigError::EmptyAllowlist)
        ));
    }

    #[test]
    fn allow_all_users_explicitly_permits_an_empty_allowlist() {
        let mut pairs = required();
        pairs.retain(|(name, _)| *name != "ALLOWED_SLACK_USER_IDS");
        pairs.push(("ALLOW_ALL_USERS", "true"));
        assert!(Settings::from_pairs(pairs).unwrap().allow_all_users);
    }

    #[test]
    fn validates_comment_probability() {
        let mut pairs = required();
        pairs.push(("COMMENT_PROBABILITY", "1.1"));
        assert!(matches!(
            Settings::from_pairs(pairs),
            Err(ConfigError::Invalid {
                name: "COMMENT_PROBABILITY",
                ..
            })
        ));
    }

    #[test]
    fn parses_comment_list_and_review_body() {
        let mut pairs = required();
        pairs.push(("APPROVAL_COMMENTS", "ship it | nice commas | "));
        pairs.push(("APPROVAL_REVIEW_BODY", " LGTM "));
        let settings = Settings::from_pairs(pairs).unwrap();
        assert_eq!(settings.approval_comments, ["ship it", "nice commas"]);
        assert_eq!(settings.approval_review_body.as_deref(), Some("LGTM"));
    }

    #[test]
    fn rejects_invalid_booleans() {
        let mut pairs = required();
        pairs.push(("DRY_RUN", "perhaps"));
        assert!(matches!(
            Settings::from_pairs(pairs),
            Err(ConfigError::Invalid {
                name: "DRY_RUN",
                ..
            })
        ));
    }

    #[test]
    fn debug_output_redacts_secrets() {
        let settings = Settings::from_pairs(required()).unwrap();
        let debug = format!("{settings:?}");
        assert!(!debug.contains("xoxb-token"));
        assert!(!debug.contains("github-token"));
        assert!(debug.contains("[REDACTED]"));
    }
}
