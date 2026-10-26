use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use super::*;
use crate::appearance::{PickerEntry, ThemeOrigin, installed_themes, picker_entries, theme_dirs};

/// One request the fixture saw: its request line and User-Agent.
#[derive(Debug, Clone)]
struct Seen {
    line: String,
    user_agent: Option<String>,
}

/// How long the fixture waits for its requests, and for each read or write.
const FIXTURE_TIMEOUT: Duration = Duration::from_secs(10);

type Routes = Vec<(&'static str, u16, Vec<u8>)>;

/// A local HTTP server that answers each path prefix with a fixed status and
/// body and records every request. It never reaches the real API. It
/// answers exactly `requests` requests, then stops listening, so an extra
/// request fails at once; it gives up after [`FIXTURE_TIMEOUT`]. Errors in
/// its thread come back over a channel, and they and any request that never
/// came fail the test when it drops (C235).
struct Fixture {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    errors: Receiver<String>,
    expected: usize,
}

impl Fixture {
    fn serve(requests: usize, routes: Routes) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (report, errors) = channel();
        let log = seen.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + FIXTURE_TIMEOUT;
            let mut served = 0;
            while served < requests {
                if Instant::now() >= deadline {
                    report_err(
                        &report,
                        format!("only {served} of {requests} requests came"),
                    );
                    return;
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        served += 1;
                        if let Err(err) = answer(stream, &routes, &log) {
                            report_err(&report, err.to_string());
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(err) => report_err(&report, err.to_string()),
                }
            }
        });
        Self {
            base,
            seen,
            errors,
            expected: requests,
        }
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let mut errors: Vec<String> = self.errors.try_iter().collect();
        // Each request is recorded before its answer is written, and the
        // client waits for that answer, so the count is final here.
        let came = self.seen.lock().map_or(0, |seen| seen.len());
        if came < self.expected {
            errors.push(format!("only {came} of {} requests came", self.expected));
        }
        if !std::thread::panicking() {
            assert!(errors.is_empty(), "fixture server: {errors:?}");
        }
    }
}

fn report_err(report: &Sender<String>, err: String) {
    // The test may have finished and dropped the receiver; nothing to tell.
    report.send(err).unwrap_or(());
}

/// Reads one request from `stream`, records it and writes the route's answer.
fn answer(stream: TcpStream, routes: &Routes, log: &Mutex<Vec<Seen>>) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(FIXTURE_TIMEOUT))?;
    stream.set_write_timeout(Some(FIXTURE_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut user_agent = None;
    // A request has a few headers; 100 is far more than reqwest sends.
    for _ in 0..100 {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        if header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("user-agent")
        {
            user_agent = Some(value.trim().to_owned());
        }
    }
    let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
    log.lock().unwrap().push(Seen {
        line: line.trim().to_owned(),
        user_agent,
    });
    let (status, body) = routes
        .iter()
        .find(|(prefix, _, _)| path.starts_with(prefix))
        .map_or((404, Vec::new()), |(_, status, body)| {
            (*status, body.clone())
        });
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut stream = stream;
    stream.write_all(head.as_bytes())?;
    stream.write_all(&body)
}

const LIST: &str = r#"{"data": [
  {"id": "catppuccin", "name": "Catppuccin", "version": "0.2.27",
   "description": "Soothing pastel theme", "authors": ["Catppuccin Org <hi@example.org>"],
   "repository": "https://github.com/catppuccin/zed", "schema_version": 1,
   "wasm_api_version": null, "provides": ["themes"],
   "published_at": "2026-01-01T00:00:00Z", "download_count": 123456},
  {"id": "nord", "name": "Nord", "version": "1.0.0", "authors": [],
   "repository": "https://github.com/x/nord", "provides": ["themes"],
   "published_at": "2026-01-01T00:00:00Z", "download_count": 9}
]}"#;

const THEME_JSON: &str = r##"{"name": "Catppuccin", "author": "Catppuccin Org",
  "themes": [{"name": "Catppuccin Mocha", "appearance": "dark",
              "style": {"background": "#1e1e2e"}}]}"##;

/// A `.tar.gz` holding `files`, laid out as Zed's archives are (flat, with
/// a leading `./`).
fn archive(files: &[(&str, &str)]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    for (path, text) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(text.len() as u64);
        header.set_mode(0o644);
        // Raw name bytes, so a test can plant a `..` path that the tar
        // crate's own setters refuse.
        let name = format!("./{path}");
        header.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name.as_bytes());
        header.set_cksum();
        tar.append(&header, text.as_bytes()).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

fn catppuccin() -> ThemeExtension {
    ThemeExtension {
        id: "catppuccin".into(),
        name: "Catppuccin".into(),
        version: "0.2.27".into(),
        description: None,
        authors: vec!["Catppuccin Org <hi@example.org>".into()],
        repository: Some("https://github.com/catppuccin/zed".into()),
        download_count: 0,
    }
}

const MANIFEST_TOML: &str = r#"id = "catppuccin"
name = "Catppuccin"
version = "0.2.27"
authors = ["Catppuccin Org <hi@example.org>"]
repository = "https://github.com/catppuccin/zed"
"#;

#[test]
fn marketplace_list_parses_and_caches_an_hour() {
    let server = Fixture::serve(3, vec![("/extensions", 200, LIST.into())]);
    let mut market = Marketplace::new(&server.base, &server.base).unwrap();
    let t0 = Instant::now();

    let list = market.search("cat", t0).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].id, "catppuccin");
    assert_eq!(list[0].version, "0.2.27");
    assert_eq!(list[0].authors, ["Catppuccin Org <hi@example.org>"]);
    assert_eq!(
        list[0].repository.as_deref(),
        Some("https://github.com/catppuccin/zed")
    );
    assert_eq!(list[0].download_count, 123_456);
    assert_eq!(list[1].description, None);

    let seen = server.requests();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].line.starts_with("GET /extensions?"),
        "{:?}",
        seen[0]
    );
    assert!(seen[0].line.contains("provides=themes"));
    assert!(seen[0].line.contains("max_schema_version=1"));
    assert!(seen[0].line.contains("filter=cat"));
    assert_eq!(seen[0].user_agent.as_deref(), Some(USER_AGENT));
    assert!(USER_AGENT.starts_with("agmen-desktop/"));

    // Under an hour: the cache answers.
    let again = market
        .search("cat", t0 + Duration::from_secs(59 * 60))
        .unwrap();
    assert_eq!(again, list);
    assert_eq!(server.requests().len(), 1);

    // Another query is its own entry.
    market.search("nord", t0).unwrap();
    assert_eq!(server.requests().len(), 2);

    // Past the hour: asked again.
    market
        .search("cat", t0 + Duration::from_secs(61 * 60))
        .unwrap();
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn marketplace_backs_off_after_errors() {
    let server = Fixture::serve(15, vec![("/extensions", 503, Vec::new())]);
    let mut market = Marketplace::new(&server.base, &server.base).unwrap();
    let t0 = Instant::now();
    let min = |m: u64| Duration::from_secs(m * 60);

    assert!(matches!(
        market.search("", t0),
        Err(MarketError::Status { status: 503, .. })
    ));
    // 1 minute, then 2, then 4: no request while waiting.
    assert!(matches!(
        market.search("", t0 + Duration::from_secs(30)),
        Err(MarketError::BackingOff { minutes: 1 })
    ));
    assert_eq!(server.requests().len(), 1);
    market.search("", t0 + min(1)).unwrap_err();
    assert!(matches!(
        market.search("", t0 + min(2)),
        Err(MarketError::BackingOff { minutes: 1 })
    ));
    market.search("", t0 + min(3)).unwrap_err();
    assert!(matches!(
        market.search("", t0 + min(4)),
        Err(MarketError::BackingOff { minutes: 3 })
    ));
    assert_eq!(server.requests().len(), 3);

    // Never longer than an hour, however many failures.
    let mut now = t0 + min(7);
    for _ in 0..12 {
        market.search("", now).unwrap_err();
        now += min(61);
    }
    let waited = market
        .retry_at
        .unwrap()
        .saturating_duration_since(now - min(61));
    assert_eq!(waited, min(60));
}

#[test]
fn unparsable_list_backs_off() {
    // Review C208: an HTML error page sent with status 200.
    let server = Fixture::serve(1, vec![("/extensions", 200, b"<html>busy</html>".to_vec())]);
    let mut market = Marketplace::new(&server.base, &server.base).unwrap();
    let t0 = Instant::now();

    assert!(matches!(
        market.search("", t0),
        Err(MarketError::Parse { .. })
    ));
    assert!(matches!(
        market.search("", t0 + Duration::from_secs(30)),
        Err(MarketError::BackingOff { minutes: 1 })
    ));
    assert_eq!(server.requests().len(), 1);
}

/// An archive with a manifest and a theme but no license file, as Zed's
/// archives are.
fn license_less_archive(manifest: &str) -> Vec<u8> {
    archive(&[
        ("extension.toml", manifest),
        ("themes/catppuccin-mocha.json", THEME_JSON),
    ])
}

/// GitHub's answer to `/repos/{owner}/{repo}/license` for `text`, with the
/// content wrapped at 60 characters as GitHub sends it.
fn license_json(text: &[u8]) -> Vec<u8> {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let wrapped = encoded
        .as_bytes()
        .chunks(60)
        .map(|chunk| std::str::from_utf8(chunk).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::json!({
        "name": "LICENSE",
        "path": "LICENSE",
        "encoding": "base64",
        "content": wrapped,
        "license": {"key": "mit", "name": "MIT License", "spdx_id": "MIT"},
    })
    .to_string()
    .into_bytes()
}

const MIT: &str = "MIT License\n\nCopyright (c) Catppuccin\n";

#[test]
fn install_saves_the_repository_license_when_the_archive_has_none() {
    // Andy, 2026-10-26: replaces install_refuses_theme_without_license.
    let server = Fixture::serve(
        2,
        vec![
            (
                "/extensions/catppuccin/0.2.27/download",
                200,
                license_less_archive(MANIFEST_TOML),
            ),
            (
                "/repos/catppuccin/zed/license",
                200,
                license_json(MIT.as_bytes()),
            ),
        ],
    );
    let mut market = Marketplace::new(&server.base, &server.base).unwrap();
    let home = tempfile::tempdir().unwrap();
    let themes = home.path().join("agmen/themes");

    let installed = market
        .install(&catppuccin(), &themes, Instant::now())
        .unwrap();
    let root = themes.join("catppuccin");
    assert_eq!(installed.licenses, [root.join("LICENSE")]);
    assert_eq!(installed.license.as_deref(), Some("MIT License"));
    assert_eq!(fs::read_to_string(root.join("LICENSE")).unwrap(), MIT);
    assert!(root.join("themes/catppuccin-mocha.json").is_file());
    let seen = server.requests();
    assert_eq!(
        seen[0].line,
        "GET /extensions/catppuccin/0.2.27/download HTTP/1.1"
    );
    assert_eq!(seen[1].line, "GET /repos/catppuccin/zed/license HTTP/1.1");
}

#[test]
fn license_request_carries_agmen_user_agent() {
    let server = Fixture::serve(
        2,
        vec![
            (
                "/extensions/catppuccin/",
                200,
                license_less_archive(MANIFEST_TOML),
            ),
            ("/repos/", 200, license_json(MIT.as_bytes())),
        ],
    );
    let mut market = Marketplace::new(&server.base, &server.base).unwrap();
    let home = tempfile::tempdir().unwrap();

    market
        .install(&catppuccin(), home.path(), Instant::now())
        .unwrap();
    let seen = server.requests();
    assert_eq!(seen.len(), 2);
    assert!(seen[1].line.starts_with("GET /repos/"), "{:?}", seen[1]);
    assert_eq!(seen[1].user_agent.as_deref(), Some(USER_AGENT));
}

#[test]
fn install_without_any_license_marks_it_not_found() {
    // The fixture answers the license request with 404.
    let server = Fixture::serve(
        2,
        vec![(
            "/extensions/catppuccin/",
            200,
            license_less_archive(MANIFEST_TOML),
        )],
    );
    let mut market = Marketplace::new(&server.base, &server.base).unwrap();
    let home = tempfile::tempdir().unwrap();
    let dirs = theme_dirs(&home.path().join(".config"), &home.path().join("no-zed"));

    let installed = market
        .install(&catppuccin(), &dirs.agmen, Instant::now())
        .unwrap();
    assert!(installed.licenses.is_empty(), "{installed:?}");
    assert_eq!(installed.license, None);
    assert_eq!(installed.authors, ["Catppuccin Org <hi@example.org>"]);
    assert_eq!(
        installed.repository.as_deref(),
        Some("https://github.com/catppuccin/zed")
    );
    assert_eq!(
        server.requests()[1].line,
        "GET /repos/catppuccin/zed/license HTTP/1.1"
    );
    // A 404 is about this repository, not an outage of Zed's API.
    assert_eq!(market.retry_at, None);

    let (themes, _) = installed_themes(&dirs);
    let entry = themes
        .iter()
        .find(|t| t.name == "Catppuccin Mocha")
        .unwrap();
    assert!(matches!(&entry.origin, ThemeOrigin::Agmen(t) if t == &installed));
}

#[test]
fn github_outage_does_not_back_off() {
    let server = Fixture::serve(
        2,
        vec![
            (
                "/extensions/catppuccin/",
                200,
                license_less_archive(MANIFEST_TOML),
            ),
            ("/repos/", 503, Vec::new()),
        ],
    );
    let mut market = Marketplace::new(&server.base, &server.base).unwrap();
    let home = tempfile::tempdir().unwrap();

    let installed = market
        .install(&catppuccin(), home.path(), Instant::now())
        .unwrap();
    assert!(installed.licenses.is_empty(), "{installed:?}");
    assert_eq!(market.retry_at, None);
    assert_eq!(market.failures, 0);
}

#[test]
fn non_github_repository_installs_without_a_request() {
    let manifest = MANIFEST_TOML.replace(
        "https://github.com/catppuccin/zed",
        "https://gitlab.com/catppuccin/zed",
    );
    let server = Fixture::serve(
        1,
        vec![(
            "/extensions/catppuccin/",
            200,
            license_less_archive(&manifest),
        )],
    );
    // A listener nobody accepts on: a request would sit in its backlog.
    let github = TcpListener::bind("127.0.0.1:0").unwrap();
    github.set_nonblocking(true).unwrap();
    let github_base = format!("http://{}", github.local_addr().unwrap());
    let mut market = Marketplace::new(&server.base, github_base).unwrap();
    let home = tempfile::tempdir().unwrap();
    let ext = ThemeExtension {
        repository: Some("https://gitlab.com/catppuccin/zed".into()),
        ..catppuccin()
    };

    let installed = market.install(&ext, home.path(), Instant::now()).unwrap();
    assert!(installed.licenses.is_empty(), "{installed:?}");
    assert_eq!(
        installed.repository.as_deref(),
        Some("https://gitlab.com/catppuccin/zed")
    );
    assert!(matches!(
        github.accept(),
        Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock
    ));
}

#[test]
fn oversized_repo_license_is_not_saved() {
    let big = vec![b'x'; MAX_LICENSE_BYTES + 1];
    let server = Fixture::serve(
        2,
        vec![
            (
                "/extensions/catppuccin/",
                200,
                license_less_archive(MANIFEST_TOML),
            ),
            ("/repos/", 200, license_json(&big)),
        ],
    );
    let mut market = Marketplace::new(&server.base, &server.base).unwrap();
    let home = tempfile::tempdir().unwrap();

    let installed = market
        .install(&catppuccin(), home.path(), Instant::now())
        .unwrap();
    assert!(installed.licenses.is_empty(), "{installed:?}");
    assert!(!home.path().join("catppuccin/LICENSE").exists());
}

#[test]
fn github_repo_accepts_only_plain_github_urls() {
    let parsed = |url: &str| github_repo(url).map(|r| (r.owner, r.repo));
    let pair = |o: &str, r: &str| Some((o.to_owned(), r.to_owned()));
    assert_eq!(
        parsed("https://github.com/catppuccin/zed"),
        pair("catppuccin", "zed")
    );
    assert_eq!(
        parsed("https://github.com/a-b/c.d_e.git"),
        pair("a-b", "c.d_e")
    );
    for bad in [
        "http://github.com/catppuccin/zed",
        "https://gitlab.com/catppuccin/zed",
        "https://github.com.evil.example/catppuccin/zed",
        "https://GitHub.com/catppuccin/zed",
        "https://github.com/catppuccin",
        "https://github.com/catppuccin/",
        "https://github.com/catppuccin/zed/",
        "https://github.com/catppuccin/zed/tree/main",
        "https://github.com/catppuccin/zed?x=1",
        "https://github.com/catppuccin/zed#readme",
        "https://github.com/../zed",
        "https://github.com/catppuccin/..",
        "https://github.com/catppuccin/.git",
        "https://github.com/-x/zed",
        "https://user@github.com/catppuccin/zed",
        "https://github.com/cat%2fppuccin/zed",
        "",
    ] {
        assert_eq!(parsed(bad), None, "{bad}");
    }
}

#[test]
fn installed_theme_keeps_license_and_author() {
    let bytes = archive(&[
        ("extension.toml", MANIFEST_TOML),
        ("LICENSE", "MIT License\n\nCopyright (c) Catppuccin\n"),
        ("themes/catppuccin-mocha.json", THEME_JSON),
        // Review C205: entries with `..` are skipped, not flattened.
        ("themes/../../escape.json", THEME_JSON),
        ("../LICENSE-ESCAPE", "Not this one\n"),
        ("grammars/x.wasm", "not kept"),
    ]);
    let server = Fixture::serve(1, vec![("/extensions/catppuccin/", 200, bytes)]);
    let mut market = Marketplace::new(&server.base, &server.base).unwrap();
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".config");
    let dirs = theme_dirs(&config, &home.path().join("no-zed"));

    let installed = market
        .install(&catppuccin(), &dirs.agmen, Instant::now())
        .unwrap();
    let root = dirs.agmen.join("catppuccin");
    assert_eq!(installed.id, "catppuccin");
    assert_eq!(installed.authors, ["Catppuccin Org <hi@example.org>"]);
    assert_eq!(installed.license.as_deref(), Some("MIT License"));
    assert_eq!(installed.licenses, [root.join("LICENSE")]);
    assert_eq!(
        fs::read_to_string(root.join("LICENSE")).unwrap(),
        "MIT License\n\nCopyright (c) Catppuccin\n"
    );
    assert!(root.join("themes/catppuccin-mocha.json").is_file());
    assert!(!root.join("grammars").exists());
    assert!(!root.join("themes/escape.json").exists());
    assert!(!root.join("LICENSE-ESCAPE").exists());
    assert!(!dirs.agmen.join(".catppuccin.partial").exists());

    // The picker shows it with its author and license, and it loads.
    let (themes, _) = installed_themes(&dirs);
    let entry = themes
        .iter()
        .find(|t| t.name == "Catppuccin Mocha")
        .unwrap();
    assert_eq!(entry.author.as_deref(), Some("Catppuccin Org"));
    assert_eq!(entry.appearance, Some(crate::appearance::SystemMode::Dark));
    assert!(matches!(&entry.origin, ThemeOrigin::Agmen(t) if t == &installed));
    let theme = super::super::theme::load_theme_with(
        &dirs,
        "Catppuccin Mocha",
        &serde_json::Map::new(),
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(
        theme.background,
        gpui::Rgba::try_from("#1e1e2e").unwrap().into()
    );
}

#[test]
fn failed_install_leaves_no_staging_folder() {
    // Review C204: a theme file name longer than the file system allows
    // makes a write fail after the staging folder exists.
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    let long = format!("themes/{}.json", "a".repeat(300));
    for (path, text) in [
        ("extension.toml", MANIFEST_TOML),
        ("LICENSE", "MIT License\n"),
        (long.as_str(), THEME_JSON),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(text.len() as u64);
        header.set_mode(0o644);
        tar.append_data(&mut header, path, text.as_bytes()).unwrap();
    }
    let bytes = tar.into_inner().unwrap().finish().unwrap();
    let server = Fixture::serve(1, vec![("/extensions/catppuccin/", 200, bytes)]);
    let mut market = Marketplace::new(&server.base, &server.base).unwrap();
    let home = tempfile::tempdir().unwrap();
    let dirs = theme_dirs(&home.path().join(".config"), &home.path().join("no-zed"));

    let err = market
        .install(&catppuccin(), &dirs.agmen, Instant::now())
        .unwrap_err();
    assert!(matches!(err, MarketError::Io { .. }), "{err}");
    assert!(!dirs.agmen.join(".catppuccin.partial").exists());
    assert!(!dirs.agmen.join("catppuccin").exists());
    let (themes, _) = installed_themes(&dirs);
    assert!(!themes.iter().any(|t| t.name == "Catppuccin Mocha"));
}

#[test]
fn dot_folders_in_agmen_themes_are_not_listed() {
    // Review C204: a staging folder left by an older agmen is not a theme.
    let home = tempfile::tempdir().unwrap();
    let dirs = theme_dirs(&home.path().join(".config"), &home.path().join("no-zed"));
    let staging = dirs.agmen.join(".catppuccin.partial/themes");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("catppuccin-mocha.json"), THEME_JSON).unwrap();

    let (themes, skipped) = installed_themes(&dirs);
    assert!(!themes.iter().any(|t| t.name == "Catppuccin Mocha"));
    assert!(skipped.is_empty(), "{skipped:?}");
}

#[test]
fn install_refuses_ids_that_leave_the_folder() {
    let mut market = Marketplace::new("http://127.0.0.1:9", "http://127.0.0.1:9").unwrap();
    let home = tempfile::tempdir().unwrap();
    for id in ["../evil", "a/b", "", ".hidden", "UPPER"] {
        let ext = ThemeExtension {
            id: id.into(),
            ..catppuccin()
        };
        assert!(matches!(
            market.install(&ext, home.path(), Instant::now()),
            Err(MarketError::BadId(_))
        ));
    }
    let ext = ThemeExtension {
        version: "../1".into(),
        ..catppuccin()
    };
    assert!(matches!(
        market.install(&ext, home.path(), Instant::now()),
        Err(MarketError::BadId(_))
    ));
}

#[test]
fn picker_lists_installed_before_marketplace() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".config");
    fs::create_dir_all(config.join("zed/themes")).unwrap();
    fs::write(config.join("zed/themes/mine.json"), THEME_JSON).unwrap();
    let extensions = home.path().join("installed");
    fs::create_dir_all(extensions.join("nord/themes")).unwrap();
    fs::write(
        extensions.join("nord/themes/nord.json"),
        r#"{"themes": [{"name": "Nord", "appearance": "dark", "style": {}}]}"#,
    )
    .unwrap();
    let dirs = theme_dirs(&config, &extensions);
    let (installed, skipped) = installed_themes(&dirs);
    assert!(skipped.is_empty(), "{skipped:?}");
    let market: Vec<ThemeExtension> = serde_json::from_str::<ListAnswer>(LIST).unwrap().data;

    let entries = picker_entries(&installed, &market);
    let names: Vec<String> = entries
        .iter()
        .map(|e| match e {
            PickerEntry::Installed(t) => format!("installed {}", t.name),
            PickerEntry::Marketplace(x) => format!("market {}", x.id),
        })
        .collect();
    assert_eq!(
        names,
        [
            format!("installed {}", crate::appearance::BUILTIN_THEME),
            "installed Wallpaper Adaptive (Balanced)".to_owned(),
            "installed Catppuccin Mocha".to_owned(),
            "installed Nord".to_owned(),
            // nord is installed in Zed, so only catppuccin is offered.
            "market catppuccin".to_owned(),
        ]
    );
}

#[test]
fn index_json_adds_themes_outside_the_themes_folder() {
    let home = tempfile::tempdir().unwrap();
    let extensions = home.path().join("extensions/installed");
    fs::create_dir_all(extensions.join("odd/colors")).unwrap();
    fs::write(extensions.join("odd/colors/odd.json"), THEME_JSON).unwrap();
    fs::write(
        home.path().join("extensions/index.json"),
        r#"{"themes": {
            "Catppuccin Mocha": {"extension": "odd", "path": "colors/odd.json"},
            "Escape": {"extension": "..", "path": "../../etc/passwd"}
        }}"#,
    )
    .unwrap();
    let dirs = theme_dirs(&home.path().join(".config"), &extensions);
    let (installed, _) = installed_themes(&dirs);
    let entry = installed
        .iter()
        .find(|t| t.name == "Catppuccin Mocha")
        .unwrap();
    assert!(
        matches!(&entry.origin, ThemeOrigin::ZedExtension(t) if t.id == "odd"),
        "{:?}",
        entry.origin
    );
}

#[test]
fn failed_read_back_names_the_manifest() {
    // Review C207: a read-back failure is not a missing license.
    let home = tempfile::tempdir().unwrap();
    let target = home.path().join("catppuccin");
    fs::create_dir_all(&target).unwrap();

    let err = read_back(&target).unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains(&target.join(MANIFEST).display().to_string()),
        "{text}"
    );
    assert!(!text.contains("license"), "{text}");
}

#[test]
fn unparsable_manifest_is_replaced_from_the_api() {
    // Review C206: a manifest that does not parse would read back with no
    // author or repository; agmen writes one from the API's answer instead.
    let bytes = archive(&[
        ("extension.toml", "authors = [not toml"),
        ("LICENSE", "MIT License\n"),
        ("themes/catppuccin-mocha.json", THEME_JSON),
    ]);
    let server = Fixture::serve(1, vec![("/extensions/catppuccin/", 200, bytes)]);
    let mut market = Marketplace::new(&server.base, &server.base).unwrap();
    let home = tempfile::tempdir().unwrap();
    let themes = home.path().join("agmen/themes");

    let installed = market
        .install(&catppuccin(), &themes, Instant::now())
        .unwrap();
    assert_eq!(installed.authors, ["Catppuccin Org <hi@example.org>"]);
    assert_eq!(
        installed.repository.as_deref(),
        Some("https://github.com/catppuccin/zed")
    );
    assert_eq!(installed.version.as_deref(), Some("0.2.27"));
}

#[test]
#[should_panic(expected = "fixture server")]
fn fixture_reports_missing_requests() {
    // Review C235: a test that makes fewer requests than it promised fails.
    let fixture = Fixture::serve(1, Vec::new());
    drop(fixture);
}

#[test]
fn unreadable_manifest_is_reported() {
    // Review C236: a manifest that does not parse keeps the install listed
    // and says why its author and repository are missing.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("broken");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("extension.toml"), "name = [unclosed").unwrap();
    let read = read_installed(&root).unwrap();
    assert!(read.authors.is_empty());
    assert!(read.manifest_error.is_some(), "{read:?}");
}
