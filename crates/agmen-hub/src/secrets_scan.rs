//! Credential-shaped text the hub refuses to record (protocol 5.2, NACK
//! `secret_detected`).
//!
//! The patterns are the gitleaks default rules (`config/gitleaks.toml` at
//! commit 5f6ec56, 2026-10-23) for GitHub, GitLab, Slack, AWS, Anthropic and
//! OpenAI keys and private key headers, copied as gitleaks writes them.
//! gitleaks has no OpenRouter rule, so that one follows OpenRouter's key
//! format (`sk-or-v1-` and 64 hex digits). gitleaks also checks entropy and
//! allowlists; the hub matches the regexes alone, so it refuses a little
//! more than gitleaks would report.

use std::sync::LazyLock;

use regex::RegexSet;

/// One regex per rule, named by its gitleaks id.
const PATTERNS: &[(&str, &str)] = &[
    ("github-pat", r"ghp_[0-9a-zA-Z]{36}"),
    ("github-oauth", r"gho_[0-9a-zA-Z]{36}"),
    ("github-app-token", r"(?:ghu|ghs)_[0-9a-zA-Z]{36}"),
    ("github-refresh-token", r"ghr_[0-9a-zA-Z]{36}"),
    ("github-fine-grained-pat", r"github_pat_\w{82}"),
    ("gitlab-pat", r"glpat-[\w-]{20}"),
    (
        "gitlab-pat-routable",
        r"\bglpat-[0-9a-zA-Z_-]{27,300}\.[0-9a-z]{2}[0-9a-z]{7}\b",
    ),
    ("slack-app-token", r"(?i)xapp-\d-[A-Z0-9]+-\d+-[a-z0-9]+"),
    (
        "slack-bot-token",
        r"xoxb-[0-9]{10,13}-[0-9]{10,13}[a-zA-Z0-9-]*",
    ),
    (
        "slack-config-access-token",
        r"(?i)xoxe.xox[bp]-\d-[A-Z0-9]{163,166}",
    ),
    ("slack-config-refresh-token", r"(?i)xoxe-\d-[A-Z0-9]{146}"),
    (
        "slack-legacy-bot-token",
        r"xoxb-[0-9]{8,14}-[a-zA-Z0-9]{18,26}",
    ),
    ("slack-legacy-token", r"xox[os]-\d+-\d+-\d+-[a-fA-F\d]+"),
    (
        "slack-legacy-workspace-token",
        r"xox[ar]-(?:\d-)?[0-9a-zA-Z]{8,48}",
    ),
    (
        "slack-user-token",
        r"xox[pe](?:-[0-9]{10,13}){3}-[a-zA-Z0-9-]{28,34}",
    ),
    (
        "slack-webhook-url",
        r"(?:https?://)?hooks.slack.com/(?:services|workflows|triggers)/[A-Za-z0-9+/]{43,56}",
    ),
    (
        "aws-access-token",
        r"\b((?:A3T[A-Z0-9]|AKIA|ASIA|ABIA|ACCA)[A-Z2-7]{16})\b",
    ),
    (
        "anthropic-api-key",
        r#"\b(sk-ant-api03-[a-zA-Z0-9_\-]{93}AA)(?:[\x60'"\s;]|\\[nr]|$)"#,
    ),
    (
        "anthropic-admin-api-key",
        r#"\b(sk-ant-admin01-[a-zA-Z0-9_\-]{93}AA)(?:[\x60'"\s;]|\\[nr]|$)"#,
    ),
    (
        "openai-api-key",
        r#"\b(sk-(?:proj|svcacct|admin)-(?:[A-Za-z0-9_-]{74}|[A-Za-z0-9_-]{58})T3BlbkFJ(?:[A-Za-z0-9_-]{74}|[A-Za-z0-9_-]{58})\b|sk-[a-zA-Z0-9]{20}T3BlbkFJ[a-zA-Z0-9]{20})(?:[\x60'"\s;]|\\[nr]|$)"#,
    ),
    ("openrouter-api-key", r"\bsk-or-v1-[0-9a-f]{64}\b"),
    (
        "private-key",
        r"(?i)-----BEGIN[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----[\s\S-]{64,}?KEY(?: BLOCK)?-----",
    ),
];

/// Every pattern, compiled once.
static SECRETS: LazyLock<Option<RegexSet>> = LazyLock::new(|| {
    RegexSet::new(PATTERNS.iter().map(|(_, pattern)| pattern))
        .inspect_err(|err| tracing::error!(error = %err, "secret patterns do not compile"))
        .ok()
});

/// The gitleaks id of the first rule `text` matches, if any. If the
/// patterns failed to compile, which the unit tests rule out, every text
/// counts as a match, so the hub refuses rather than records unscanned
/// text.
pub(crate) fn find_secret(text: &str) -> Option<&'static str> {
    let Some(set) = SECRETS.as_ref() else {
        return Some("unscanned");
    };
    set.matches(text)
        .iter()
        .next()
        .and_then(|i| PATTERNS.get(i))
        .map(|(id, _)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures are built at runtime (ruling R22), so the commit hook and
    // gitleaks never see a secret-shaped literal in this file.
    fn alnum(n: usize) -> String {
        "aB3".repeat(n).chars().take(n).collect()
    }

    #[test]
    fn patterns_compile() {
        assert!(SECRETS.is_some());
    }

    #[test]
    fn each_family_is_caught() {
        let cases = [
            (format!("token {}{} here", "ghp_", alnum(36)), "github-pat"),
            (format!("{}{}", "gho_", alnum(36)), "github-oauth"),
            (format!("{}{}", "ghs_", alnum(36)), "github-app-token"),
            (
                format!("{}{}", "github_pat_", "a_".repeat(41)),
                "github-fine-grained-pat",
            ),
            (format!("{}{}", "glpat-", "x".repeat(20)), "gitlab-pat"),
            (
                format!("{}-1234567890-1234567890-abc", "xoxb"),
                "slack-bot-token",
            ),
            (
                format!("id {}{} end", "AKIA", "QZ2Z3Z4Z5Z6Z7ZAB"),
                "aws-access-token",
            ),
            (
                format!("{}{}AA ", "sk-ant-api03-", "a".repeat(93)),
                "anthropic-api-key",
            ),
            (
                format!("{}{}{}{} ", "sk-", alnum(20), "T3BlbkFJ", alnum(20)),
                "openai-api-key",
            ),
            (
                format!("{}{}", "sk-or-v1-", "0f".repeat(32)),
                "openrouter-api-key",
            ),
            (
                format!(
                    "-----BEGIN {} PRIVATE KEY-----\n{}\n-----END {} PRIVATE KEY-----",
                    "EC",
                    "A".repeat(64),
                    "EC"
                ),
                "private-key",
            ),
        ];
        for (text, id) in cases {
            assert_eq!(find_secret(&text), Some(id), "{text}");
        }
    }

    #[test]
    fn ordinary_text_passes() {
        for text in [
            "Fixed the truncation notice; regression spec added.",
            "ghp_ is GitHub's token prefix",
            "see https://github.com/gitleaks/gitleaks",
            "B-412 on L-UI, commit 1f0cd24",
            "-----BEGIN PUBLIC KEY-----",
        ] {
            assert_eq!(find_secret(text), None, "{text}");
        }
    }
}
