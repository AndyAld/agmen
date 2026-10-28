//! Opens URLs and copies text for the person at a terminal, unlike `cmd/open.rs`.

use std::io::IsTerminal as _;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt as _;
use url::Url;

const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

fn openable(url: &Url) -> bool {
    agmen_client::check_hub_url(url).is_ok()
}

pub(crate) fn open_url(url: &Url) -> bool {
    if !std::io::stdout().is_terminal() || !openable(url) {
        return false;
    }
    #[cfg(target_os = "macos")]
    let result = run_first(&[("open", &[url.as_str()])], None);
    #[cfg(target_os = "linux")]
    let result = run_first(&[("xdg-open", &[url.as_str()])], None);
    #[cfg(target_os = "windows")]
    let result = run_first(
        &[("rundll32", &["url.dll,FileProtocolHandler", url.as_str()])],
        None,
    );
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let result = false;
    result
}

pub(crate) fn copy_text(text: &str) -> bool {
    if !std::io::stdout().is_terminal() {
        return false;
    }
    run_first(
        &[
            ("pbcopy", &[]),
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
            ("clip", &[]),
        ],
        Some(text),
    )
}

fn run_first(candidates: &[(&str, &[&str])], input: Option<&str>) -> bool {
    // This runs on the CLI's blocking pool; async pipes bound writes as well as waits.
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    for (program, args) in candidates {
        let success = runtime.block_on(async {
            let Ok(mut child) = tokio::process::Command::new(program)
                .args(*args)
                .stdin(if input.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
            else {
                return false;
            };
            let result = tokio::time::timeout(OPEN_TIMEOUT, async {
                if let Some(text) = input {
                    let mut stdin = child
                        .stdin
                        .take()
                        .ok_or_else(|| std::io::Error::other("helper stdin was not piped"))?;
                    stdin.write_all(text.as_bytes()).await?;
                    // Dropping stdin sends EOF before waiting for the helper.
                }
                child.wait().await
            })
            .await;
            // kill_on_drop terminates the helper on timeout or a failed pipe write.
            matches!(result, Ok(Ok(status)) if status.success())
        });
        if success {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_or_loopback_http_opens() {
        for text in [
            "https://github.com/login/device",
            "http://127.0.0.1:8080/x",
            "http://localhost/x",
            "http://[::1]/x",
        ] {
            assert!(openable(&text.parse().unwrap()), "{text}");
        }
        for text in [
            "http://example.com",
            "file:///etc/passwd",
            "javascript:alert(1)",
        ] {
            assert!(!openable(&text.parse().unwrap()), "{text}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn run_first_falls_through_to_the_next_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("copied");
        let path = file.to_str().unwrap();
        assert!(run_first(
            &[
                ("agmen-no-such-program", &[][..]),
                ("sh", &["-c", "F=$1; cat > \"$F\"", "sh", path]),
            ],
            Some("x"),
        ));
        assert_eq!(std::fs::read_to_string(file).unwrap(), "x");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_first_works_on_the_blocking_pool() {
        let ran = crate::client::blocking(|| Ok(run_first(&[("true", &[][..])], None)))
            .await
            .unwrap();
        assert!(ran);
    }
}
