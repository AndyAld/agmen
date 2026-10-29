//! Listener settings storage and change cards, integrated by L21.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use agmen_local::ErrorCode;
use agmen_local::console::{SettingsChangeEntry, SettingsChangeRow, SettingsChangeState};
use agmen_proto::ProjectId;
use toml_edit::{Document, Item, Table, TableLike, value};

use super::classify::{AgentState, ChangeSettings, Direction, Field, PushApproval, classify};
use super::policy::{Settings, UsdCents};

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_CHANGES: usize = 256;
const MAX_ACCEPT_FROM: usize = 64;
const MAX_WORKER_ROLES: usize = 32;
const MAX_CENTS: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reviews {
    On,
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ListenerSettings {
    pub(super) model: String,
    pub(super) effort: Option<String>,
    pub(super) per_delegation: UsdCents,
    pub(super) daily: UsdCents,
    pub(super) max_workers: usize,
    pub(super) listener_daily: UsdCents,
    pub(super) accept_from: Vec<String>,
    pub(super) worker_roles: Vec<String>,
    pub(super) reviews: Reviews,
}

impl Default for ListenerSettings {
    fn default() -> Self {
        let policy = Settings::new("", &[], &[]);
        Self {
            model: "claude-haiku-5-5".into(),
            effort: None,
            per_delegation: policy.per_delegation,
            daily: policy.daily,
            max_workers: policy.max_workers,
            listener_daily: UsdCents(200),
            accept_from: Vec::new(),
            worker_roles: vec!["engineer".into()],
            reviews: Reviews::On,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum SettingsError {
    #[error("settings I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("settings file exceeds 1 MiB: {path}")]
    TooLarge { path: PathBuf },
    #[error("cannot parse settings at {path}: {message}")]
    Unparsable { path: PathBuf, message: String },
    #[error("bad settings value for {key}: {message}")]
    BadValue { key: String, message: &'static str },
    #[error("coordinators always reach your listener")]
    CoordinatorRemoval,
    #[error("unknown or already decided change: {0:?}")]
    UnknownChange(ChangeId),
    #[error("settings changed since {0:?} was proposed; propose it again")]
    StaleChange(ChangeId),
}

fn bad(key: &str, message: &'static str) -> SettingsError {
    SettingsError::BadValue {
        key: key.into(),
        message,
    }
}

fn io(path: &Path, source: std::io::Error) -> SettingsError {
    SettingsError::Io {
        path: path.into(),
        source,
    }
}

fn read_document(path: &Path) -> Result<Document<String>, SettingsError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return parse_document(path, String::new());
        }
        Err(err) => return Err(io(path, err)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| io(path, err))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(SettingsError::TooLarge { path: path.into() });
    }
    let parse = |message| SettingsError::Unparsable {
        path: path.into(),
        message,
    };
    let text = String::from_utf8(bytes).map_err(|err| parse(err.to_string()))?;
    parse_document(path, text)
}

fn parse_document(path: &Path, text: String) -> Result<Document<String>, SettingsError> {
    Document::parse(text).map_err(|err| SettingsError::Unparsable {
        path: path.into(),
        message: err.to_string(),
    })
}

pub(super) fn load(path: &Path, project: &ProjectId) -> Result<ListenerSettings, SettingsError> {
    let doc = read_document(path)?;
    let Some(listener) = doc.get("listener") else {
        return Ok(ListenerSettings::default());
    };
    let listener = listener
        .as_table_like()
        .ok_or_else(|| bad("listener", "expected a table"))?;
    let Some(ours) = listener.get(project.as_str()) else {
        return Ok(ListenerSettings::default());
    };
    let table = ours
        .as_table_like()
        .ok_or_else(|| bad(project.as_str(), "expected a table"))?;
    let defaults = ListenerSettings::default();
    let text = |key, default: String| -> Result<String, SettingsError> {
        table.get(key).map_or(Ok(default), |item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| bad(key, "expected a string"))
        })
    };
    let money = |key, default| {
        table
            .get(key)
            .map_or(Ok(default), |item| parse_money(key, item))
    };
    let settings = ListenerSettings {
        model: text("model", defaults.model)?,
        effort: table
            .get("effort")
            .map(|_| text("effort", String::new()))
            .transpose()?,
        per_delegation: money("per_delegation_usd", defaults.per_delegation)?,
        daily: money("daily_usd", defaults.daily)?,
        max_workers: match table.get("max_workers") {
            None => defaults.max_workers,
            Some(item) => item
                .as_integer()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| bad("max_workers", "expected a nonnegative integer"))?,
        },
        listener_daily: money("listener_daily_usd", defaults.listener_daily)?,
        accept_from: parse_list(table, "accept_from", MAX_ACCEPT_FROM, defaults.accept_from)?,
        worker_roles: parse_list(
            table,
            "worker_roles",
            MAX_WORKER_ROLES,
            defaults.worker_roles,
        )?,
        reviews: match text("reviews", "on".into())?.as_str() {
            "on" => Reviews::On,
            "off" => Reviews::Off,
            _ => return Err(bad("reviews", "expected on or off")),
        },
    };
    Ok(settings)
}

pub(super) fn parse_money(key: &str, item: &Item) -> Result<UsdCents, SettingsError> {
    let invalid = || {
        bad(
            key,
            "expected a two-decimal USD string from 0.00 to 10000.00",
        )
    };
    let (dollars, cents) = item
        .as_str()
        .and_then(|s| s.split_once('.'))
        .ok_or_else(invalid)?;
    if dollars.is_empty()
        || !dollars.bytes().all(|b| b.is_ascii_digit())
        || cents.len() != 2
        || !cents.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(invalid());
    }
    let amount = dollars
        .parse::<u64>()
        .ok()
        .and_then(|d| d.checked_mul(100))
        .and_then(|d| d.checked_add(cents.parse::<u64>().ok()?))
        .filter(|n| *n <= MAX_CENTS)
        .ok_or_else(invalid)?;
    Ok(UsdCents(amount))
}

fn parse_list(
    table: &dyn TableLike,
    key: &str,
    max: usize,
    default: Vec<String>,
) -> Result<Vec<String>, SettingsError> {
    let Some(item) = table.get(key) else {
        return Ok(default);
    };
    let array = item
        .as_array()
        .ok_or_else(|| bad(key, "expected an array of strings"))?;
    if array.len() > max {
        return Err(bad(key, "too many entries"));
    }
    array
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| bad(key, "expected an array of strings"))
        })
        .collect()
}

fn validate(
    path: &Path,
    project: &ProjectId,
    settings: &ListenerSettings,
) -> Result<(), SettingsError> {
    for (key, amount) in [
        ("per_delegation_usd", settings.per_delegation),
        ("daily_usd", settings.daily),
        ("listener_daily_usd", settings.listener_daily),
    ] {
        if amount.0 > MAX_CENTS {
            return Err(bad(key, "must not exceed 10000.00 USD"));
        }
    }
    for (key, count, max) in [
        ("accept_from", settings.accept_from.len(), MAX_ACCEPT_FROM),
        (
            "worker_roles",
            settings.worker_roles.len(),
            MAX_WORKER_ROLES,
        ),
    ] {
        if count > max {
            return Err(bad(key, "too many entries"));
        }
    }
    if i64::try_from(settings.max_workers).is_err() {
        return Err(bad("max_workers", "exceeds the TOML integer range"));
    }
    let bytes = std::iter::once(settings.model.as_str())
        .chain(settings.effort.as_deref())
        .chain(
            settings
                .accept_from
                .iter()
                .chain(&settings.worker_roles)
                .map(String::as_str),
        )
        .try_fold(0_u64, |total, text| total.checked_add(text.len() as u64));
    if bytes.is_none_or(|bytes| bytes > MAX_FILE_BYTES)
        || settings_table(settings).to_string().len() as u64
            + project.as_str().len() as u64
            + "[listener.]\n".len() as u64
            > MAX_FILE_BYTES
    {
        return Err(SettingsError::TooLarge { path: path.into() });
    }
    Ok(())
}

fn settings_table(settings: &ListenerSettings) -> Table {
    let mut table = Table::new();
    table.insert("model", value(&settings.model));
    if let Some(effort) = &settings.effort {
        table.insert("effort", value(effort));
    }
    for (key, amount) in [
        ("per_delegation_usd", settings.per_delegation),
        ("daily_usd", settings.daily),
        ("listener_daily_usd", settings.listener_daily),
    ] {
        table.insert(
            key,
            value(format!("{}.{:02}", amount.0 / 100, amount.0 % 100)),
        );
    }
    table.insert("max_workers", value(settings.max_workers as i64));
    table.insert(
        "accept_from",
        value(settings.accept_from.iter().collect::<toml_edit::Array>()),
    );
    table.insert(
        "worker_roles",
        value(settings.worker_roles.iter().collect::<toml_edit::Array>()),
    );
    table.insert(
        "reviews",
        value(match settings.reviews {
            Reviews::On => "on",
            Reviews::Off => "off",
        }),
    );
    table
}

pub(super) fn save(
    path: &Path,
    project: &ProjectId,
    settings: &ListenerSettings,
) -> Result<(), SettingsError> {
    validate(path, project, settings)?;
    // Re-read here rather than saving the document a proposal loaded earlier.
    let doc = read_document(path)?;
    let text = edit_text(doc, project, settings_table(settings))?;
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(SettingsError::TooLarge { path: path.into() });
    }
    let checked = parse_document(path, text)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|err| io(path, err))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|err| io(path, err))?;
    tmp.write_all(checked.raw().as_bytes())
        .map_err(|err| io(path, err))?;
    tmp.as_file().sync_all().map_err(|err| io(path, err))?;
    tmp.persist(path).map_err(|err| io(path, err.error))?;
    Ok(())
}

/// Use parsed spans so rendering never normalizes bytes outside our values.
fn edit_text(
    doc: Document<String>,
    project: &ProjectId,
    fields: Table,
) -> Result<String, SettingsError> {
    let raw = doc.raw();
    let listener = doc.get("listener");
    let parent = listener
        .map(|item| {
            item.as_table_like()
                .ok_or_else(|| bad("listener", "expected a table"))
        })
        .transpose()?;
    let ours = parent.and_then(|table| table.get(project.as_str()));
    if ours.is_some_and(|item| !item.is_table_like()) {
        return Err(bad(project.as_str(), "expected a table"));
    }
    if let Some(table) = ours.and_then(Item::as_table_like) {
        for key in fields
            .iter()
            .map(|(key, _)| key)
            .chain(std::iter::once("effort"))
        {
            if table
                .get(key)
                .is_some_and(|item| !item.is_value() || item.is_table_like())
            {
                return Err(bad(key, "expected a value, found a table"));
            }
        }
    }
    // Inline syntax has no standalone key lines. Edit only its value span.
    let inline = ours
        .filter(|item| item.is_inline_table())
        .or_else(|| listener.filter(|item| item.is_inline_table()));
    if let Some(inline) = inline {
        let range = inline
            .span()
            .ok_or_else(|| bad("listener", "missing value span"))?;
        let parent_inline = ours.is_none();
        return edit_inline(doc, project, fields, range, parent_inline);
    }
    let mut patches = Vec::with_capacity(11);
    if let Some(table) = ours
        .and_then(Item::as_table)
        .filter(|table| !table.is_implicit() || table.is_dotted())
    {
        let span = table
            .span()
            .ok_or_else(|| bad(project.as_str(), "missing table span"))?;
        let last_value = table
            .get_values()
            .iter()
            .filter_map(|(_, val)| val.span())
            .map(|range| range.end)
            .max()
            .unwrap_or(span.end);
        let insert_at = line_end(raw, last_value);
        let prefix = if table.is_dotted() {
            let start = raw[..span.start].rfind('\n').map_or(0, |n| n + 1);
            format!("{}.", &raw[start..span.end])
        } else {
            String::new()
        };
        // At most the file cap bounds unknown fields; only ten owned fields produce patches.
        for (key, item) in table.iter() {
            let Some(val) = item.as_value() else {
                continue;
            };
            let range = val.span().ok_or_else(|| bad(key, "missing value span"))?;
            if let Some(new) = fields.get(key) {
                patches.push((range, new.to_string()));
            } else if key == "effort" {
                let (key, _) = table
                    .get_key_value(key)
                    .ok_or_else(|| bad(key, "missing key"))?;
                let start = key
                    .span()
                    .ok_or_else(|| bad("effort", "missing key span"))?
                    .start;
                let start = raw[..start].rfind('\n').map_or(0, |n| n + 1);
                patches.push((start..line_end(raw, range.end), String::new()));
            }
        }
        let mut extra = String::new();
        for (key, val) in fields.iter().filter(|(key, _)| !table.contains_key(key)) {
            extra.push_str(&format!("{prefix}{key} = {val}\n"));
        }
        if !extra.is_empty() {
            if insert_at > 0 && !raw[..insert_at].ends_with('\n') {
                extra.insert(0, '\n');
            }
            patches.push((insert_at..insert_at, extra));
        }
    } else {
        let separator = if raw.is_empty() || raw.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        patches.push((
            raw.len()..raw.len(),
            format!("{separator}[listener.{project}]\n{fields}"),
        ));
    }
    Ok(replace_spans(raw.to_owned(), patches))
}

fn edit_inline(
    doc: Document<String>,
    project: &ProjectId,
    fields: Table,
    range: Range<usize>,
    parent_inline: bool,
) -> Result<String, SettingsError> {
    if doc
        .get("listener")
        .and_then(Item::as_table_like)
        .and_then(|parent| parent.get(project.as_str()))
        .and_then(Item::as_inline_table)
        .is_some_and(toml_edit::InlineTable::is_dotted)
    {
        return edit_dotted_inline(&doc, project, fields);
    }
    let text = doc.raw().to_owned();
    if parent_inline {
        let parent = doc
            .get("listener")
            .and_then(Item::as_table_like)
            .ok_or_else(|| bad("listener", "expected an inline table"))?;
        let comma = if parent.is_empty() { "" } else { "," };
        let insertion = format!("{project} = {}{comma}", fields.into_inline_table());
        let at = range.start + 1;
        return Ok(replace_spans(text, vec![(at..at, insertion)]));
    }
    let mut edited = doc.into_mut();
    let listener = edited
        .get_mut("listener")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| bad("listener", "expected a table"))?;
    let target = listener
        .entry(project.as_str())
        .or_insert(value(toml_edit::InlineTable::new()));
    let table = target
        .as_table_like_mut()
        .ok_or_else(|| bad(project.as_str(), "expected a table"))?;
    table.remove("effort");
    for (key, item) in fields {
        table.insert(&key, item);
    }
    let item = &edited["listener"][project.as_str()];
    let mut rendered = item
        .as_value()
        .cloned()
        .ok_or_else(|| bad("listener", "expected an inline table"))?;
    rendered.decor_mut().clear();
    Ok(replace_spans(text, vec![(range, rendered.to_string())]))
}

fn edit_dotted_inline(
    doc: &Document<String>,
    project: &ProjectId,
    fields: Table,
) -> Result<String, SettingsError> {
    let raw = doc.raw();
    let parent = doc["listener"]
        .as_table_like()
        .ok_or_else(|| bad("listener", "expected an inline table"))?;
    let table = parent
        .get(project.as_str())
        .and_then(Item::as_table_like)
        .ok_or_else(|| bad(project.as_str(), "expected a dotted inline table"))?;
    let parent_span = doc["listener"]
        .span()
        .ok_or_else(|| bad("listener", "missing value span"))?;
    let values = parent.get_values();
    let mut patches = Vec::with_capacity(11);
    for (keys, val) in &values {
        if keys.len() != 2 || keys[0].get() != project.as_str() {
            continue;
        }
        let key = keys[1].get();
        let range = val.span().ok_or_else(|| bad(key, "missing value span"))?;
        if let Some(new) = fields.get(key) {
            patches.push((range, new.to_string()));
        } else if key == "effort" {
            let key_start = keys[1]
                .span()
                .ok_or_else(|| bad(key, "missing key span"))?
                .start;
            let previous = values
                .iter()
                .filter_map(|(_, val)| val.span())
                .map(|span| span.end)
                .filter(|end| *end < key_start)
                .max();
            let after_separator = match previous {
                Some(end) => next_token(raw, end).ok_or_else(|| bad(key, "missing separator"))? + 1,
                None => parent_span.start + 1,
            };
            let start = next_token(raw, after_separator).ok_or_else(|| bad(key, "missing key"))?;
            if let Some(comma) = next_token(raw, range.end).filter(|at| raw.as_bytes()[*at] == b',')
            {
                patches.push((comma..comma + 1, String::new()));
            }
            patches.push((start..range.end, String::new()));
        }
    }
    // A trailing comma is valid TOML 1.1, including when effort was the only entry.
    let mut extra = String::new();
    for (key, val) in fields.iter().filter(|(key, _)| !table.contains_key(key)) {
        extra.push_str(&format!("{project}.{key} = {val},"));
    }
    let at = parent_span.start + 1;
    patches.push((at..at, extra));
    Ok(replace_spans(raw.to_owned(), patches))
}

fn next_token(text: &str, after: usize) -> Option<usize> {
    let mut comment = false;
    for (offset, byte) in text.as_bytes()[after..].iter().enumerate() {
        match byte {
            b'\n' => comment = false,
            _ if comment => {}
            b'#' => comment = true,
            b' ' | b'\t' | b'\r' => {}
            _ => return Some(after + offset),
        }
    }
    None
}

fn replace_spans(mut text: String, mut patches: Vec<(Range<usize>, String)>) -> String {
    // Descending spans keep earlier offsets valid, including a final value
    // with no newline before an insertion at its end.
    patches.sort_by_key(|(range, _)| (range.start, range.end));
    for (range, replacement) in patches.into_iter().rev() {
        text.replace_range(range, &replacement);
    }
    text
}

fn line_end(text: &str, after: usize) -> usize {
    text[after..]
        .find('\n')
        .map_or(text.len(), |n| after + n + 1)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ChangeId(pub(super) String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Applied {
    Now { undo: ChangeId },
    Pending { apply: ChangeId, widens: Vec<Field> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChangeState {
    Pending,
    Applied,
    Cancelled,
    Undone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AuditRow {
    pub(super) id: ChangeId,
    pub(super) direction: Direction,
    pub(super) widens: Vec<Field>,
    pub(super) state: ChangeState,
}

#[derive(Debug)]
struct Change {
    row: AuditRow,
    before: ListenerSettings,
    after: ListenerSettings,
    coordinators: Vec<String>,
    /// The person the proposal named. The console card shows it.
    requested_by: String,
}

#[derive(Debug, Default)]
struct ProjectChanges {
    entries: VecDeque<Change>,
    audit: VecDeque<AuditRow>,
}

// shortcut: pending and undo share 256 entries per project; audit keeps 256 rows,
// lost on agmend restart; persist this history in state.rs when restart recovery is needed.
#[derive(Debug, Default)]
pub(super) struct Changes {
    projects: HashMap<ProjectId, ProjectChanges>,
    published: Arc<Mutex<Published>>,
}

impl Changes {
    pub(super) fn published(&self) -> Arc<Mutex<Published>> {
        self.published.clone()
    }

    pub(super) fn propose(
        &mut self,
        path: &Path,
        project: &ProjectId,
        person: &str,
        coordinators: &[&str],
        after: ListenerSettings,
    ) -> Result<Applied, SettingsError> {
        self.propose_with(carry_classified, path, project, person, coordinators, after)
    }

    /// `propose` with the step that names the classified fields injectable, so a
    /// test can stand in for a field nobody classified.
    fn propose_with(
        &mut self,
        carry: fn(&mut ListenerSettings, &ListenerSettings),
        path: &Path,
        project: &ProjectId,
        person: &str,
        coordinators: &[&str],
        after: ListenerSettings,
    ) -> Result<Applied, SettingsError> {
        validate(path, project, &after)?;
        let before = load(path, project)?;
        if before
            .accept_from
            .iter()
            .any(|id| coordinators.contains(&id.as_str()) && !after.accept_from.contains(id))
        {
            return Err(SettingsError::CoordinatorRemoval);
        }
        let direction = direction(&before, &after, person, coordinators, carry);
        let id = ChangeId(
            crate::launch::new_session_id()
                .map_err(|err| io(Path::new("/dev/urandom"), err))?
                .chars()
                .filter(|c| *c != '-')
                .take(16)
                .collect(),
        );
        let (applied, state, widens) = match &direction {
            Direction::Narrow => {
                save(path, project, &after)?;
                (
                    Applied::Now { undo: id.clone() },
                    ChangeState::Applied,
                    Vec::new(),
                )
            }
            Direction::Widen(fields) => (
                Applied::Pending {
                    apply: id.clone(),
                    widens: fields.clone(),
                },
                ChangeState::Pending,
                fields.clone(),
            ),
        };
        let row = AuditRow {
            id,
            direction,
            widens,
            state,
        };
        {
            let history = self.projects.entry(project.clone()).or_default();
            if history.entries.len() == MAX_CHANGES {
                history.entries.pop_front();
            }
            history.entries.push_back(Change {
                row: row.clone(),
                before,
                after,
                coordinators: coordinators.iter().map(|id| (*id).to_owned()).collect(),
                requested_by: person.to_owned(),
            });
            history.record(row);
        }
        self.publish(path, project);
        Ok(applied)
    }

    // The runtime authorizes the human before calling this store.
    pub(super) fn decide(
        &mut self,
        path: &Path,
        project: &ProjectId,
        id: &ChangeId,
        apply: bool,
    ) -> Result<(), SettingsError> {
        {
            let history = self
                .projects
                .get_mut(project)
                .ok_or_else(|| SettingsError::UnknownChange(id.clone()))?;
            let change = history
                .entries
                .iter_mut()
                .find(|entry| entry.row.id == *id && entry.row.state == ChangeState::Pending)
                .ok_or_else(|| SettingsError::UnknownChange(id.clone()))?;
            if apply {
                let merged = rebase(path, project, change, &change.before, &change.after)?;
                save(path, project, &merged)?;
            }
            change.row.state = if apply {
                ChangeState::Applied
            } else {
                ChangeState::Cancelled
            };
            let row = change.row.clone();
            history.record(row);
        }
        self.publish(path, project);
        Ok(())
    }

    // The runtime authorizes the human before calling this store.
    pub(super) fn undo(
        &mut self,
        path: &Path,
        project: &ProjectId,
        id: &ChangeId,
    ) -> Result<(), SettingsError> {
        {
            let history = self
                .projects
                .get_mut(project)
                .ok_or_else(|| SettingsError::UnknownChange(id.clone()))?;
            let change = history
                .entries
                .iter_mut()
                .find(|entry| entry.row.id == *id && entry.row.state == ChangeState::Applied)
                .ok_or_else(|| SettingsError::UnknownChange(id.clone()))?;
            let merged = rebase(path, project, change, &change.after, &change.before)?;
            save(path, project, &merged)?;
            change.row.state = ChangeState::Undone;
            let row = change.row.clone();
            history.record(row);
        }
        self.publish(path, project);
        Ok(())
    }

    pub(super) fn audit(&self, project: &ProjectId) -> impl Iterator<Item = &AuditRow> {
        self.projects
            .get(project)
            .into_iter()
            .flat_map(|history| history.audit.iter())
    }

    fn publish(&self, path: &Path, project: &ProjectId) {
        remember(&self.published, path, project, self.cards_for(project));
    }

    fn cards_for(&self, project: &ProjectId) -> Vec<SettingsChangeEntry> {
        let Some(history) = self.projects.get(project) else {
            return Vec::new();
        };
        history
            .entries
            .iter()
            .take(MAX_CHANGES)
            .map(|change| SettingsChangeEntry {
                id: change.row.id.0.clone(),
                message_id: None,
                requested_by: change.requested_by.clone(),
                widens: matches!(change.row.direction, Direction::Widen(_)),
                state: match change.row.state {
                    ChangeState::Pending => SettingsChangeState::Pending,
                    ChangeState::Applied => SettingsChangeState::Applied,
                    ChangeState::Cancelled => SettingsChangeState::Cancelled,
                    ChangeState::Undone => SettingsChangeState::Undone,
                },
                rows: rows_between(&change.before, &change.after),
            })
            .collect()
    }
}

/// The caps a project's listener works to, as `load` reads them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListenerCaps {
    /// `[listener.<project>]` exists. When it does not, the values below
    /// are the defaults: a running listener works to them, but the person
    /// set none of them, so a listener that is not running does not show them.
    pub(crate) configured: bool,
    /// The configured model.
    pub(crate) model: String,
    /// Per-worker cap, USD with two decimals.
    pub(crate) per_delegation_usd: String,
    /// Workers' shared daily cap.
    pub(crate) workers_daily_usd: String,
    /// How many workers may run at once.
    pub(crate) max_workers: u32,
    /// The listener's own daily cap.
    pub(crate) listener_daily_usd: String,
    /// Who may hand it work, beyond the person and coordinators.
    pub(crate) accept_from: Vec<String>,
}

/// The project's listener caps, defaults included when the file or its
/// table is missing.
///
/// A read or parse failure is `None`: the console snapshot still returns
/// the chat.
#[must_use]
pub(crate) fn listener_caps(path: &Path, project: &ProjectId) -> Option<ListenerCaps> {
    let doc = read_document(path).ok()?;
    let configured = doc
        .get("listener")
        .and_then(Item::as_table_like)
        .and_then(|listener| listener.get(project.as_str()))
        .is_some_and(Item::is_table_like);
    let settings = load(path, project).ok()?;
    Some(ListenerCaps {
        configured,
        model: settings.model,
        per_delegation_usd: usd_text(settings.per_delegation),
        workers_daily_usd: usd_text(settings.daily),
        max_workers: u32::try_from(settings.max_workers).unwrap_or(u32::MAX),
        listener_daily_usd: usd_text(settings.listener_daily),
        accept_from: settings.accept_from,
    })
}

/// Cards for `project` last published by a change on `path`. Empty when
/// this store has not proposed, decided or undone one there.
#[must_use]
pub(crate) fn published_changes(
    published: &Mutex<Published>,
    path: &Path,
    project: &ProjectId,
) -> Vec<SettingsChangeEntry> {
    let guard = published.lock().unwrap_or_else(PoisonError::into_inner);
    guard
        .by_path
        .get(path)
        .and_then(|projects| projects.get(project))
        .cloned()
        .unwrap_or_default()
}

fn usd_text(amount: UsdCents) -> String {
    format!("{}.{:02}", amount.0 / 100, amount.0 % 100)
}

fn effort_text(effort: &Option<String>) -> String {
    effort.clone().unwrap_or_default()
}

fn reviews_text(reviews: Reviews) -> &'static str {
    match reviews {
        Reviews::On => "on",
        Reviews::Off => "off",
    }
}

fn list_text(values: &[String]) -> String {
    values
        .iter()
        .take(MAX_ACCEPT_FROM)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ")
}

fn rows_between(before: &ListenerSettings, after: &ListenerSettings) -> Vec<SettingsChangeRow> {
    let mut rows = Vec::new();
    let mut push = |setting: &str, left: String, right: String| {
        if left != right && rows.len() < 32 {
            rows.push(SettingsChangeRow {
                target: "listener".into(),
                setting: setting.into(),
                before: left,
                after: right,
            });
        }
    };
    push("model", before.model.clone(), after.model.clone());
    push(
        "effort",
        effort_text(&before.effort),
        effort_text(&after.effort),
    );
    push(
        "per_delegation_usd",
        usd_text(before.per_delegation),
        usd_text(after.per_delegation),
    );
    push("daily_usd", usd_text(before.daily), usd_text(after.daily));
    push(
        "max_workers",
        before.max_workers.to_string(),
        after.max_workers.to_string(),
    );
    push(
        "listener_daily_usd",
        usd_text(before.listener_daily),
        usd_text(after.listener_daily),
    );
    push(
        "reviews",
        reviews_text(before.reviews).to_owned(),
        reviews_text(after.reviews).to_owned(),
    );
    push(
        "accept_from",
        list_text(&before.accept_from),
        list_text(&after.accept_from),
    );
    push(
        "worker_roles",
        list_text(&before.worker_roles),
        list_text(&after.worker_roles),
    );
    rows
}

// Each Changes/Listeners value owns its published cards. A separate short lock
// lets console reads proceed while a writer holds history across file I/O.
// Keep at most 64 paths, and at most MAX_CHANGES cards per project.
const MAX_PUBLISHED_PATHS: usize = 64;

#[derive(Debug, Default)]
pub(crate) struct Published {
    order: VecDeque<PathBuf>,
    by_path: HashMap<PathBuf, HashMap<ProjectId, Vec<SettingsChangeEntry>>>,
}

fn remember(
    published: &Mutex<Published>,
    path: &Path,
    project: &ProjectId,
    cards: Vec<SettingsChangeEntry>,
) {
    let mut guard = published.lock().unwrap_or_else(PoisonError::into_inner);
    let path = path.to_path_buf();
    if let Some(index) = guard.order.iter().position(|item| item == &path) {
        if let Some(item) = guard.order.remove(index) {
            guard.order.push_back(item);
        }
    } else {
        while guard.order.len() >= MAX_PUBLISHED_PATHS {
            if let Some(old) = guard.order.pop_front() {
                guard.by_path.remove(&old);
            } else {
                break;
            }
        }
        guard.order.push_back(path.clone());
    }
    guard
        .by_path
        .entry(path)
        .or_default()
        .insert(project.clone(), cards);
}

/// Moves only the fields `change` touched from `from` to `to`, on top of the
/// file as it is now. A touched field that is no longer at its `from` value
/// has been changed since, so the owner never saw this outcome: refuse it.
fn rebase(
    path: &Path,
    project: &ProjectId,
    change: &Change,
    from: &ListenerSettings,
    to: &ListenerSettings,
) -> Result<ListenerSettings, SettingsError> {
    fn step<T: Clone + PartialEq>(current: &T, from: &T, to: &T, out: &mut T) -> bool {
        if from == to {
            return true;
        }
        out.clone_from(to);
        current == from
    }
    let current = load(path, project)?;
    let mut merged = current.clone();
    let fresh = step(&current.model, &from.model, &to.model, &mut merged.model)
        & step(
            &current.effort,
            &from.effort,
            &to.effort,
            &mut merged.effort,
        )
        & step(
            &current.per_delegation,
            &from.per_delegation,
            &to.per_delegation,
            &mut merged.per_delegation,
        )
        & step(&current.daily, &from.daily, &to.daily, &mut merged.daily)
        & step(
            &current.max_workers,
            &from.max_workers,
            &to.max_workers,
            &mut merged.max_workers,
        )
        & step(
            &current.listener_daily,
            &from.listener_daily,
            &to.listener_daily,
            &mut merged.listener_daily,
        )
        & step(
            &current.accept_from,
            &from.accept_from,
            &to.accept_from,
            &mut merged.accept_from,
        )
        & step(
            &current.worker_roles,
            &from.worker_roles,
            &to.worker_roles,
            &mut merged.worker_roles,
        )
        & step(
            &current.reviews,
            &from.reviews,
            &to.reviews,
            &mut merged.reviews,
        );
    if !fresh {
        return Err(SettingsError::StaleChange(change.row.id.clone()));
    }
    if current
        .accept_from
        .iter()
        .any(|id| change.coordinators.contains(id) && !merged.accept_from.contains(id))
    {
        return Err(SettingsError::CoordinatorRemoval);
    }
    Ok(merged)
}

impl ProjectChanges {
    fn record(&mut self, row: AuditRow) {
        if self.audit.len() == MAX_CHANGES {
            self.audit.pop_front();
        }
        self.audit.push_back(row);
    }
}

fn direction(
    before: &ListenerSettings,
    after: &ListenerSettings,
    person: &str,
    coordinators: &[&str],
    carry: fn(&mut ListenerSettings, &ListenerSettings),
) -> Direction {
    let before_accept: Vec<_> = before.accept_from.iter().map(String::as_str).collect();
    let after_accept: Vec<_> = after.accept_from.iter().map(String::as_str).collect();
    let before_roles: Vec<_> = before.worker_roles.iter().map(String::as_str).collect();
    let after_roles: Vec<_> = after.worker_roles.iter().map(String::as_str).collect();
    let snapshot = |settings: &ListenerSettings, accept_from, worker_roles| ChangeSettings {
        listener: Settings {
            person,
            coordinators,
            accept_from,
            worker_roles,
            per_delegation: settings.per_delegation,
            daily: settings.daily,
            max_workers: settings.max_workers,
        },
        listener_daily: settings.listener_daily,
        budget: UsdCents(0),
        tools: &[],
        lanes: &[],
        lends: &[],
        push_approval: PushApproval::Required,
        state: AgentState::Stopped,
    };
    let direction = classify(
        &snapshot(before, &before_accept, &before_roles),
        &snapshot(after, &after_accept, &after_roles),
    );
    let own = [
        (Field::Model, after.model != before.model),
        (Field::Effort, effort_raises(&before.effort, &after.effort)),
        (
            Field::Reviews,
            before.reviews == Reviews::On && after.reviews == Reviews::Off,
        ),
        (
            Field::Unclassified,
            differs_unclassified(before, after, carry),
        ),
    ];
    let mut fields = match direction {
        Direction::Narrow => Vec::new(),
        Direction::Widen(fields) => fields,
    };
    fields.extend(
        own.into_iter()
            .filter_map(|(f, widens)| widens.then_some(f)),
    );
    if fields.is_empty() {
        Direction::Narrow
    } else {
        Direction::Widen(fields)
    }
}

/// Copies every field `direction` judges. Add a new `ListenerSettings` field
/// here only together with its widening rule in `direction`.
fn carry_classified(to: &mut ListenerSettings, from: &ListenerSettings) {
    to.model.clone_from(&from.model);
    to.effort.clone_from(&from.effort);
    to.per_delegation = from.per_delegation;
    to.daily = from.daily;
    to.max_workers = from.max_workers;
    to.listener_daily = from.listener_daily;
    to.accept_from.clone_from(&from.accept_from);
    to.worker_roles.clone_from(&from.worker_roles);
    to.reviews = from.reviews;
}

/// Fail closed: a field that differs but is not carried above has no rule, so
/// the change waits for the owner instead of applying as a narrowing.
fn differs_unclassified(
    before: &ListenerSettings,
    after: &ListenerSettings,
    carry: fn(&mut ListenerSettings, &ListenerSettings),
) -> bool {
    let mut probe = before.clone();
    carry(&mut probe, after);
    probe != *after
}

/// Claude Code's `--effort` words, lowest first (the desktop's `Effort` order).
const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// True unless both levels are known and the new one is not higher. No level
/// means the agent's own default, so it cannot be shown to be lower or equal.
fn effort_raises(before: &Option<String>, after: &Option<String>) -> bool {
    let rank = |level: &str| EFFORT_LEVELS.iter().position(|known| *known == level);
    match (before.as_deref(), after.as_deref()) {
        (None, None) => false,
        (Some(before), Some(after)) => match (rank(before), rank(after)) {
            (Some(before), Some(after)) => after > before,
            _ => before != after,
        },
        _ => true,
    }
}

pub(super) fn check_request(
    reply_to_from: &str,
    person: &str,
    reply_to_in_batch: bool,
) -> Result<(), ErrorCode> {
    if !reply_to_in_batch {
        Err(ErrorCode::StaleRequest)
    } else if reply_to_from != person {
        Err(ErrorCode::Forbidden)
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
