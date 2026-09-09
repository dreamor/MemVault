//! Content-based sensitive-info detection.
//!
//! Complements the provenance-based guard in [`crate::extractor`]
//! (`SourceRole::Agent` rejection): that one asks "who produced this text",
//! this one asks "does this text itself contain something that should never
//! be persisted regardless of who said it" — credentials, private keys,
//! bearer tokens. Deliberately minimal: no credit-card/SSN/entropy-based
//! detection, just the patterns that are unambiguous keep-out signals.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensitiveKind {
    AwsAccessKey,
    PrivateKeyBlock,
    PasswordAssignment,
    SecretAssignment,
    BearerToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensitiveMatch {
    pub kind: SensitiveKind,
    /// Truncated to avoid echoing the full secret into logs/reports.
    pub snippet: String,
}

/// `(kind, pattern)` — adding a new provider's key format later is a single
/// entry here, not a redesign.
static RULES: LazyLock<Vec<(SensitiveKind, Regex)>> = LazyLock::new(|| {
    vec![
        (
            SensitiveKind::AwsAccessKey,
            Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
        ),
        (
            SensitiveKind::PrivateKeyBlock,
            Regex::new(r"-----BEGIN (RSA |EC |OPENSSH |PGP )?PRIVATE KEY-----").unwrap(),
        ),
        (
            SensitiveKind::PasswordAssignment,
            Regex::new(r"(?i)\b(password|passwd|pwd)\s*[:=]\s*\S{4,}").unwrap(),
        ),
        (
            SensitiveKind::SecretAssignment,
            Regex::new(r"(?i)\b(secret|api[_-]?key|token)\s*[:=]\s*\S{4,}").unwrap(),
        ),
        (
            SensitiveKind::BearerToken,
            Regex::new(r"Bearer [A-Za-z0-9._-]{20,}").unwrap(),
        ),
    ]
});

const SNIPPET_MAX_LEN: usize = 24;

pub fn scan(text: &str) -> Vec<SensitiveMatch> {
    RULES
        .iter()
        .filter_map(|(kind, re)| {
            re.find(text).map(|m| SensitiveMatch {
                kind: *kind,
                snippet: truncate(m.as_str()),
            })
        })
        .collect()
}

pub fn is_sensitive(text: &str) -> bool {
    RULES.iter().any(|(_, re)| re.is_match(text))
}

fn truncate(s: &str) -> String {
    if s.len() <= SNIPPET_MAX_LEN {
        s.to_string()
    } else {
        format!("{}…", &s[..SNIPPET_MAX_LEN])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_aws_access_key() {
        let m = scan("prod key is AKIAABCDEFGHIJKLMNOP, keep it safe");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].kind, SensitiveKind::AwsAccessKey);
    }

    #[test]
    fn detects_private_key_block() {
        assert!(is_sensitive(
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEow...\n-----END RSA PRIVATE KEY-----"
        ));
    }

    #[test]
    fn detects_password_assignment() {
        assert!(is_sensitive("db password=Sup3rSecret!"));
        assert!(is_sensitive("PWD: hunter2222"));
    }

    #[test]
    fn detects_secret_and_token_assignment() {
        assert!(is_sensitive("api_key=sk-abcdefghijklmno"));
        assert!(is_sensitive("token: eyJhbGciOi.abcdef"));
    }

    #[test]
    fn detects_bearer_token() {
        assert!(is_sensitive(
            "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"
        ));
    }

    #[test]
    fn plain_preference_text_is_not_sensitive() {
        assert!(!is_sensitive("用户偏好 Python，不用 Java"));
        assert!(!is_sensitive("prefers dark mode and terse commit messages"));
    }

    #[test]
    fn snippet_is_truncated_not_full_secret() {
        let long_secret = "token=".to_string() + &"a".repeat(200);
        let m = scan(&long_secret);
        assert_eq!(m.len(), 1);
        assert!(m[0].snippet.len() <= SNIPPET_MAX_LEN + 3);
    }
}
