//! Regenerates the committed `schema/` from the agmen-proto types:
//! `cargo run -p agmen-proto --example export-schema`.

use std::fs;

use anyhow::Context as _;

#[path = "../tests/support/export.rs"]
mod export;

fn main() -> anyhow::Result<()> {
    let dir = export::committed_dir();
    // Remove the old output first so a renamed or deleted type leaves no file behind.
    for file in export::generated_files(&dir)? {
        let path = dir.join(file);
        fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    }
    export::export(&dir)?;
    println!("wrote {}", dir.display());
    Ok(())
}
