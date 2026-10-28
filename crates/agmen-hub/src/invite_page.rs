//! The static invite page keeps the invite code in the browser's fragment.

use axum::http::header;
use axum::response::{IntoResponse as _, Response};

const INVITE_PAGE: &str = include_str!("invite_page.html");
pub(crate) const INVITE_PAGE_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

pub(crate) async fn page() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_SECURITY_POLICY, INVITE_PAGE_CSP),
        ],
        INVITE_PAGE,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_reads_the_code_only_from_the_fragment() {
        assert!(INVITE_PAGE.contains("location.hash"));
        assert!(INVITE_PAGE.contains("/^[0-9a-f]{64}$/"));
        for forbidden in [
            "fetch(",
            "XMLHttpRequest",
            "sendBeacon",
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "location.search",
        ] {
            assert!(!INVITE_PAGE.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn page_shows_the_install_lines_and_join() {
        for line in [
            "curl --proto '=https' --tlsv1.2 -fsSL https://getagmen.ai/install.sh | sh",
            "irm https://getagmen.ai/install.ps1 | iex",
            "brew install agmen-dev/tap/agmen",
            "agmen join ",
        ] {
            assert!(INVITE_PAGE.contains(line), "{line}");
        }
    }

    #[test]
    fn csp_blocks_every_request() {
        for directive in [
            "default-src 'none'",
            "frame-ancestors 'none'",
            "base-uri 'none'",
            "form-action 'none'",
        ] {
            assert!(INVITE_PAGE_CSP.contains(directive), "{directive}");
        }
        assert!(!INVITE_PAGE_CSP.contains("connect-src"));
        assert!(!INVITE_PAGE_CSP.contains("img-src"));
    }
}
