use super::*;
use std::time::Duration;
use tokio::time::Instant;

#[test]
fn git_read_tools_fixed_args() {
    assert_eq!(
        argv(&GitRead::Log {
            rev_range: Some("main..fix/x; rm -rf /".into()),
            n: None,
        })
        .unwrap(),
        [
            "log",
            "--oneline",
            "-n",
            "50",
            "--end-of-options",
            "main..fix/x; rm -rf /"
        ]
    );
    for stat in [false, true] {
        let mut expected = vec!["show", "--no-ext-diff", "--no-textconv", "--no-color"];
        if stat {
            expected.push("--stat");
        }
        expected.extend(["--end-of-options", "HEAD"]);
        assert_eq!(
            argv(&GitRead::Show {
                rev: "HEAD".into(),
                stat
            })
            .unwrap(),
            expected
        );
    }
    assert_eq!(
        argv(&GitRead::Diff {
            base: "main".into(),
            head: "HEAD".into()
        })
        .unwrap(),
        [
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--end-of-options",
            "main",
            "HEAD",
            "--"
        ]
    );
    assert_eq!(
        argv(&GitRead::Status).unwrap(),
        ["status", "--porcelain=v1", "--branch"]
    );
    assert_eq!(
        argv(&GitRead::Log {
            rev_range: None,
            n: None
        })
        .unwrap(),
        ["log", "--oneline", "-n", "50", "--end-of-options"]
    );
}

#[test]
fn git_read_refuses_option_like_revs() {
    for rev in [
        "--output=/tmp/x".into(),
        "-p".into(),
        String::new(),
        "x".repeat(257),
        "HEAD\n".into(),
        "HEAD\0".into(),
        "HEAD\u{7f}".into(),
        "HEAD\u{85}".into(),
    ] {
        for req in [
            GitRead::Log {
                rev_range: Some(rev.clone()),
                n: None,
            },
            GitRead::Show {
                rev: rev.clone(),
                stat: false,
            },
            GitRead::Diff {
                base: rev.clone(),
                head: "HEAD".into(),
            },
            GitRead::Diff {
                base: "HEAD".into(),
                head: rev.clone(),
            },
        ] {
            assert!(matches!(argv(&req), Err(GitReadError::BadRev { .. })));
        }
    }
    assert!(
        argv(&GitRead::Show {
            rev: "x".repeat(256),
            stat: false
        })
        .is_ok()
    );
}

#[test]
fn git_read_count_is_bounded() {
    for n in [0, 201] {
        assert!(matches!(
            argv(&GitRead::Log {
                rev_range: None,
                n: Some(n)
            }),
            Err(GitReadError::BadCount)
        ));
    }
    for n in [1, 200] {
        assert_eq!(
            argv(&GitRead::Log {
                rev_range: None,
                n: Some(n)
            })
            .unwrap()[3],
            n.to_string()
        );
    }
}

async fn repo() -> (tempfile::TempDir, Git) {
    let dir = tempfile::tempdir().unwrap();
    let git = Git::new(dir.path(), Instant::now() + Duration::from_secs(30));
    git.run_str(&["init", "-q", "--initial-branch=main"], 1024)
        .await
        .unwrap();
    std::fs::write(dir.path().join("file.txt"), "before\n").unwrap();
    commit(&git).await;
    (dir, git)
}

async fn commit(git: &Git) {
    git.run_str(&["add", "file.txt"], 1024).await.unwrap();
    git.run_str(
        &[
            "-c",
            "user.name=Listener Test",
            "-c",
            "user.email=listener@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "fixture",
        ],
        1024,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn git_read_caps_output_at_32k() {
    let (dir, git) = repo().await;
    std::fs::write(dir.path().join("file.txt"), "x".repeat(40 * 1024)).unwrap();
    commit(&git).await;
    let answer = run(
        &git,
        &GitRead::Diff {
            base: "HEAD~1".into(),
            head: "HEAD".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(answer.text.len(), GIT_READ_CAP);
    assert!(answer.truncated);
}

#[tokio::test]
async fn git_read_injection_has_no_side_effect() {
    let (dir, git) = repo().await;
    let marker = dir.path().join("pwned");
    let result = run(
        &git,
        &GitRead::Log {
            rev_range: Some(format!("main..x; touch {}", marker.display())),
            n: None,
        },
    )
    .await;
    assert!(matches!(result, Err(GitReadError::Git(_))));
    assert!(!marker.exists());
}

#[tokio::test]
async fn git_read_status_lists_changes() {
    let (dir, git) = repo().await;
    std::fs::write(dir.path().join("file.txt"), "after\n").unwrap();
    let answer = run(&git, &GitRead::Status).await.unwrap();
    assert!(answer.text.contains(" M file.txt"));
    assert!(!answer.truncated);
}

#[tokio::test]
async fn git_read_lossy_utf8_stays_within_cap() {
    let (dir, git) = repo().await;
    std::fs::write(dir.path().join("file.txt"), vec![0xff; 40 * 1024]).unwrap();
    commit(&git).await;
    let answer = run(
        &git,
        &GitRead::Diff {
            base: "HEAD~1".into(),
            head: "HEAD".into(),
        },
    )
    .await
    .unwrap();
    assert!(answer.text.len() <= GIT_READ_CAP);
    assert!(answer.text.contains('\u{fffd}'));
    assert!(answer.truncated);
}

#[tokio::test]
async fn git_read_diff_never_reads_files_outside_the_repo() {
    let (dir, git) = repo().await;
    let outside = tempfile::tempdir().unwrap();
    // Past the 32 KiB cap, a no-index diff is stopped and reported as a
    // success, so its text would reach the caller.
    std::fs::write(outside.path().join("a"), "secret-one\n".repeat(4096)).unwrap();
    std::fs::write(outside.path().join("b"), "secret-two\n".repeat(4096)).unwrap();
    let name = outside.path().file_name().unwrap().to_str().unwrap();
    // Without a trailing `--`, git treats two paths outside the repository as
    // `git diff --no-index` and prints both files.
    let pairs = [
        (
            outside.path().join("a").display().to_string(),
            outside.path().join("b").display().to_string(),
        ),
        (format!("../{name}/a"), format!("../{name}/b")),
        (format!("sub/../../{name}/a"), format!("sub/../../{name}/b")),
    ];
    for (base, head) in pairs {
        let result = run(&git, &GitRead::Diff { base, head }).await;
        assert!(
            matches!(result, Err(GitReadError::Git(_))),
            "diff of paths outside {} answered with text",
            dir.path().display()
        );
    }
}
