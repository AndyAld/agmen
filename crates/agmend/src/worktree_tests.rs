use super::*;
use crate::git::Git;
use std::ffi::OsStr;
use std::time::Duration;
use tokio::time::Instant;

async fn git(dir: &Path, args: &[&str]) -> String {
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let out = Git::new(dir, Instant::now() + Duration::from_secs(30))
        .run_with_options(&args, 1024 * 1024, Duration::from_secs(30), None)
        .await
        .unwrap();
    assert!(!out.truncated);
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

async fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-b", "main"]).await;
    git(dir.path(), &["config", "user.name", "T2 tests"]).await;
    git(dir.path(), &["config", "user.email", "t2@example.invalid"]).await;
    git(dir.path(), &["config", "commit.gpgsign", "false"]).await;
    git(dir.path(), &["config", "core.hooksPath", "/dev/null"]).await;
    std::fs::write(dir.path().join("tracked"), "base\n").unwrap();
    git(dir.path(), &["add", "."]).await;
    git(dir.path(), &["commit", "-m", "base"]).await;
    dir
}

async fn edit_commit(path: &Path) {
    std::fs::write(path.join("feature"), "feature\n").unwrap();
    git(path, &["add", "."]).await;
    git(path, &["commit", "-m", "feature"]).await;
}

#[tokio::test]
async fn create_makes_branch_from_base() {
    let repo = repo().await;
    let path = create(repo.path(), "main", "fix/b-1-slug").await.unwrap();
    assert!(path.join("tracked").is_file());
    assert_eq!(
        git(&path, &["rev-parse", "HEAD"]).await,
        git(repo.path(), &["rev-parse", "main"]).await
    );
    assert_eq!(
        git(&path, &["branch", "--show-current"]).await,
        "fix/b-1-slug"
    );
    assert!(create(repo.path(), "main", "main").await.is_err());
    assert_eq!(
        git(repo.path(), &["branch", "--show-current"]).await,
        "main"
    );
}

#[tokio::test]
async fn merged_state_ancestor() {
    let repo = repo().await;
    let path = create(repo.path(), "main", "fix/b-1-slug").await.unwrap();
    edit_commit(&path).await;
    git(repo.path(), &["merge", "--ff-only", "fix/b-1-slug"]).await;
    assert_eq!(
        merged_state(repo.path(), "fix/b-1-slug", "main")
            .await
            .unwrap(),
        Merged::Ancestor
    );
}

#[tokio::test]
async fn merged_state_cherry_after_rebase() {
    let repo = repo().await;
    let path = create(repo.path(), "main", "fix/b-1-slug").await.unwrap();
    edit_commit(&path).await;
    std::fs::write(repo.path().join("other"), "diverge\n").unwrap();
    git(repo.path(), &["add", "."]).await;
    git(repo.path(), &["commit", "-m", "diverge"]).await;
    git(repo.path(), &["cherry-pick", "fix/b-1-slug"]).await;
    assert_eq!(
        merged_state(repo.path(), "fix/b-1-slug", "main")
            .await
            .unwrap(),
        Merged::Cherry
    );
    assert!(merged_state(repo.path(), "missing", "main").await.is_err());
}

#[tokio::test]
async fn prune_removes_merged_clean() {
    let repo = repo().await;
    let path = create(repo.path(), "main", "fix/b-1-slug").await.unwrap();
    remove(repo.path(), &path, "fix/b-1-slug").await.unwrap();
    assert!(!path.exists());
    assert!(
        git(repo.path(), &["branch", "--list", "fix/b-1-slug"])
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn prune_keeps_dirty_worktree() {
    let repo = repo().await;
    let path = create(repo.path(), "main", "fix/b-1-slug").await.unwrap();
    std::fs::write(path.join("new file\nname"), "uncommitted\n").unwrap();
    let error = remove(repo.path(), &path, "fix/b-1-slug")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("uncommitted"), "{error:#}");
    assert!(path.exists());
    assert_eq!(
        git(
            repo.path(),
            &["show", "refs/wip/fix/b-1-slug:new file\nname"]
        )
        .await,
        "uncommitted"
    );
}

#[tokio::test]
async fn prune_keeps_unmerged() {
    let repo = repo().await;
    let path = create(repo.path(), "main", "fix/b-1-slug").await.unwrap();
    edit_commit(&path).await;
    std::fs::write(path.join("tracked"), "saved dirty work\n").unwrap();
    let saved = save_wip(repo.path(), &path, "fix/b-1-slug").await.unwrap();
    std::fs::write(path.join("tracked"), "base\n").unwrap();
    assert_eq!(
        merged_state(repo.path(), "fix/b-1-slug", "main")
            .await
            .unwrap(),
        Merged::No
    );
    let error = remove(repo.path(), &path, "fix/b-1-slug")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not merged"), "{error:#}");
    assert!(path.exists());
    assert!(
        !git(repo.path(), &["branch", "--list", "fix/b-1-slug"])
            .await
            .is_empty()
    );
    assert_eq!(
        git(repo.path(), &["rev-parse", "refs/wip/fix/b-1-slug"]).await,
        saved
    );
}

#[tokio::test]
async fn never_forces() {
    let repo = repo().await;
    let path = create(repo.path(), "main", "fix/b-1-slug").await.unwrap();
    save_wip(repo.path(), &path, "fix/b-1-slug").await.unwrap();
    std::fs::write(path.join("dirty"), "uncommitted").unwrap();
    assert!(remove(repo.path(), &path, "fix/b-1-slug").await.is_err());
    std::fs::remove_file(path.join("dirty")).unwrap();
    edit_commit(&path).await;
    assert!(remove(repo.path(), &path, "fix/b-1-slug").await.is_err());
    std::fs::write(repo.path().join("other"), "diverge").unwrap();
    git(repo.path(), &["add", "other"]).await;
    git(repo.path(), &["commit", "-m", "diverge"]).await;
    git(repo.path(), &["cherry-pick", "fix/b-1-slug"]).await;
    assert_eq!(
        merged_state(repo.path(), "fix/b-1-slug", "main")
            .await
            .unwrap(),
        Merged::Cherry
    );
    assert!(remove(repo.path(), &path, "fix/b-1-slug").await.is_err());
    git(repo.path(), &["merge", "--no-edit", "fix/b-1-slug"]).await;
    remove(repo.path(), &path, "fix/b-1-slug").await.unwrap();
    create_detached(
        repo.path(),
        "main",
        &repo.path().join(".worktrees/reviewer"),
    )
    .await
    .unwrap();
    COMMANDS.with_borrow(|log| {
        assert!(!log.is_empty());
        assert!(
            log.iter()
                .flatten()
                .all(|arg| arg != "--force" && arg != "-f" && arg != "-D")
        );
        for command in [
            "worktree",
            "branch",
            "commit-tree",
            "update-ref",
            "cherry",
            "status",
        ] {
            assert!(
                log.iter().any(|args| args.iter().any(|arg| arg == command)),
                "missing {command}"
            );
        }
    });
}

#[tokio::test]
async fn detached_create_checks_out_exact_commit() {
    let repo = repo().await;
    let head = git(repo.path(), &["rev-parse", "HEAD"]).await;
    let path = create_detached(
        repo.path(),
        &head,
        &repo.path().join(".worktrees/reviewer space"),
    )
    .await
    .unwrap();
    assert_eq!(git(&path, &["rev-parse", "HEAD"]).await, head);
    assert!(git(&path, &["branch", "--show-current"]).await.is_empty());
}

#[tokio::test]
async fn save_wip_preserves_branch_and_index() {
    let repo = repo().await;
    let path = create(repo.path(), "main", "fix/b-1-slug").await.unwrap();
    std::fs::write(path.join("tracked"), "staged\n").unwrap();
    git(&path, &["add", "tracked"]).await;
    std::fs::write(path.join("tracked"), "unstaged\n").unwrap();
    std::fs::write(path.join("new"), "untracked\n").unwrap();
    let head = git(&path, &["rev-parse", "HEAD"]).await;
    let index_path = git(
        &path,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )
    .await;
    let index = std::fs::read(&index_path).unwrap();
    let status = git(&path, &["status", "--porcelain=v1"]).await;
    let oid = save_wip(repo.path(), &path, "fix/b-1-slug").await.unwrap();
    assert_eq!(git(&path, &["rev-parse", "HEAD"]).await, head);
    assert_eq!(std::fs::read(&index_path).unwrap(), index);
    assert_eq!(git(&path, &["status", "--porcelain=v1"]).await, status);
    assert_eq!(
        git(repo.path(), &["show", &format!("{oid}:tracked")]).await,
        "unstaged"
    );
    assert_eq!(
        git(repo.path(), &["show", &format!("{oid}:new")]).await,
        "untracked"
    );
    assert_eq!(
        git(repo.path(), &["rev-parse", &format!("{oid}^")]).await,
        head
    );
}

#[tokio::test]
async fn save_wip_keeps_previous_snapshot_reachable() {
    let repo = repo().await;
    let path = create(repo.path(), "main", "worker").await.unwrap();
    std::fs::write(path.join("tracked"), "first\n").unwrap();
    let first = save_wip(repo.path(), &path, "worker").await.unwrap();
    std::fs::write(path.join("tracked"), "second\n").unwrap();
    let second = save_wip(repo.path(), &path, "worker").await.unwrap();
    assert_ne!(first, second);
    let reachable = git(repo.path(), &["rev-list", "refs/wip/worker"]).await;
    assert!(reachable.lines().any(|oid| oid == first), "{reachable}");
    assert_eq!(
        git(repo.path(), &["show", &format!("{first}:tracked")]).await,
        "first"
    );
    assert_eq!(
        git(repo.path(), &["show", "refs/wip/worker:tracked"]).await,
        "second"
    );
    COMMANDS.with_borrow(|log| {
        let updates: Vec<_> = log.iter().filter(|args| args[0] == "update-ref").collect();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].len(), 4);
        assert_eq!(updates[0][3], "0".repeat(first.len()).as_str());
        assert_eq!(updates[1][3], first.as_str());
    });
}

#[tokio::test]
async fn remove_refuses_main_worktree() {
    let repo = repo().await;
    assert!(remove(repo.path(), repo.path(), "main").await.is_err());
    assert!(repo.path().join("tracked").is_file());
}

#[tokio::test]
async fn remove_refuses_mismatched_branch() {
    let repo = repo().await;
    let path = create(repo.path(), "main", "worker").await.unwrap();
    git(repo.path(), &["branch", "other"]).await;
    assert!(remove(repo.path(), &path, "other").await.is_err());
    assert!(path.exists());
    assert_eq!(git(&path, &["branch", "--show-current"]).await, "worker");
}

#[tokio::test]
async fn remove_refuses_outside_root() {
    let repo = repo().await;
    let path = repo.path().join("outside");
    git(
        repo.path(),
        &["worktree", "add", "-b", "worker", path.to_str().unwrap()],
    )
    .await;
    assert!(remove(repo.path(), &path, "worker").await.is_err());
    assert!(path.exists());
}

#[tokio::test]
async fn remove_refuses_default_branch() {
    let repo = repo().await;
    let path = create(repo.path(), "main", "default").await.unwrap();
    git(
        repo.path(),
        &["update-ref", "refs/remotes/origin/default", "HEAD"],
    )
    .await;
    git(
        repo.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/default",
        ],
    )
    .await;
    assert!(remove(repo.path(), &path, "default").await.is_err());
    assert!(path.exists());
}

#[tokio::test]
async fn remove_refuses_worker_as_repo() {
    let repo = repo().await;
    let path = create(repo.path(), "main", "worker").await.unwrap();
    edit_commit(&path).await;
    assert!(remove(&path, &path, "worker").await.is_err());
    assert!(path.exists());
    assert_eq!(git(&path, &["branch", "--show-current"]).await, "worker");
}

#[tokio::test]
async fn remove_refuses_main_current_branch_in_linked_checkout() {
    let repo = repo().await;
    let path = create(repo.path(), "main", "worker").await.unwrap();
    git(repo.path(), &["symbolic-ref", "HEAD", "refs/heads/worker"]).await;
    assert!(remove(repo.path(), &path, "worker").await.is_err());
    assert!(path.exists());
}

#[tokio::test]
async fn worktree_operations_refuse_invalid_branch_names() {
    let repo = repo().await;
    let path = create(repo.path(), "main", "worker").await.unwrap();
    for branch in ["-x", "a/../b"] {
        assert!(create(repo.path(), "main", branch).await.is_err());
        assert!(remove(repo.path(), &path, branch).await.is_err());
        assert!(save_wip(repo.path(), &path, branch).await.is_err());
    }
    assert!(path.exists());
}

#[tokio::test]
async fn detached_create_refuses_outside_root() {
    let repo = repo().await;
    for path in [
        repo.path().join("outside"),
        repo.path().join(".worktrees/../escape"),
    ] {
        assert!(create_detached(repo.path(), "main", &path).await.is_err());
        assert!(!path.exists());
    }
}

#[tokio::test]
async fn create_from_worker_uses_main_root() {
    let repo = repo().await;
    let worker = create(repo.path(), "main", "worker").await.unwrap();
    let path = create(&worker, "main", "second").await.unwrap();
    assert_eq!(
        path,
        std::fs::canonicalize(repo.path())
            .unwrap()
            .join(".worktrees/second")
    );
}

#[tokio::test]
async fn create_refuses_symlink_escape() {
    let repo = repo().await;
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir(repo.path().join(".worktrees")).unwrap();
    std::os::unix::fs::symlink(outside.path(), repo.path().join(".worktrees/link")).unwrap();
    assert!(create(repo.path(), "main", "link/worker").await.is_err());
    assert!(!outside.path().join("worker").exists());
    assert!(
        create_detached(
            repo.path(),
            "main",
            &repo.path().join(".worktrees/link/review")
        )
        .await
        .is_err()
    );
    assert!(!outside.path().join("review").exists());
}

#[test]
fn branch_slugs_are_bounded() {
    assert_eq!(slug(" Fix: THE / Parser?! "), "fix-the-parser");
    assert_eq!(slug("!!!"), "work");
    assert_eq!(slug(&"a".repeat(100)).len(), 40);
    assert!(!slug(&format!("{} b", "a".repeat(39))).ends_with('-'));
}

#[tokio::test]
async fn remove_keeps_checkout_when_upstream_is_behind() {
    let repo = repo().await;
    git(repo.path(), &["branch", "stale"]).await;
    let path = create(repo.path(), "main", "fix/b-1-slug").await.unwrap();
    edit_commit(&path).await;
    git(repo.path(), &["merge", "--ff-only", "fix/b-1-slug"]).await;
    git(repo.path(), &["config", "branch.fix/b-1-slug.remote", "."]).await;
    git(
        repo.path(),
        &["config", "branch.fix/b-1-slug.merge", "refs/heads/stale"],
    )
    .await;
    let error = remove(repo.path(), &path, "fix/b-1-slug")
        .await
        .unwrap_err();
    assert!(path.exists(), "preflight should keep checkout: {error:#}");
    assert!(
        !git(repo.path(), &["branch", "--list", "fix/b-1-slug"])
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn worker_upstream_does_not_prove_base_merge() {
    let repo = repo().await;
    let path = create(repo.path(), "main", "fix/b-1-slug").await.unwrap();
    edit_commit(&path).await;
    git(repo.path(), &["config", "branch.fix/b-1-slug.remote", "."]).await;
    git(
        repo.path(),
        &[
            "config",
            "branch.fix/b-1-slug.merge",
            "refs/heads/fix/b-1-slug",
        ],
    )
    .await;
    let error = remove(repo.path(), &path, "fix/b-1-slug")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not merged"), "{error:#}");
    assert!(path.exists());
}

#[tokio::test]
async fn create_refuses_default_branch_without_a_local_copy() {
    let repo = repo().await;
    git(repo.path(), &["branch", "-m", "main", "other"]).await;
    git(
        repo.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    )
    .await;
    git(
        repo.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    )
    .await;
    let error = create(repo.path(), "origin/main", "main")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("default branch"), "{error:#}");
    assert!(
        git(repo.path(), &["branch", "--list", "main"])
            .await
            .is_empty()
    );
}
