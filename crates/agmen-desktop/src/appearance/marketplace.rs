//! Themes from Zed's extensions, through `api.zed.dev` (Andy, 2026-10-26).
//!
//! Only GET requests (the API answers HEAD with 404), with a User-Agent
//! naming agmen. Lists are cached for an hour; failures back off 1, 2, 4 ...
//! up to 60 minutes. An install copies the theme files, the extension's
//! license and its author into agmen's own `~/.config/agmen/themes/<id>/`,
//! never into Zed's folders. Zed's archives carry no license file, so an
//! install without one asks GitHub for the repository's license; when none
//! is found it installs anyway, and the picker says "license not found"
//! (Andy, 2026-10-26). That request runs without the [`Marketplace`] lock
//! (see [`install_shared`]), follows redirects only on its first origin and
//! sends no Referer (C304, C305).

use std::collections::HashMap;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use super::theme::Family;
use super::zed::parse_jsonc;

/// Zed's extension API.
pub(crate) const API_BASE: &str = "https://api.zed.dev";
/// Where the picker sends people to see the extensions themselves.
pub(crate) const EXTENSIONS_PAGE: &str = "https://zed.dev/extensions";
/// GitHub's REST API, asked for a repository's license.
pub(crate) const GITHUB_API: &str = "https://api.github.com";
/// Sent with every request so Zed can tell agmen's traffic apart.
pub(crate) const USER_AGENT: &str = concat!("agmen-desktop/", env!("CARGO_PKG_VERSION"));

/// How long a list answer is reused.
const LIST_TTL: Duration = Duration::from_secs(60 * 60);
/// The first wait after a failure; each further failure doubles it.
const FIRST_BACKOFF: Duration = Duration::from_secs(60);
/// The longest wait after failures.
const MAX_BACKOFF: Duration = Duration::from_secs(60 * 60);
/// How many queries the list cache holds; the oldest goes first.
const MAX_CACHED_QUERIES: usize = 32;
/// Every request gives up after this long. It is set on each request as
/// well as on the clients: in blocking reqwest, a client's timeout bounds
/// each read, so a slow drip would never end; a request's bounds the whole
/// answer (C308).
const TIMEOUT: Duration = Duration::from_secs(20);
/// The full theme list was 259,792 bytes in October 2026.
const MAX_LIST_BYTES: u64 = 8 * 1024 * 1024;
/// A theme archive, compressed (catppuccin's was 14,948 bytes).
const MAX_ARCHIVE_BYTES: u64 = 16 * 1024 * 1024;
/// A theme archive, unpacked.
const MAX_UNPACKED_BYTES: u64 = 32 * 1024 * 1024;
/// Entries read from one archive at most.
const MAX_ARCHIVE_ENTRIES: usize = 4_096;
/// The longest extension id accepted.
const MAX_ID_LEN: usize = 128;
/// A repository license, decoded (GPL-3.0's text is about 35 KB).
const MAX_LICENSE_BYTES: usize = 128 * 1024;
/// GitHub's license answer: the base64 text, wrapped, plus metadata.
const MAX_LICENSE_ANSWER_BYTES: u64 = 512 * 1024;
/// The license request gives up after this long; the install goes on.
const LICENSE_TIMEOUT: Duration = Duration::from_secs(10);
/// The file name a repository license is saved under.
const REPO_LICENSE_FILE: &str = "LICENSE";
/// GitHub's limits: owners up to 39 characters, repositories up to 100.
const MAX_GITHUB_OWNER_LEN: usize = 39;
const MAX_GITHUB_REPO_LEN: usize = 100;
/// Redirects the license request follows at most (GitHub sends one 301 for
/// a renamed repository).
const MAX_LICENSE_REDIRECTS: usize = 5;
/// The longest license label shown, in characters, ellipsis included.
const MAX_LICENSE_LABEL_CHARS: usize = 80;
/// The `extension.toml` key that keeps the license label GitHub detected.
const LICENSE_KEY: &str = "license";

/// The manifest file of an installed theme extension.
const MANIFEST: &str = "extension.toml";
/// The folder its theme files go in.
const THEMES_DIR: &str = "themes";

/// What can go wrong talking to Zed's API or installing a theme.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MarketError {
    #[error("Zed's theme extensions are unreachable; agmen tries again in {minutes} min")]
    BackingOff { minutes: u64 },
    #[error("cannot reach {url}: {message}")]
    Http { url: String, message: String },
    #[error("{url} answered HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("{url} sent more than {limit} bytes")]
    TooLarge { url: String, limit: u64 },
    #[error("cannot parse the answer from {url}: {message}")]
    Parse { url: String, message: String },
    #[error("\"{0}\" is not an extension id agmen accepts")]
    BadId(String),
    #[error("{id} ships no theme files")]
    NoThemes { id: String },
    #[error("cannot unpack {id}: {message}")]
    Archive { id: String, message: String },
    #[error("cannot read {} back after installing: {message}", path.display())]
    ReadBack { path: PathBuf, message: String },
    #[error("the theme list is unavailable after an earlier crash")]
    Poisoned,
    #[error("cannot write {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl MarketError {
    /// Whether the failure says the API is unreachable or broken, as opposed
    /// to an answer about one extension (a 404). A list
    /// answer that does not parse counts: an error page sent with status 200
    /// or a changed schema would fail the same way on every retry (C208).
    const fn counts_as_outage(&self) -> bool {
        match self {
            Self::Http { .. } | Self::TooLarge { .. } | Self::Parse { .. } => true,
            Self::Status { status, .. } => *status >= 500 || *status == 429,
            _ => false,
        }
    }
}

/// One theme extension in the API's list.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct ThemeExtension {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) authors: Vec<String>,
    #[serde(default)]
    pub(crate) repository: Option<String>,
    #[serde(default)]
    pub(crate) download_count: u64,
}

#[derive(Deserialize)]
struct ListAnswer {
    data: Vec<ThemeExtension>,
}

/// The `extension.toml` keys agmen reads back from an install.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Manifest {
    name: Option<String>,
    version: Option<String>,
    authors: Vec<String>,
    repository: Option<String>,
    /// The license label agmen kept at install (C303). Any TOML value, so a
    /// manifest whose `license` is not a string still parses; only a string
    /// is shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    license: Option<toml::Value>,
}

/// A theme extension installed by agmen or by Zed, as read back from its
/// folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InstalledTheme {
    /// The extension id, which is also the folder name.
    pub(crate) id: String,
    pub(crate) name: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) authors: Vec<String>,
    pub(crate) repository: Option<String>,
    /// The license files kept with it.
    pub(crate) licenses: Vec<PathBuf>,
    /// The license label: GitHub's SPDX id or name as kept in the manifest,
    /// else the first line of the first license file, without control or
    /// bidi characters and at most [`MAX_LICENSE_LABEL_CHARS`] long. `None`
    /// with `licenses` not empty means the file has no readable line.
    pub(crate) license: Option<String>,
    /// Why the manifest could not be read, when it could not; the install
    /// is still listed, without author or repository (C236).
    pub(crate) manifest_error: Option<String>,
}

/// The API client, with its list cache and backoff. Calls block, so the
/// app runs them on the background executor.
#[derive(Debug)]
pub(crate) struct Marketplace {
    base: String,
    github: String,
    client: reqwest::blocking::Client,
    /// The client for GitHub: redirects only on the first origin, no
    /// Referer (C304).
    github_client: reqwest::blocking::Client,
    cache: HashMap<String, (Instant, Vec<ThemeExtension>)>,
    failures: u32,
    retry_at: Option<Instant>,
    /// [`LICENSE_TIMEOUT`]; tests shorten it.
    license_timeout: Duration,
    /// [`TIMEOUT`]; tests shorten it.
    timeout: Duration,
}

impl Marketplace {
    /// A client for the API at `base` that asks `github` for repository
    /// licenses: [`API_BASE`] and [`GITHUB_API`] in the app, a local fixture
    /// in tests.
    ///
    /// # Errors
    ///
    /// When the HTTP client cannot be built (no TLS backend).
    pub(crate) fn new(
        base: impl Into<String>,
        github: impl Into<String>,
    ) -> Result<Self, MarketError> {
        let base = base.into();
        let github = github.into();
        let unbuilt = |url: &str| {
            let url = url.to_owned();
            move |err: reqwest::Error| MarketError::Http {
                url,
                message: err.to_string(),
            }
        };
        let client = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(TIMEOUT)
            .build()
            .map_err(unbuilt(&base))?;
        let github_client = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if follows_license_redirect(attempt.url(), attempt.previous()) {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .referer(false)
            .build()
            .map_err(unbuilt(&github))?;
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            github: github.trim_end_matches('/').to_owned(),
            client,
            github_client,
            cache: HashMap::new(),
            failures: 0,
            retry_at: None,
            license_timeout: LICENSE_TIMEOUT,
            timeout: TIMEOUT,
        })
    }

    /// Theme extensions whose name matches `query` (all of them for an empty
    /// query), from the cache when it is under an hour old at `now`.
    ///
    /// # Errors
    ///
    /// While backing off, or when the request fails or the answer does not
    /// parse.
    pub(crate) fn search(
        &mut self,
        query: &str,
        now: Instant,
    ) -> Result<Vec<ThemeExtension>, MarketError> {
        let query = query.trim().to_owned();
        if let Some((at, list)) = self.cache.get(&query)
            && now.saturating_duration_since(*at) < LIST_TTL
        {
            return Ok(list.clone());
        }
        let url = reqwest::Url::parse_with_params(
            &format!("{}/extensions", self.base),
            &[
                ("max_schema_version", "1"),
                ("provides", "themes"),
                ("filter", query.as_str()),
            ],
        )
        .map_err(|err| MarketError::Parse {
            url: self.base.clone(),
            message: err.to_string(),
        })?;
        let list_url = format!("{}/extensions", self.base);
        let answer = self.get(url, MAX_LIST_BYTES, now).and_then(|body| {
            serde_json::from_slice::<ListAnswer>(&body).map_err(|err| MarketError::Parse {
                url: list_url,
                message: err.to_string(),
            })
        });
        self.record(&answer, now);
        let answer = answer?;
        if self.cache.len() >= MAX_CACHED_QUERIES
            && let Some(oldest) = self
                .cache
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(key, _)| key.clone())
        {
            self.cache.remove(&oldest);
        }
        self.cache.insert(query, (now, answer.data.clone()));
        Ok(answer.data)
    }

    /// Downloads `ext` and keeps its theme files that parse, its license
    /// files and its manifest, ready for [`Download::write`]. An archive
    /// without a license file gets a GitHub request to make, for its
    /// repository when it names one there.
    ///
    /// # Errors
    ///
    /// While backing off; when the download fails; when the archive has no
    /// theme file that parses (before any GitHub request, C305).
    pub(crate) fn download(
        &mut self,
        ext: &ThemeExtension,
        now: Instant,
    ) -> Result<Download, MarketError> {
        check_id(&ext.id)?;
        let path_part = |s: &str| {
            s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
        };
        if ext.version.is_empty() || !path_part(&ext.version) {
            return Err(MarketError::BadId(format!("{} {}", ext.id, ext.version)));
        }
        let url = reqwest::Url::parse(&format!(
            "{}/extensions/{}/{}/download",
            self.base, ext.id, ext.version
        ))
        .map_err(|err| MarketError::Parse {
            url: self.base.clone(),
            message: err.to_string(),
        })?;
        let archive = self.get(url, MAX_ARCHIVE_BYTES, now);
        // An archive that does not unpack is about this extension, not the
        // API, so only the download itself counts toward the backoff.
        self.record(&archive, now);
        let mut unpacked = unpack(&ext.id, &archive?)?;
        keep_parsed_themes(&ext.id, &mut unpacked)?;
        let license = if unpacked.licenses.is_empty() {
            unpacked
                .manifest
                .as_deref()
                .and_then(parse_manifest)
                .and_then(|manifest| manifest.repository)
                .or_else(|| ext.repository.clone())
                .as_deref()
                .and_then(github_repo)
                .and_then(|repo| self.license_request(&repo))
        } else {
            None
        };
        Ok(Download {
            ext: ext.clone(),
            unpacked,
            license,
        })
    }

    /// The request for `repo`'s license on GitHub; `None` when its URL does
    /// not parse.
    fn license_request(&self, repo: &GithubRepo) -> Option<LicenseRequest> {
        let url = reqwest::Url::parse(&format!(
            "{}/repos/{}/{}/license",
            self.github, repo.owner, repo.repo
        ))
        .ok()?;
        Some(LicenseRequest {
            client: self.github_client.clone(),
            url,
            timeout: self.license_timeout,
        })
    }

    /// GETs `url`, at most `limit` bytes, unless backing off at `now`. The
    /// caller passes the outcome to [`Self::record`].
    fn get(&self, url: reqwest::Url, limit: u64, now: Instant) -> Result<Vec<u8>, MarketError> {
        if let Some(at) = self.retry_at
            && now < at
        {
            let wait = at.saturating_duration_since(now).as_secs();
            return Err(MarketError::BackingOff {
                minutes: wait.div_ceil(60).max(1),
            });
        }
        self.fetch(&url, limit)
    }

    /// Updates the backoff from the outcome of a request made at `now`.
    fn record<T>(&mut self, result: &Result<T, MarketError>, now: Instant) {
        match result {
            Ok(_) => {
                self.failures = 0;
                self.retry_at = None;
            }
            Err(err) if err.counts_as_outage() => {
                // 1, 2, 4 ... minutes, then 60 from the seventh failure on.
                let doubled = FIRST_BACKOFF.saturating_mul(1 << self.failures.min(6));
                self.retry_at = Some(now + doubled.min(MAX_BACKOFF));
                self.failures = self.failures.saturating_add(1);
            }
            // A 404, or a request not made while backing off, says nothing
            // about an outage.
            Err(_) => {}
        }
    }

    fn fetch(&self, url: &reqwest::Url, limit: u64) -> Result<Vec<u8>, MarketError> {
        read(
            self.client.get(url.clone()).timeout(self.timeout),
            url,
            limit,
        )
    }
}

/// Installs `ext` through the shared `market` into `agmen_themes/<id>/`,
/// replacing an older install of the same id. The lock is held for the
/// download from Zed and for the writes, but not for the GitHub request, so
/// searches and other installs never wait on GitHub (C305).
///
/// # Errors
///
/// As [`Marketplace::download`] and [`Download::write`], and when the lock
/// is poisoned.
pub(crate) fn install_shared(
    market: &Mutex<Marketplace>,
    ext: &ThemeExtension,
    agmen_themes: &Path,
    now: Instant,
) -> Result<InstalledTheme, MarketError> {
    let mut download = market
        .lock()
        .map_err(|_| MarketError::Poisoned)?
        .download(ext, now)?;
    download.fetch_license();
    // Writes stay under the lock, so two installs of one id never share a
    // staging folder.
    let _writing = market.lock().map_err(|_| MarketError::Poisoned)?;
    download.write(agmen_themes)
}

/// A downloaded extension, not yet written.
#[derive(Debug)]
pub(crate) struct Download {
    ext: ThemeExtension,
    unpacked: Unpacked,
    /// The GitHub request to make, when the archive has no license file.
    license: Option<LicenseRequest>,
}

impl Download {
    /// Asks GitHub for the repository license, when the archive had none,
    /// and keeps it with its label. Any failure leaves the install without
    /// one ("license not found"); it blocks for at most the license timeout.
    pub(crate) fn fetch_license(&mut self) {
        if let Some(found) = self.license.take().and_then(|request| request.fetch()) {
            self.unpacked
                .licenses
                .push((REPO_LICENSE_FILE.to_owned(), found.text));
            self.unpacked.license_label = found.label;
        }
    }

    /// Writes the install into `agmen_themes/<id>/`, replacing an older
    /// install of the same id.
    ///
    /// # Errors
    ///
    /// When writing or reading the install back fails.
    pub(crate) fn write(&self, agmen_themes: &Path) -> Result<InstalledTheme, MarketError> {
        write_install(&self.ext, &self.unpacked, agmen_themes)
    }
}

/// GitHub's `/repos/{owner}/{repo}/license`, ready to send.
#[derive(Debug)]
struct LicenseRequest {
    client: reqwest::blocking::Client,
    url: reqwest::Url,
    timeout: Duration,
}

/// A repository license from GitHub.
struct RepoLicense {
    text: Vec<u8>,
    /// GitHub's SPDX id, or its name when it has no SPDX id.
    label: Option<String>,
}

impl LicenseRequest {
    /// The license, decoded; `None` when the repository has none (404), when
    /// the request or its answer fails, or when the text is over
    /// [`MAX_LICENSE_BYTES`]. One request, without a token, and it never
    /// counts toward the backoff, which is about Zed's API.
    fn fetch(&self) -> Option<RepoLicense> {
        let request = self
            .client
            .get(self.url.clone())
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .timeout(self.timeout);
        // Each `.ok()?` here is deliberate: any failure (network, 404, 5xx,
        // too large, not JSON, not base64) means "license not found" for
        // this install, and the install goes on without one.
        let body = read(request, &self.url, MAX_LICENSE_ANSWER_BYTES).ok()?;
        let answer: LicenseAnswer = serde_json::from_slice(&body).ok()?;
        if answer.encoding != "base64" {
            return None;
        }
        // GitHub wraps the base64 in lines of 60 characters.
        let packed: Vec<u8> = answer
            .content
            .bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        // Four base64 characters carry three bytes: refuse before decoding.
        if packed.len() / 4 * 3 > MAX_LICENSE_BYTES + 3 {
            return None;
        }
        let text = base64::engine::general_purpose::STANDARD
            .decode(&packed)
            .ok()?;
        let label = answer.license.and_then(DetectedLicense::label);
        (!text.is_empty() && text.len() <= MAX_LICENSE_BYTES).then_some(RepoLicense { text, label })
    }
}

/// Whether the license request follows a redirect to `next`, after
/// requesting `previous` (the first request first): only to the scheme,
/// host and port of the first request, which is `https://api.github.com`
/// in the app, and at most [`MAX_LICENSE_REDIRECTS`] times (C304).
fn follows_license_redirect(next: &reqwest::Url, previous: &[reqwest::Url]) -> bool {
    previous.len() <= MAX_LICENSE_REDIRECTS
        && previous
            .first()
            .is_some_and(|first| first.origin() == next.origin())
}

/// Sends `request` for `url` and reads at most `limit` bytes of a
/// successful answer.
fn read(
    request: reqwest::blocking::RequestBuilder,
    url: &reqwest::Url,
    limit: u64,
) -> Result<Vec<u8>, MarketError> {
    let http = |err: reqwest::Error| MarketError::Http {
        url: url.to_string(),
        message: err.to_string(),
    };
    let response = request.send().map_err(http)?;
    let status = response.status();
    if !status.is_success() {
        return Err(MarketError::Status {
            url: url.to_string(),
            status: status.as_u16(),
        });
    }
    let mut body = Vec::new();
    response
        .take(limit.saturating_add(1))
        .read_to_end(&mut body)
        .map_err(|err| MarketError::Http {
            url: url.to_string(),
            message: err.to_string(),
        })?;
    if body.len() as u64 > limit {
        return Err(MarketError::TooLarge {
            url: url.to_string(),
            limit,
        });
    }
    Ok(body)
}

/// An extension id is a folder name: lowercase letters, digits, `-` and `_`.
fn check_id(id: &str) -> Result<(), MarketError> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && !id.starts_with(['-', '_'])
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'));
    if ok {
        Ok(())
    } else {
        Err(MarketError::BadId(id.to_owned()))
    }
}

/// A repository on GitHub, as `https://github.com/{owner}/{repo}` names it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GithubRepo {
    owner: String,
    repo: String,
}

/// Parses `https://github.com/{owner}/{repo}` with an optional `.git`, one
/// trailing slash, or a `/tree/{ref}/...` folder (C302), and nothing else:
/// no other host or scheme, no user, port, other path, or query. Only
/// GitHub's own name characters pass, so the parts go into the API URL as
/// they are; the folder part is dropped, never sent.
fn github_repo(url: &str) -> Option<GithubRepo> {
    let rest = url.strip_prefix("https://github.com/")?;
    let (owner, rest) = rest.split_once('/')?;
    let repo = match rest.split_once('/') {
        None | Some((_, "")) => rest.strip_suffix('/').unwrap_or(rest),
        Some((repo, tail)) => {
            let reference = tail.strip_prefix("tree/")?;
            if reference.is_empty() || reference.starts_with('/') {
                return None;
            }
            repo
        }
    };
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let owner_ok = !owner.is_empty()
        && owner.len() <= MAX_GITHUB_OWNER_LEN
        && !owner.starts_with('-')
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let repo_ok = !repo.is_empty()
        && repo.len() <= MAX_GITHUB_REPO_LEN
        && repo != "."
        && repo != ".."
        && repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    (owner_ok && repo_ok).then(|| GithubRepo {
        owner: owner.to_owned(),
        repo: repo.to_owned(),
    })
}

/// The keys agmen reads from GitHub's `/repos/{owner}/{repo}/license`.
#[derive(Deserialize)]
struct LicenseAnswer {
    content: String,
    encoding: String,
    #[serde(default)]
    license: Option<DetectedLicense>,
}

/// The license GitHub detected in the file.
#[derive(Deserialize)]
struct DetectedLicense {
    #[serde(default)]
    spdx_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

impl DetectedLicense {
    /// The SPDX id, or the name when GitHub has no SPDX id for it
    /// (`NOASSERTION`), cleaned as [`license_label`] does (C303).
    fn label(self) -> Option<String> {
        self.spdx_id
            .filter(|id| id != "NOASSERTION")
            .and_then(|id| license_label(&id))
            .or_else(|| self.name.as_deref().and_then(license_label))
    }
}

/// `line` as a license label: without control or bidi characters, trimmed,
/// and cut to [`MAX_LICENSE_LABEL_CHARS`] with an ellipsis; `None` when
/// nothing is left. The text is untrusted: a bidi override could reorder
/// the credit line, and one long line would make its row tall (C303).
fn license_label(line: &str) -> Option<String> {
    let bidi = |c: char| {
        matches!(
            c,
            '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
    };
    let clean: String = line
        .chars()
        .filter(|&c| !c.is_control() && !bidi(c))
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        return None;
    }
    if clean.chars().count() <= MAX_LICENSE_LABEL_CHARS {
        return Some(clean.to_owned());
    }
    let mut cut: String = clean.chars().take(MAX_LICENSE_LABEL_CHARS - 1).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('\u{2026}');
    Some(cut)
}

/// An `extension.toml` that parses, or `None`.
fn parse_manifest(bytes: &[u8]) -> Option<Manifest> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| toml::from_str(text).ok())
}

/// Whether a file at an extension's root is a license, by Zed's and
/// GitHub's usual names (`LICENSE`, `LICENSE-MIT`, `LICENCE.md`, `COPYING`).
fn is_license(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["LICENSE", "LICENCE", "COPYING", "UNLICENSE"]
        .iter()
        .any(|stem| upper.starts_with(stem))
}

/// The files of an archive agmen keeps.
#[derive(Debug, Default)]
struct Unpacked {
    manifest: Option<Vec<u8>>,
    /// File name and contents of each `themes/*.json`.
    themes: Vec<(String, Vec<u8>)>,
    /// File name and contents of each license file at the root.
    licenses: Vec<(String, Vec<u8>)>,
    /// The label GitHub gave the repository license, kept in the manifest.
    license_label: Option<String>,
}

/// Reads the `.tar.gz` in memory, keeping the manifest, `themes/*.json` and
/// root license files; nothing is written while reading, and paths are only
/// matched, never joined.
fn unpack(id: &str, archive: &[u8]) -> Result<Unpacked, MarketError> {
    let bad = |message: String| MarketError::Archive {
        id: id.to_owned(),
        message,
    };
    let gz = flate2::read::GzDecoder::new(archive).take(MAX_UNPACKED_BYTES);
    let mut tar = tar::Archive::new(gz);
    let mut out = Unpacked::default();
    let entries = tar.entries().map_err(|err| bad(err.to_string()))?;
    for (count, entry) in entries.enumerate() {
        if count >= MAX_ARCHIVE_ENTRIES {
            return Err(bad(format!("more than {MAX_ARCHIVE_ENTRIES} entries")));
        }
        let mut entry = entry.map_err(|err| bad(err.to_string()))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|err| bad(err.to_string()))?
            .into_owned();
        // `./` is how Zed's archives start; any other component (`..`, a
        // root, a name that is not UTF-8) skips the entry (C205).
        let mut parts = Vec::new();
        let mut plain = true;
        for component in path.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::Normal(part) => match part.to_str() {
                    Some(part) => parts.push(part.to_owned()),
                    None => plain = false,
                },
                _ => plain = false,
            }
        }
        let keep = match parts.as_slice() {
            _ if !plain => None,
            [name] if name == MANIFEST => Some(Kept::Manifest),
            [name] if is_license(name) => Some(Kept::License(name.clone())),
            [dir, name] if dir == THEMES_DIR && name.ends_with(".json") => {
                Some(Kept::Theme(name.clone()))
            }
            _ => None,
        };
        let Some(keep) = keep else { continue };
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|err| bad(err.to_string()))?;
        match keep {
            Kept::Manifest => out.manifest = Some(bytes),
            Kept::License(name) => out.licenses.push((name, bytes)),
            Kept::Theme(name) => out.themes.push((name, bytes)),
        }
    }
    Ok(out)
}

enum Kept {
    Manifest,
    License(String),
    Theme(String),
}

/// Keeps only the theme files of `unpacked` that parse as a Zed theme
/// family.
///
/// # Errors
///
/// [`MarketError::NoThemes`] when none does.
fn keep_parsed_themes(id: &str, unpacked: &mut Unpacked) -> Result<(), MarketError> {
    unpacked.themes.retain(|(name, bytes)| {
        std::str::from_utf8(bytes).is_ok_and(|text| {
            parse_jsonc::<Family>(text, Path::new(name)).is_ok_and(|f| !f.themes.is_empty())
        })
    });
    if unpacked.themes.is_empty() {
        return Err(MarketError::NoThemes { id: id.to_owned() });
    }
    Ok(())
}

/// `manifest` with its `license` key set to `label`.
fn with_license_key(manifest: &[u8], label: &str) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(manifest).map_err(|err| err.to_string())?;
    let mut table: toml::Table = toml::from_str(text).map_err(|err| err.to_string())?;
    table.insert(
        LICENSE_KEY.to_owned(),
        toml::Value::String(label.to_owned()),
    );
    toml::to_string(&table)
        .map(String::into_bytes)
        .map_err(|err| err.to_string())
}

/// Writes `unpacked`, whose themes all parse, to `agmen_themes/<id>/`
/// through a staging folder and a rename. A failed install removes the
/// staging folder and leaves any older install of the same id in place
/// (C204).
fn write_install(
    ext: &ThemeExtension,
    unpacked: &Unpacked,
    agmen_themes: &Path,
) -> Result<InstalledTheme, MarketError> {
    let id = ext.id.clone();
    let io = |path: &Path| {
        let path = path.to_owned();
        move |source| MarketError::Io { path, source }
    };
    let manifest = match &unpacked.manifest {
        Some(bytes) if parse_manifest(bytes).is_some() => bytes.clone(),
        // An archive without a manifest that parses still records who made
        // it, from the API's answer (C206).
        _ => toml::to_string(&Manifest {
            name: Some(ext.name.clone()),
            version: Some(ext.version.clone()),
            authors: ext.authors.clone(),
            repository: ext.repository.clone(),
            license: None,
        })
        .map_err(|err| MarketError::Archive {
            id: id.clone(),
            message: err.to_string(),
        })?
        .into_bytes(),
    };
    let manifest = match &unpacked.license_label {
        Some(label) => {
            with_license_key(&manifest, label).map_err(|message| MarketError::Archive {
                id: id.clone(),
                message,
            })?
        }
        None => manifest,
    };
    fs::create_dir_all(agmen_themes).map_err(io(agmen_themes))?;
    let staging = agmen_themes.join(format!(".{id}.partial"));
    let old = agmen_themes.join(format!(".{id}.old"));
    for leftover in [&staging, &old] {
        if leftover.exists() {
            fs::remove_dir_all(leftover).map_err(io(leftover))?;
        }
    }
    let target = agmen_themes.join(&id);
    let swapped = write_staging(&staging, &manifest, &unpacked.licenses, &unpacked.themes)
        .and_then(|()| {
            let had_old = target.exists();
            if had_old {
                fs::rename(&target, &old).map_err(io(&target))?;
            }
            fs::rename(&staging, &target)
                .map_err(io(&target))
                .inspect_err(|_| {
                    if had_old {
                        // Best effort: the rename error is what the person needs to
                        // see; a failure here leaves the old install at `.{id}.old`.
                        fs::rename(&old, &target).unwrap_or(());
                    }
                })
        });
    if let Err(err) = swapped {
        if staging.exists() {
            // Best effort: the write error matters more, and the next
            // install of this id removes a leftover first.
            fs::remove_dir_all(&staging).unwrap_or(());
        }
        return Err(err);
    }
    if old.exists() {
        // The new install is in place; a leftover `.{id}.old` is hidden
        // from the theme list and removed by the next install of this id.
        fs::remove_dir_all(&old).unwrap_or(());
    }
    read_back(&target)
}

/// Writes the manifest, licenses and theme files into `staging`.
fn write_staging(
    staging: &Path,
    manifest: &[u8],
    licenses: &[(String, Vec<u8>)],
    themes: &[(String, Vec<u8>)],
) -> Result<(), MarketError> {
    let write = |path: PathBuf, bytes: &[u8]| {
        fs::write(&path, bytes).map_err(|source| MarketError::Io { path, source })
    };
    let theme_dir = staging.join(THEMES_DIR);
    fs::create_dir_all(&theme_dir).map_err(|source| MarketError::Io {
        path: theme_dir.clone(),
        source,
    })?;
    write(staging.join(MANIFEST), manifest)?;
    for (name, bytes) in licenses {
        write(staging.join(name), bytes)?;
    }
    for (name, bytes) in themes {
        write(theme_dir.join(name), bytes)?;
    }
    Ok(())
}

/// Reads a fresh install back from `target`.
fn read_back(target: &Path) -> Result<InstalledTheme, MarketError> {
    read_installed(target).ok_or_else(|| MarketError::ReadBack {
        path: target.join(MANIFEST),
        message: "the manifest is missing or unreadable".to_owned(),
    })
}

/// Reads an install back from its folder; `None` when it is not one (no
/// manifest). A folder without a license file reads with `license: None`;
/// for agmen's own installs that means none was found (Andy, 2026-10-26).
/// A license file that is not UTF-8 or is blank reads with no label.
pub(crate) fn read_installed(root: &Path) -> Option<InstalledTheme> {
    let id = root.file_name()?.to_str()?.to_owned();
    if id.starts_with('.') {
        return None;
    }
    let text = fs::read_to_string(root.join(MANIFEST)).ok()?;
    // A manifest that does not parse still marks the folder as an install:
    // its themes stay listed, without author and repository, and the error
    // is kept to say why. agmen's own installs always parse (write_install
    // replaces one that does not), so this only reaches a Zed extension's
    // hand-edited manifest (C206, C236).
    let (manifest, manifest_error) = match toml::from_str::<Manifest>(&text) {
        Ok(manifest) => (manifest, None),
        Err(err) => (Manifest::default(), Some(err.message().to_owned())),
    };
    let mut licenses: Vec<PathBuf> = fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(is_license)
        })
        .collect();
    licenses.sort();
    // The label kept at install first, as GitHub named the license; else
    // the file's first line that has any text.
    let license = manifest
        .license
        .as_ref()
        .and_then(toml::Value::as_str)
        .and_then(license_label)
        .or_else(|| {
            licenses
                .first()
                .and_then(|path| fs::read_to_string(path).ok())
                .and_then(|text| text.lines().find_map(license_label))
        });
    Some(InstalledTheme {
        id,
        name: manifest.name,
        version: manifest.version,
        authors: manifest.authors,
        repository: manifest.repository,
        licenses,
        license,
        manifest_error,
    })
}

#[cfg(test)]
#[path = "marketplace_tests.rs"]
mod tests;
