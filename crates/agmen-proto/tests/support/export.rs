//! Writes the JSON Schema and TypeScript for the wire types. Shared by the
//! `export-schema` example, which writes the committed `schema/`, and the
//! `schema_drift` test, which writes a temporary copy to compare.

use std::fs;
use std::path::{Path, PathBuf};

use agmen_proto::{ClientEnvelope, DeliveryReply, RoleSpec, StampedEnvelope};
use anyhow::Context as _;
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use ts_rs::{Config, TS};

/// The committed output: `schema/` at the workspace root.
pub fn committed_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema")
}

/// Writes every schema file into `dir` and the TypeScript into `dir/ts`.
pub fn export(dir: &Path) -> anyhow::Result<()> {
    write_schema::<ClientEnvelope>(dir, "envelope.schema.json")?;
    write_schema::<StampedEnvelope>(dir, "stamped-envelope.schema.json")?;
    write_schema::<DeliveryReply>(dir, "delivery-reply.schema.json")?;
    write_schema::<RoleSpec>(dir, "role-spec.schema.json")?;
    // JSON numbers parse to JS numbers, so u64 fields are `number`, not `bigint`.
    let cfg = Config::new()
        .with_out_dir(dir.join("ts"))
        .with_large_int("number");
    ClientEnvelope::export_all(&cfg).context("export ClientEnvelope TypeScript")?;
    StampedEnvelope::export_all(&cfg).context("export StampedEnvelope TypeScript")?;
    DeliveryReply::export_all(&cfg).context("export DeliveryReply TypeScript")?;
    RoleSpec::export_all(&cfg).context("export RoleSpec TypeScript")?;
    Ok(())
}

fn write_schema<T: JsonSchema>(dir: &Path, name: &str) -> anyhow::Result<()> {
    let schema = SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<T>();
    let mut text = serde_json::to_string_pretty(&schema).context("serialize schema")?;
    text.push('\n');
    let path = dir.join(name);
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    fs::write(&path, text).with_context(|| format!("write {}", path.display()))
}

/// The generated files under `dir`, as paths relative to it, sorted.
pub fn generated_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for sub in [Path::new(""), Path::new("ts")] {
        let full = dir.join(sub);
        if !full.exists() {
            continue;
        }
        for entry in fs::read_dir(&full).with_context(|| format!("list {}", full.display()))? {
            let path = entry
                .with_context(|| format!("list {}", full.display()))?
                .path();
            let generated = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".schema.json") || name.ends_with(".ts"));
            if path.is_file() && generated {
                files.push(sub.join(path.file_name().unwrap_or_default()));
            }
        }
    }
    files.sort();
    Ok(files)
}
