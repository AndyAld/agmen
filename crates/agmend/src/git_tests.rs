use super::*;

fn out(bytes: &[u8]) -> Output {
    Output {
        stdout: bytes.to_vec(),
        truncated: false,
    }
}

#[test]
fn worktree_list_reads_paths_with_spaces_and_skips_bare_and_prunable() {
    let listed = parse_worktrees(&out(
        b"worktree /src/a b\0HEAD 1111111111111111111111111111111111111111\0branch refs/heads/main\0\0\
          worktree /src/rv \"2\"\0HEAD 2222222222222222222222222222222222222222\0detached\0\0\
          worktree /src/gone\0HEAD 3333333333333333333333333333333333333333\0detached\0prunable gitdir file points to non-existent location\0\0",
    ));
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[0].path, PathBuf::from("/src/a b"));
    assert_eq!(listed[0].branch.as_deref(), Some("refs/heads/main"));
    assert_eq!(listed[1].path, PathBuf::from("/src/rv \"2\""));
    assert_eq!(listed[1].branch, None);
    assert!(!listed[1].skip);
    assert!(listed[2].skip);
}

#[test]
fn status_reads_every_entry_type_and_skips_non_utf8_paths() {
    let status = parse_status(&out(
        b"# branch.oid 1111111111111111111111111111111111111111\0# branch.head main\0\
          # branch.upstream origin/main\0# branch.ab +2 -1\0\
          1 .M N... 100644 100644 100644 aaaa bbbb dir with space/a \"q\".rs\0\
          1 D. N... 100644 000000 000000 aaaa 0000 gone.rs\0\
          1 A. N... 000000 100644 100644 0000 bbbb new.rs\0\
          2 R. N... 100644 100644 100644 aaaa bbbb R100 to name.rs\0from name.rs\0\
          u UU N... 100644 100644 100644 100644 aaaa bbbb cccc both.rs\0\
          1 .M N... 100644 100644 100644 aaaa bbbb bad\xff.rs\0\
          ? un tracked.txt\0",
    ));
    assert_eq!(
        status.upstream,
        Some(Upstream {
            ahead: 2,
            behind: 1
        })
    );
    let got: Vec<(&str, ChangeKindEntry, bool)> = status
        .entries
        .iter()
        .map(|e| (e.path.as_str(), e.kind, e.untracked))
        .collect();
    assert_eq!(
        got,
        [
            (
                "dir with space/a \"q\".rs",
                ChangeKindEntry::Modified,
                false
            ),
            ("gone.rs", ChangeKindEntry::Deleted, false),
            ("new.rs", ChangeKindEntry::Added, false),
            ("to name.rs", ChangeKindEntry::Modified, false),
            ("both.rs", ChangeKindEntry::Modified, false),
            ("un tracked.txt", ChangeKindEntry::Added, true),
        ]
    );
}

#[test]
fn a_cut_short_output_drops_its_partial_record() {
    let cut = Output {
        stdout: b"a.rs\0b.rs\0c.r".to_vec(),
        truncated: true,
    };
    assert_eq!(records(&cut).collect::<Vec<_>>(), [b"a.rs", b"b.rs"]);
    // Complete output ending in NUL keeps every record.
    assert_eq!(records(&out(b"a.rs\0b.rs\0")).count(), 2);
}

#[test]
fn numstat_reads_tabs_in_paths_and_binary_files() {
    let rows = parse_numstat(&out(b"3\t1\ta\tb.rs\0-\t-\timg.png\0"));
    assert_eq!(
        rows,
        [(b"a\tb.rs".to_vec(), 3, 1), (b"img.png".to_vec(), 0, 0)]
    );
}

#[test]
fn log_reads_five_fields_per_commit() {
    let commits = parse_log(&out(
        b"1111111111111111111111111111111111111111\x001111111\0Andy A\x001761400000\0Fix \"it\"\0\
          2222222222222222222222222222222222222222\x002222222\0Se\xffan\x001761300000\0First\0",
    ));
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].subject, "Fix \"it\"");
    assert_eq!(commits[0].at, 1_761_400_000);
    assert_eq!(commits[1].author, "Se\u{fffd}an");
}

#[test]
fn hunks_follow_header_counts() {
    // A removed line that reads "-- x" and an added "++ y" stay content.
    let diff = b"diff --git a/a b/a\nindex 1..2 100644\n--- a/a\n+++ b/a\n\
                 @@ -1,3 +1,3 @@ fn main\n keep\n--- x\n+++ y\n keep\n\
                 @@ -10 +10,0 @@\n-gone\n\\ No newline at end of file\n";
    let hunks = parse_hunks(diff);
    assert_eq!(hunks.len(), 2);
    let kinds: Vec<(DiffLineKindEntry, &str)> = hunks[0]
        .lines
        .iter()
        .map(|l| (l.kind, l.text.as_str()))
        .collect();
    assert_eq!(
        kinds,
        [
            (DiffLineKindEntry::Context, "keep"),
            (DiffLineKindEntry::Removed, "-- x"),
            (DiffLineKindEntry::Added, "++ y"),
            (DiffLineKindEntry::Context, "keep"),
        ]
    );
    assert_eq!((hunks[1].old_start, hunks[1].lines.len()), (10, 1));
}

#[test]
fn marks_come_from_u0_headers() {
    let diff = b"@@ -0,0 +1,2 @@\n+a\n+b\n@@ -5 +7 @@\n-x\n+y\n@@ -9,2 +10,0 @@\n-p\n-q\n";
    assert_eq!(
        parse_marks(diff),
        [
            MarkEntry::Added { from: 1, to: 3 },
            MarkEntry::Modified { from: 7, to: 8 },
            MarkEntry::Deleted { after: 10 },
        ]
    );
}

#[test]
fn a_patch_splits_per_file() {
    let patch = b"diff --git a/x b/x\n@@ -1 +1 @@\n-diff --git a/q b/q\n+b\ndiff --git a/y b/y\nBinary files differ\n";
    let sections = split_patch(patch, false);
    assert_eq!(sections.len(), 2);
    assert!(sections[0].ends_with(b"+b\n"));
    assert_eq!(split_patch(patch, true).len(), 1);
}
