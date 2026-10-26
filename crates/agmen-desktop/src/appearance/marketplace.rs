//! Themes from Zed's extensions, through `api.zed.dev` (Andy, 2026-10-26).
//!
//! Only GET requests (the API answers HEAD with 404), with a User-Agent
//! naming agmen. Lists are cached for an hour; failures back off 1, 2, 4 ...
//! up to 60 minutes. An install copies the theme files, the extension's
//! license and its author into agmen's own `~/.config/agmen/themes/<id>/`,
//! never into Zed's folders, and a theme with no license file is refused.

use std::collections::HashMap;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::theme::Family;
use super::zed::parse_jsonc;

/// Zed's extension API.
pub(crate) const API_BASE: &str = "https://api.zed.dev";
/// Where the picker sends people to see the extensions themselves.
pub(crate) const EXTENSIONS_PAGE: &str = "https://zed.dev/extensions";
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
/// Every request gives up after this long.
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
    #[error("{id} ships no license file, so agmen does not install it")]
    NoLicense { id: String },
    #[error("{id} ships no theme files")]
    NoThemes { id: String },
    #[error("cannot unpack {id}: {message}")]
    Archive { id: String, message: String },
    #[error("cannot read {} back after installing: {message}", path.display())]
    ReadBack { path: PathBuf, message: String },
    #[error("cannot write {}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl MarketError {
    /// Whether the failure says the API is unreachable or broken, as opposed
    /// to an answer about one extension (a 404, a missing license). A list
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
    /// The first line of the first license, such as "MIT License".
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
    client: reqwest::blocking::Client,
    cache: HashMap<String, (Instant, Vec<ThemeExtension>)>,
    failures: u32,
    retry_at: Option<Instant>,
}

impl Marketplace {
    /// A client for the API at `base`: [`API_BASE`] in the app, a local
    /// fixture in tests.
    ///
    /// # Errors
    ///
    /// When the HTTP client cannot be built (no TLS backend).
    pub(crate) fn new(base: impl Into<String>) -> Result<Self, MarketError> {
        let base = base.into();
        let client = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(TIMEOUT)
            .build()
            .map_err(|err| MarketError::Http {
                url: base.clone(),
                message: err.to_string(),
            })?;
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            client,
            cache: HashMap::new(),
            failures: 0,
            retry_at: None,
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

    /// Downloads `ext` and installs its themes, license and manifest into
    /// `agmen_themes/<id>/`, replacing an older install of the same id.
    ///
    /// # Errors
    ///
    /// While backing off; when the download fails; when the archive has no
    /// license file or no theme file that parses; when writing fails.
    pub(crate) fn install(
        &mut self,
        ext: &ThemeExtension,
        agmen_themes: &Path,
        now: Instant,
    ) -> Result<InstalledTheme, MarketError> {
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
        let unpacked = unpack(&ext.id, &archive?)?;
        write_install(ext, &unpacked, agmen_themes)
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
        let http = |err: reqwest::Error| MarketError::Http {
            url: url.to_string(),
            message: err.to_string(),
        };
        let response = self.client.get(url.clone()).send().map_err(http)?;
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

/// Checks `unpacked` and writes it to `agmen_themes/<id>/` through a staging
/// folder and a rename. A failed install removes the staging folder and
/// leaves any older install of the same id in place (C204).
fn write_install(
    ext: &ThemeExtension,
    unpacked: &Unpacked,
    agmen_themes: &Path,
) -> Result<InstalledTheme, MarketError> {
    let id = ext.id.clone();
    if unpacked.licenses.is_empty() {
        return Err(MarketError::NoLicense { id });
    }
    // Only theme files that parse as a Zed theme family are kept.
    let themes: Vec<&(String, Vec<u8>)> = unpacked
        .themes
        .iter()
        .filter(|(name, bytes)| {
            std::str::from_utf8(bytes).is_ok_and(|text| {
                parse_jsonc::<Family>(text, Path::new(name)).is_ok_and(|f| !f.themes.is_empty())
            })
        })
        .collect();
    if themes.is_empty() {
        return Err(MarketError::NoThemes { id });
    }
    let io = |path: &Path| {
        let path = path.to_owned();
        move |source| MarketError::Io { path, source }
    };
    let parses = |bytes: &Vec<u8>| {
        std::str::from_utf8(bytes).is_ok_and(|text| toml::from_str::<Manifest>(text).is_ok())
    };
    let manifest = match &unpacked.manifest {
        Some(bytes) if parses(bytes) => bytes.clone(),
        // An archive without a manifest that parses still records who made
        // it, from the API's answer (C206).
        _ => toml::to_string(&Manifest {
            name: Some(ext.name.clone()),
            version: Some(ext.version.clone()),
            authors: ext.authors.clone(),
            repository: ext.repository.clone(),
        })
        .map_err(|err| MarketError::Archive {
            id: id.clone(),
            message: err.to_string(),
        })?
        .into_bytes(),
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
    let swapped = write_staging(&staging, &manifest, &unpacked.licenses, &themes).and_then(|()| {
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
    themes: &[&(String, Vec<u8>)],
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
/// manifest). A folder without a license file reads with `license: None`.
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
    let license = licenses.first().and_then(|path| {
        fs::read_to_string(path).ok().and_then(|text| {
            text.lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_owned)
        })
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
