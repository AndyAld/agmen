//! Fails when the committed `schema/` no longer matches the Rust types.
//! Fix it with `cargo run -p agmen-proto --example export-schema`.

use std::fs;
use std::path::Path;

mod support {
    pub mod export;
}

use support::export;

#[test]
fn committed_schema_matches_types() {
    let fresh = Path::new(env!("CARGO_TARGET_TMPDIR")).join("schema_drift");
    if fresh.exists() {
        fs::remove_dir_all(&fresh).unwrap();
    }
    export::export(&fresh).unwrap();
    let committed = export::committed_dir();

    let expected = export::generated_files(&fresh).unwrap();
    assert!(!expected.is_empty(), "the export wrote nothing");
    assert_eq!(
        export::generated_files(&committed).unwrap(),
        expected,
        "schema/ holds a different set of files; run the export-schema example"
    );
    for file in &expected {
        let want = fs::read(fresh.join(file)).unwrap();
        let have = fs::read(committed.join(file)).unwrap();
        assert!(
            want == have,
            "schema/{} is stale; run `cargo run -p agmen-proto --example export-schema`",
            file.display()
        );
    }
}
