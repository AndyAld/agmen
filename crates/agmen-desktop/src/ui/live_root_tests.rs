use std::time::{Duration, Instant};

use agmen_local::console::ConsoleEvent;
use agmen_local::{ApiError, ErrorCode};
use gpui::{TestAppContext, VisualTestContext};

use super::*;
use crate::appearance::DesktopSettings;
use crate::backend::fake_agmend::{FakeAgmend, console, message};
use crate::ui::test_support::{click, init, redraw};

/// A project id, as `.agmen/identity.json` names one.
fn project() -> ProjectId {
    "project1".parse().unwrap()
}

fn joined_folder(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(dir)
            .status()
            .unwrap()
            .success()
    );
    std::fs::create_dir(dir.join(".agmen")).unwrap();
    std::fs::write(dir.join(".git/info/exclude"), ".agmen/\n").unwrap();
    std::fs::write(
        dir.join(".agmen/identity.json"),
        r#"{
        "tenant":"tenant01", "project":"project1", "identity_id":"idn7k2q9",
        "handle":"andy/sol", "owner_id":"andyandy", "hub_url":"https://api.agmen.example",
        "broker_url":"wss://agmen.example/mqtt"
    }"#,
    )
    .unwrap();
}

#[gpui::test]
fn unjoined_folder_offers_sign_in(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("new-project");
    std::fs::create_dir(&folder).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&folder)
            .status()
            .unwrap()
            .success()
    );
    let (root, cx) = open(cx, empty_launch(&fake));
    pick(&root, cx, &folder);
    assert!(cx.debug_bounds("live:sign-in").is_some());
    assert!(cx.debug_bounds("live:choose-another").is_some());
}

#[gpui::test]
fn non_git_folder_says_so(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("plain");
    std::fs::create_dir(&folder).unwrap();
    let (root, cx) = open(cx, empty_launch(&fake));
    pick(&root, cx, &folder);
    assert!(cx.debug_bounds("live:not-git").is_some());
    assert!(cx.debug_bounds("live:choose-another").is_some());
}

fn helper_projects() -> std::process::Output {
    let mut out = helper_success();
    out.stdout = br#"{"projects":[]}"#.to_vec();
    out
}

fn helper_success() -> std::process::Output {
    use std::os::unix::process::ExitStatusExt as _;
    std::process::Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: vec![],
        stderr: vec![],
    }
}

#[gpui::test]
fn saved_login_offers_invite_and_successful_join_opens_project(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![message(1)]));
    let folder = fake.dir.path().join("join-here");
    std::fs::create_dir(&folder).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&folder)
            .status()
            .unwrap()
            .success()
    );
    let login = fake.dir.path().join("login.json");
    std::fs::write(
        &login,
        r#"{"hub_url":"https://hub.example/","human_id":"andyandy","handle":"andy"}"#,
    )
    .unwrap();
    let (root, cx) = open(cx, empty_launch(&fake));
    let joined = folder.clone();
    root.update_in(cx, |root, _, _| {
        root.onboarding.login_path = Some(login);
        root.onboarding.program = Some(PathBuf::from("/injected/agmen"));
        root.onboarding.helper =
            std::sync::Arc::new(move |program, args, timeout, cwd, stdin, _| {
                if args == ["whoami", "--json"].map(std::ffi::OsString::from) {
                    return Ok(helper_projects());
                }
                assert_eq!(program, std::path::Path::new("/injected/agmen"));
                assert_eq!(
                    args,
                    ["join", "--stdin", "--no-setup"].map(std::ffi::OsString::from)
                );
                assert_eq!(timeout, Duration::from_secs(120));
                assert_eq!(cwd, Some(joined.as_path()));
                assert_eq!(stdin, b"test-invite");
                joined_folder(&joined);
                Ok(helper_success())
            });
    });
    pick(&root, cx, &folder);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.onboarding.task.is_none())
    });
    assert!(cx.debug_bounds("live:invite-code").is_some());
    assert!(cx.debug_bounds("live:sign-in").is_none());
    root.update_in(cx, |root, window, cx| {
        assert_eq!(root.onboarding.hub.read(cx).text(), "https://hub.example/");
        root.onboarding
            .invite
            .update(cx, |input, cx| input.set_text("test-invite", window, cx));
    });
    click(cx, "live:join");
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    assert_eq!(
        root.read_with(cx, |root, _| root.launch.root.clone()),
        Some(folder.canonicalize().unwrap())
    );
}

#[test]
fn device_prompt_accepts_github_device_flow() {
    let prompt = super::onboarding::device_prompt(b"Your code is BCDF-GHJK\nOpen https://github.com/login/device and enter the code. It expires in 15 minutes.\n");
    assert_eq!(
        prompt,
        Some(("BCDF-GHJK".into(), "https://github.com/login/device".into()))
    );
}

#[gpui::test]
fn join_other_hub_refusal_shows_origins_without_device_login(cx: &mut TestAppContext) {
    use std::os::unix::process::ExitStatusExt as _;

    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("join-here");
    let (root, cx) = open(cx, empty_launch(&fake));
    membership_fixture(&root, cx, &folder, serde_json::json!([]));
    pick(&root, cx, &folder);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.onboarding.task.is_none())
    });
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let message = "This invite is for https://other.example. You are signed in to https://hub.example. Sign in to that hub first.";
    root.update_in(cx, |root, window, cx| {
        root.onboarding.invite.update(cx, |input, cx| {
            input.set_text(&format!("https://other.example/i#{}", "ab".repeat(32)), window, cx);
        });
        root.onboarding.helper = std::sync::Arc::new(move |_, args, _, _, _, _| {
            assert_eq!(args, ["join", "--stdin", "--no-setup"].map(std::ffi::OsString::from));
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(2 << 8),
                stdout: vec![],
                stderr: format!("{message}\nRerun with --yes or sign in with agmen login --hub https://other.example.\n").into_bytes(),
            })
        });
    });
    click(cx, "live:join");
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.onboarding.task.is_none())
    });
    root.read_with(cx, |root, _| {
        assert_eq!(root.service_error.as_deref(), Some(message));
        assert!(root.onboarding.device.is_none());
        assert!(root.onboarding.signed_in);
        assert!(root.workspace().is_none());
    });
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(cx.debug_bounds("live:device-code").is_none());
}

#[gpui::test]
fn sign_in_shows_device_prompt_then_invite_without_network(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("sign-in-here");
    std::fs::create_dir(&folder).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&folder)
            .status()
            .unwrap()
            .success()
    );
    let (root, cx) = open(cx, empty_launch(&fake));
    let (release, wait) = std::sync::mpsc::sync_channel(1);
    let wait = std::sync::Mutex::new(wait);
    root.update_in(cx, |root, _, _| {
        root.onboarding.program = Some(PathBuf::from("/injected/agmen"));
        root.onboarding.helper = std::sync::Arc::new(move |_, args, timeout, _, input, progress| {
            if args == ["whoami", "--json"].map(std::ffi::OsString::from) { return Ok(helper_projects()); }
            assert_eq!(args, ["login", "--hub", "https://hub.example/"].map(std::ffi::OsString::from));
            assert_eq!(timeout, Duration::from_secs(120));
            assert!(input.is_empty());
            progress(b"Your code is ABCD-1234\nOpen https://hub.example/device to confirm it. It expires in 2 minutes.\n");
            wait.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(helper_success())
        });
    });
    pick(&root, cx, &folder);
    root.update_in(cx, |root, window, cx| {
        root.onboarding.hub.update(cx, |input, cx| {
            input.set_text("https://hub.example/", window, cx)
        })
    });
    click(cx, "live:sign-in");
    wait_until(cx, |cx| cx.debug_bounds("live:device-code").is_some());
    assert!(cx.debug_bounds("live:verification-browser").is_some());
    assert!(cx.debug_bounds("live:choose-another").is_some());
    release.send(()).unwrap();
    wait_until(cx, |cx| cx.debug_bounds("live:invite-code").is_some());
    assert!(cx.debug_bounds("live:device-code").is_none());
    // A folder worker may deliver a logged-out snapshot after login completes.
    root.update_in(cx, |root, window, cx| {
        root.apply_saved_login(0, Ok(None), window, cx);
        root.apply_saved_login(
            0,
            Ok(Some(agmen_client::Login {
                hub_url: "https://old-hub.example/".parse().unwrap(),
                human_id: "andyandy".parse().unwrap(),
                handle: "andy".into(),
            })),
            window,
            cx,
        );
        assert_eq!(root.onboarding.hub.read(cx).text(), "https://hub.example/");
        cx.notify();
    });
    redraw(cx);
    assert!(cx.debug_bounds("live:invite-code").is_some());
}

#[gpui::test]
fn failed_join_stays_in_app_with_stderr(cx: &mut TestAppContext) {
    use std::os::unix::process::ExitStatusExt as _;
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("failed-join");
    std::fs::create_dir(&folder).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&folder)
            .status()
            .unwrap()
            .success()
    );
    let (root, cx) = open(cx, empty_launch(&fake));
    pick(&root, cx, &folder);
    root.update_in(cx, |root, window, cx| {
        root.onboarding.signed_in = true;
        root.onboarding.program = Some(PathBuf::from("/injected/agmen"));
        root.onboarding
            .invite
            .update(cx, |input, cx| input.set_text("test-invite", window, cx));
        root.onboarding.helper = std::sync::Arc::new(|_, _, _, _, _, _| {
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(256),
                stdout: vec![],
                stderr: b"the invite has expired; ask for a new one".to_vec(),
            })
        });
        cx.notify();
    });
    redraw(cx);
    click(cx, "live:join");
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.onboarding.task.is_none())
    });
    assert_eq!(
        root.read_with(cx, |root, _| root.service_error.clone())
            .as_deref(),
        Some("the invite has expired; ask for a new one")
    );
    assert!(cx.debug_bounds("live:invite-code").is_some());
    assert!(cx.debug_bounds("live:choose-another").is_some());
}

#[gpui::test]
fn successful_helper_without_identity_reports_failure(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("no-identity");
    std::fs::create_dir(&folder).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&folder)
            .status()
            .unwrap()
            .success()
    );
    let (root, cx) = open(cx, empty_launch(&fake));
    pick(&root, cx, &folder);
    root.update_in(cx, |root, window, cx| {
        root.onboarding.signed_in = true;
        root.onboarding.program = Some(PathBuf::from("/injected/agmen"));
        root.onboarding
            .invite
            .update(cx, |input, cx| input.set_text("test-invite", window, cx));
        root.onboarding.helper = std::sync::Arc::new(|_, _, _, _, _, _| Ok(helper_success()));
        cx.notify();
    });
    redraw(cx);
    click(cx, "live:join");
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| {
            root.onboarding.task.is_none() && root._resolving.is_none()
        })
    });
    assert!(root.read_with(cx, |root, _| root.workspace().is_none()));
    assert!(root.read_with(cx, |root, _| {
        root.service_error
            .as_ref()
            .is_some_and(|error| error.contains("trusted project identity"))
    }));
}

#[gpui::test]
fn choosing_another_folder_clears_device_prompt(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let (root, cx) = open(cx, empty_launch(&fake));
    root.update_in(cx, |root, _, _| {
        root.onboarding.device = Some(("TEST-CODE".into(), "https://hub.example/device".into()))
    });
    let folder = fake.dir.path().join("another");
    std::fs::create_dir(&folder).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&folder)
            .status()
            .unwrap()
            .success()
    );
    pick(&root, cx, &folder);
    assert!(cx.debug_bounds("live:device-code").is_none());
}

#[gpui::test]
fn late_join_does_not_replace_newly_selected_project(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![message(1)]));
    let folder = fake.dir.path().join("joining");
    std::fs::create_dir(&folder).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&folder)
            .status()
            .unwrap()
            .success()
    );
    let another = fake.dir.path().join("another");
    joined_folder(&another);
    let (root, cx) = open(cx, empty_launch(&fake));
    pick(&root, cx, &folder);
    let (release, wait) = std::sync::mpsc::sync_channel(1);
    let wait = std::sync::Mutex::new(wait);
    let joined = folder.clone();
    root.update_in(cx, |root, window, cx| {
        root.onboarding.signed_in = true;
        root.onboarding.program = Some(PathBuf::from("/injected/agmen"));
        root.onboarding
            .invite
            .update(cx, |input, cx| input.set_text("test-invite", window, cx));
        root.onboarding.helper = std::sync::Arc::new(move |_, args, _, _, _, _| {
            if args == ["whoami", "--json"].map(std::ffi::OsString::from) {
                return Ok(helper_projects());
            }
            wait.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            joined_folder(&joined);
            Ok(helper_success())
        });
        cx.notify();
    });
    redraw(cx);
    click(cx, "live:join");
    pick(&root, cx, &another);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    release.send(()).unwrap();
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.onboarding.task.is_none())
    });
    assert_eq!(
        root.read_with(cx, |root, _| root.launch.root.clone()),
        Some(another.canonicalize().unwrap())
    );
}

#[gpui::test]
fn late_login_signs_in_the_newly_selected_folder(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let first = fake.dir.path().join("first");
    let second = fake.dir.path().join("second");
    for folder in [&first, &second] {
        std::fs::create_dir(folder).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .arg(folder)
                .status()
                .unwrap()
                .success()
        );
    }
    let (root, cx) = open(cx, empty_launch(&fake));
    pick(&root, cx, &first);
    let (release, wait) = std::sync::mpsc::sync_channel(1);
    let wait = std::sync::Mutex::new(wait);
    root.update_in(cx, |root, window, cx| {
        root.onboarding.program = Some(PathBuf::from("/injected/agmen"));
        root.onboarding.hub.update(cx, |input, cx| {
            input.set_text("https://hub.example/", window, cx)
        });
        root.onboarding.helper = std::sync::Arc::new(move |_, args, _, _, _, _| {
            if args == ["whoami", "--json"].map(std::ffi::OsString::from) {
                return Ok(helper_projects());
            }
            wait.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            Ok(helper_success())
        });
    });
    click(cx, "live:sign-in");
    pick(&root, cx, &second);
    release.send(()).unwrap();
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.onboarding.task.is_none())
    });
    assert!(cx.debug_bounds("live:invite-code").is_some());
}

#[gpui::test]
fn enter_preserves_onboarding_fields(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("enter");
    std::fs::create_dir(&folder).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&folder)
            .status()
            .unwrap()
            .success()
    );
    let (root, cx) = open(cx, empty_launch(&fake));
    pick(&root, cx, &folder);
    root.update_in(cx, |root, window, cx| {
        root.onboarding.hub.update(cx, |input, cx| {
            input.set_text("https://hub.example/", window, cx)
        });
        window.focus(&root.onboarding.hub.focus_handle(cx), cx);
    });
    cx.simulate_keystrokes("enter");
    assert_eq!(
        root.read_with(cx, |root, cx| root
            .onboarding
            .hub
            .read(cx)
            .text()
            .to_owned()),
        "https://hub.example/"
    );
    root.update_in(cx, |root, window, cx| {
        root.onboarding.signed_in = true;
        root.onboarding
            .invite
            .update(cx, |input, cx| input.set_text("test-invite", window, cx));
        window.focus(&root.onboarding.invite.focus_handle(cx), cx);
        cx.notify();
    });
    redraw(cx);
    cx.simulate_keystrokes("enter");
    assert_eq!(
        root.read_with(cx, |root, cx| root
            .onboarding
            .invite
            .read(cx)
            .text()
            .to_owned()),
        "test-invite"
    );
}

fn pick(root: &Entity<LiveRoot>, cx: &mut VisualTestContext, path: &std::path::Path) {
    let selector = if cx.debug_bounds("live:choose-another").is_some() {
        "live:choose-another"
    } else {
        "live:open-project"
    };
    click(cx, selector);
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(!options.files);
        assert!(options.directories);
        assert!(!options.multiple);
        Some(vec![path.to_owned()])
    });
    cx.run_until_parked();
    redraw(cx);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root._resolving.is_none())
    });
    root.read_with(cx, |_, _| {});
}

fn empty_launch(fake: &FakeAgmend) -> LiveLaunch {
    LiveLaunch {
        socket: fake.path(),
        root: None,
        project: ProjectChoice::None { why: None },
        settings: Some(fake.dir.path().join("settings.toml")),
    }
}

fn stored_recents(path: &std::path::Path) -> Vec<PathBuf> {
    let text = std::fs::read_to_string(path).unwrap();
    let table: toml::Table = text.parse().unwrap();
    table["desktop"]["recent_projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| PathBuf::from(v.as_str().unwrap()))
        .collect()
}

#[gpui::test]
fn picker_joined_repo_opens_and_remembers(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![message(1)]));
    let folder = fake.dir.path().join("joined");
    joined_folder(&folder);
    let settings = fake.dir.path().join("settings.toml");
    let (root, cx) = open(cx, empty_launch(&fake));
    pick(&root, cx, &folder);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    assert_eq!(
        DesktopSettings::load(&settings).unwrap().last_project,
        Some(project())
    );
    assert_eq!(
        stored_recents(&settings),
        vec![std::fs::canonicalize(folder).unwrap()]
    );
}

#[gpui::test]
fn picker_subfolder_resolves_to_joined_root(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![message(1)]));
    let folder = fake.dir.path().join("joined");
    joined_folder(&folder);
    let sub = folder.join("src/nested");
    std::fs::create_dir_all(&sub).unwrap();
    let (root, cx) = open(cx, empty_launch(&fake));
    pick(&root, cx, &sub);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    let expected = std::fs::canonicalize(folder).unwrap();
    root.read_with(cx, |root, _| {
        assert_eq!(root.launch.root.as_ref(), Some(&expected))
    });
    assert_eq!(
        stored_recents(&fake.dir.path().join("settings.toml")),
        vec![expected]
    );
}

#[gpui::test]
fn picker_non_repo_and_unjoined_repo_keep_last_project(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let settings = fake.dir.path().join("settings.toml");
    std::fs::write(&settings, "[desktop]\nlast_project = 'projectold'\n").unwrap();
    let folder = fake.dir.path().join("unjoined");
    std::fs::create_dir(&folder).unwrap();
    let (root, cx) = open(cx, empty_launch(&fake));
    for is_repo in [false, true] {
        if is_repo {
            assert!(
                std::process::Command::new("git")
                    .args(["init", "-q"])
                    .arg(&folder)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        pick(&root, cx, &folder);
        assert!(cx.debug_bounds("live:not-a-project").is_some());
        assert!(cx.debug_bounds("live:choose-another").is_some());
        assert_eq!(
            DesktopSettings::load(&settings).unwrap().last_project,
            Some("projectold".parse().unwrap())
        );
        assert_eq!(
            std::fs::read_to_string(&settings).unwrap(),
            "[desktop]\nlast_project = 'projectold'\n"
        );
    }
}

#[gpui::test]
fn missing_recent_is_dimmed_and_removable(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let settings = fake.dir.path().join("settings.toml");
    let missing = fake.dir.path().join("missing");
    std::fs::write(
        &settings,
        format!(
            "[desktop]\nrecent_projects = [{}]\n",
            toml::Value::String(missing.to_string_lossy().into_owned())
        ),
    )
    .unwrap();
    let (_, cx) = open(cx, empty_launch(&fake));
    assert!(cx.debug_bounds("live:recent-missing:0").is_some());
    click(cx, "live:remove-recent:0");
    assert!(stored_recents(&settings).is_empty());
    redraw(cx);
    assert!(cx.debug_bounds("live:recent-missing:0").is_none());
}

#[gpui::test]
fn recents_bound_dedupe_and_order(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let (root, cx) = open(cx, empty_launch(&fake));
    let mut expected = Vec::new();
    for index in (0..12).chain([5]) {
        let folder = fake.dir.path().join(format!("joined{index}"));
        if !folder.exists() {
            joined_folder(&folder);
        }
        if root.read_with(cx, |root, _| root.workspace().is_some()) {
            cx.simulate_keystrokes("cmd-o");
            assert!(cx.did_prompt_for_paths());
            cx.simulate_path_prompt_response(|_| Some(vec![folder.clone()]));
        } else {
            pick(&root, cx, &folder);
        }
        let canonical = std::fs::canonicalize(&folder).unwrap();
        wait_until(cx, |cx| {
            root.read_with(cx, |root, _| {
                root.workspace().is_some() && root.launch.root.as_ref() == Some(&canonical)
            })
        });
        expected.retain(|path| path != &canonical);
        expected.insert(0, canonical);
        expected.truncate(10);
        assert_eq!(
            stored_recents(&fake.dir.path().join("settings.toml")),
            expected
        );
    }
}

#[gpui::test]
fn invalid_pick_preserves_workspace_and_switch_drops_old_backend(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let mut launch = empty_launch(&fake);
    launch.project = ProjectChoice::Project(project());
    let (root, cx) = open(cx, launch);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    let workspace = root.read_with(cx, |root, _| root.workspace().unwrap().entity_id());
    let old_backend = cx.update(|_, cx| Rc::downgrade(&cx.global::<BackendHandle>().0));
    cx.simulate_keystrokes("cmd-o");
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| Some(vec![fake.dir.path().to_owned()]));
    cx.run_until_parked();
    wait_until(cx, |cx| cx.debug_bounds("live:not-a-project").is_some());
    assert!(cx.debug_bounds("live:not-a-project").is_some());
    root.read_with(cx, |root, _| {
        assert_eq!(root.workspace().unwrap().entity_id(), workspace)
    });
    assert!(old_backend.upgrade().is_some());
    click(cx, "live:keep-project");
    assert!(cx.debug_bounds("chat").is_some());
    let folder = fake.dir.path().join("next");
    joined_folder(&folder);
    cx.simulate_keystrokes("cmd-o");
    cx.simulate_path_prompt_response(|_| Some(vec![folder.clone()]));
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| {
            root.workspace().is_some() && root.launch.root.is_some()
        })
    });
    assert!(old_backend.upgrade().is_none(), "old backend stayed alive");
    root.read_with(cx, |root, _| {
        assert_ne!(root.workspace().unwrap().entity_id(), workspace)
    });
    assert_eq!(cx.update(|_, cx| cx.windows().len()), 1);
}

#[test]
fn recents_are_additive_to_old_desktop_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    std::fs::write(&path, "[desktop]\nlast_project = 'projectold'\n").unwrap();
    let settings = DesktopSettings::load(&path).unwrap();
    assert_eq!(settings.last_project, Some("projectold".parse().unwrap()));
    let value = toml::Value::try_from(settings).unwrap();
    assert_eq!(value["recent_projects"].as_array().unwrap().len(), 0);
}

#[test]
fn missing_recent_menu_item_is_disabled_with_remove_action() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing");
    let menus = super::project_open::project_menus(&[missing]);
    let gpui::MenuItem::Submenu(recent) = &menus[1].items[1] else {
        panic!("no Open Recent submenu")
    };
    assert!(matches!(
        &recent.items[0],
        gpui::MenuItem::Action { disabled: true, .. }
    ));
    assert!(
        matches!(&recent.items[1], gpui::MenuItem::Action { name, disabled: false, .. } if name == "Remove from list")
    );
}

#[gpui::test]
fn launch_argument_opens_joined_subfolder_through_live_backend(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("joined");
    joined_folder(&folder);
    let sub = folder.join("src");
    std::fs::create_dir(&sub).unwrap();
    let launch = crate::Launch::parse(&[sub.to_string_lossy().into_owned()]).unwrap();
    let crate::Launch::OpenProject(path) = launch else {
        panic!("folder was not a live launch")
    };
    init(cx);
    cx.executor().allow_parking();
    let (root, cx) =
        cx.add_window_view(|window, cx| LiveRoot::new(empty_launch(&fake), Some(path), window, cx));
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    assert_eq!(
        stored_recents(&fake.dir.path().join("settings.toml")),
        vec![std::fs::canonicalize(folder).unwrap()]
    );
    let state = fake.state.lock().unwrap();
    assert!(state.watch_params.iter().any(|params| {
        params["root"]
            == sub
                .parent()
                .unwrap()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .as_ref()
    }));
}

#[gpui::test]
fn cancelling_picker_keeps_current_project(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let mut launch = empty_launch(&fake);
    launch.project = ProjectChoice::Project(project());
    let (root, cx) = open(cx, launch);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    let before = std::fs::read(fake.dir.path().join("settings.toml")).unwrap();
    let workspace = root.read_with(cx, |root, _| root.workspace().unwrap().entity_id());
    cx.simulate_keystrokes("cmd-o");
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| None);
    cx.run_until_parked();
    root.read_with(cx, |root, _| {
        assert_eq!(root.workspace().unwrap().entity_id(), workspace)
    });
    assert_eq!(
        std::fs::read(fake.dir.path().join("settings.toml")).unwrap(),
        before
    );
}

#[gpui::test]
fn recent_list_and_file_url_open_in_the_same_window(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("a project");
    joined_folder(&folder);
    let settings = fake.dir.path().join("settings.toml");
    std::fs::write(
        &settings,
        format!(
            "[desktop]\nrecent_projects = [{}]\n",
            toml::Value::String(folder.to_string_lossy().into_owned())
        ),
    )
    .unwrap();
    let (root, cx) = open(cx, empty_launch(&fake));
    click(cx, "live:open-recent:0");
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    let old = root.read_with(cx, |root, _| root.workspace().unwrap().entity_id());
    let url = reqwest::Url::from_file_path(&folder).unwrap();
    assert!(url.as_str().contains("%20"));
    let path = crate::file_open_path(&[url.to_string()]).unwrap();
    cx.cx.update(|cx| crate::open_in_live_window(path, cx));
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| {
            root.workspace()
                .is_some_and(|workspace| workspace.entity_id() != old)
        })
    });
    assert_eq!(cx.update(|_, cx| cx.windows().len()), 1);
    assert_eq!(
        stored_recents(&settings),
        vec![folder.canonicalize().unwrap()]
    );
    assert!(crate::file_open_path(&["https://example.com/project".into()]).is_none());
    assert!(crate::file_open_path(&["file://remote-host/project".into()]).is_none());
}

#[gpui::test]
fn recent_menu_action_uses_folder_opener(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("joined");
    joined_folder(&folder);
    let (root, cx) = open(cx, empty_launch(&fake));
    cx.dispatch_action(super::OpenRecent(folder.clone()));
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    assert_eq!(
        stored_recents(&fake.dir.path().join("settings.toml")),
        vec![folder.canonicalize().unwrap()]
    );
}

#[gpui::test]
fn refused_project_has_open_project_button(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let (root, cx) = open(cx, empty_launch(&fake));
    root.update(cx, |root, cx| {
        root.state = LiveState::Refused("Project not found".into());
        cx.notify();
    });
    redraw(cx);
    click(cx, "live:open-project");
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| None);
    cx.run_until_parked();
}

#[gpui::test]
fn project_errors_are_visible_over_workspace_and_rejected_folder(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let mut launch = empty_launch(&fake);
    launch.project = ProjectChoice::Project(project());
    let (root, cx) = open(cx, launch);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    root.update(cx, |root, cx| {
        root.service_error = Some("Opening the folder picker failed: test failure".into());
        cx.notify();
    });
    redraw(cx);
    assert!(cx.debug_bounds("live:project-error").is_some());
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.open_folder(fake.dir.path().to_owned(), window, cx)
        })
    });
    wait_until(cx, |cx| cx.debug_bounds("live:not-a-project").is_some());
    assert!(cx.debug_bounds("live:project-error").is_some());
    assert!(cx.debug_bounds("live:not-a-project").is_some());
}

#[gpui::test]
fn folder_resolution_draws_before_git_answers(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("joined");
    joined_folder(&folder);
    let (root, cx) = open(cx, empty_launch(&fake));
    cx.update(|window, cx| root.update(cx, |root, cx| root.open_folder(folder, window, cx)));
    redraw(cx);
    assert!(cx.debug_bounds("live:checking-folder").is_some());
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
}

#[test]
fn bare_binary_keeps_todays_agmend_down_screen() {
    let wording = Wording::of(&LiveState::AgmendDown, &ProjectChoice::Project(project()));
    assert_eq!(wording.command, Some("agmend service install"));
}

#[test]
fn host_without_project_waits_for_hub_health() {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| {
        state.host_status = Some(serde_json::json!({
            "configured": true, "state": "running", "last_error": null,
            "public_url": "http://127.0.0.1:9/"
        }))
    });
    let result = LiveRoot::connect_host(
        &fake.path(),
        &ProjectChoice::None { why: None },
        None,
        Observation {
            check: Check::NoBundle,
            running: true,
        },
        Instant::now() + Duration::from_millis(100),
    );
    assert!(
        result.is_err(),
        "a live process without a listening hub must keep waiting"
    );
}

#[test]
fn host_without_project_checks_the_hubs_health_endpoint() {
    use std::io::{Read as _, Write as _};
    for code in [200, 503] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for _ in 0..100 {
                if let Ok((mut stream, _)) = listener.accept() {
                    stream
                        .set_read_timeout(Some(Duration::from_millis(200)))
                        .unwrap();
                    let mut request = [0; 4096];
                    let n = stream.read(&mut request).unwrap();
                    assert!(
                        std::str::from_utf8(&request[..n])
                            .unwrap()
                            .starts_with("GET /healthz ")
                    );
                    write!(
                        stream,
                        "HTTP/1.1 {code} Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .unwrap();
                    return true;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            false
        });
        let fake = FakeAgmend::start(console(vec![]));
        fake.edit(|state| {
            state.host_status = Some(serde_json::json!({
                "configured": true, "state": "running", "last_error": null, "public_url": url
            }))
        });
        let result = LiveRoot::connect_host(
            &fake.path(),
            &ProjectChoice::None { why: None },
            None,
            Observation {
                check: Check::NoBundle,
                running: true,
            },
            Instant::now() + Duration::from_millis(200),
        );
        assert_eq!(result.is_ok(), code == 200);
        assert!(
            server.join().unwrap(),
            "the hub health endpoint must be checked"
        );
    }
}

#[gpui::test]
fn host_starting_draws_without_daemon_down_or_sign_in(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| {
        state.snapshot_delay = Duration::from_millis(500);
        state.host_status = Some(serde_json::json!({
            "configured": true, "state": "running", "last_error": null, "public_url": "https://hub.example/"
        }));
    });
    std::fs::write(fake.dir.path().join("host.toml"), "configured").unwrap();
    let mut launch = empty_launch(&fake);
    launch.project = ProjectChoice::Project(project());
    let (root, cx) = open(cx, launch);
    assert!(cx.debug_bounds("live:host-starting").is_some());
    assert!(cx.debug_bounds("live:agmend-down").is_none());
    assert!(cx.debug_bounds("live:sign-in").is_none());
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
}

#[gpui::test]
fn hosted_startup_opens_automatically_when_the_hub_starts(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| state.host_status = Some(serde_json::json!({
        "configured": true, "state": "stopped", "last_error": null, "public_url": "https://hub.example/"
    })));
    std::fs::write(fake.dir.path().join("host.toml"), "configured").unwrap();
    let mut launch = empty_launch(&fake);
    launch.project = ProjectChoice::Project(project());
    let (root, cx) = open(cx, launch);
    let unjoined = fake.dir.path().join("unjoined");
    std::fs::create_dir(&unjoined).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&unjoined)
            .status()
            .unwrap()
            .success()
    );
    root.update_in(cx, |root, window, cx| {
        root.open_folder(unjoined, window, cx)
    });
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root._resolving.is_none())
    });
    assert!(cx.debug_bounds("live:host-starting").is_some());
    assert!(cx.debug_bounds("live:agmend-down").is_none());
    assert!(cx.debug_bounds("live:sign-in").is_none());
    fake.edit(|state| state.host_status.as_mut().unwrap()["state"] = serde_json::json!("running"));
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
}

#[gpui::test]
fn hosted_startup_reports_the_hubs_failure(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| {
        state.host_status = Some(serde_json::json!({
            "configured": true, "state": "failed", "last_error": "Postgres connection refused",
            "public_url": "https://hub.example/"
        }))
    });
    std::fs::write(fake.dir.path().join("host.toml"), "configured").unwrap();
    let mut launch = empty_launch(&fake);
    launch.project = ProjectChoice::Project(project());
    let (root, cx) = open(cx, launch);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| matches!(root.state(), LiveState::Refused(_)))
    });
    root.read_with(cx, |root, _| assert!(matches!(root.state(), LiveState::Refused(error) if error == "Postgres connection refused")));
    assert!(cx.debug_bounds("live:agmend-down").is_none());
    assert!(cx.debug_bounds("live:sign-in").is_none());
}

#[gpui::test]
fn failed_host_shows_its_error_over_sign_in_and_can_restart(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| {
        state.host_status = Some(serde_json::json!({
            "configured": true, "state": "failed", "last_error": "Postgres connection refused",
            "public_url": "https://hub.example/"
        }))
    });
    std::fs::write(fake.dir.path().join("host.toml"), "configured").unwrap();
    let mut launch = empty_launch(&fake);
    launch.project = ProjectChoice::Project(project());
    let (root, cx) = open(cx, launch);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| matches!(root.state(), LiveState::Refused(_)))
    });
    let folder = fake.dir.path().join("unjoined");
    std::fs::create_dir(&folder).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&folder)
            .status()
            .unwrap()
            .success()
    );
    root.update_in(cx, |root, window, cx| root.open_folder(folder, window, cx));
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root._resolving.is_none())
    });
    assert!(cx.debug_bounds("live:refused").is_some());
    assert!(cx.debug_bounds("live:sign-in").is_none());
    assert!(cx.debug_bounds("live:restart-host").is_some());
    click(cx, "live:restart-host");
    wait_until(cx, |_| fake.state.lock().unwrap().host_restarts == 1);
    fake.edit(|state| state.host_status.as_mut().unwrap()["state"] = serde_json::json!("running"));
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
}

#[gpui::test]
fn hosted_startup_timeout_stops_the_starting_screen(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| {
        state.host_status = Some(serde_json::json!({
            "configured": true, "state": "stopped", "last_error": null,
            "public_url": "https://hub.example/"
        }))
    });
    std::fs::write(fake.dir.path().join("host.toml"), "configured").unwrap();
    let mut launch = empty_launch(&fake);
    launch.project = ProjectChoice::Project(project());
    let (root, cx) = open(cx, launch);
    cx.executor().advance_clock(Duration::from_secs(30));
    cx.run_until_parked();
    root.read_with(cx, |root, _| {
        assert!(matches!(root.state(), LiveState::Refused(error) if error.contains("30 seconds")))
    });
    fake.edit(|state| {
        let status = state.host_status.as_mut().unwrap();
        status["state"] = serde_json::json!("failed");
        status["last_error"] = serde_json::json!("test stopped");
    });
}

#[test]
fn hosting_still_shows_a_service_that_needs_setup() {
    // A stale or missing agmend service cannot answer host_status: the
    // setup screen, not a 30 s hub wait, is what helps.
    let fake = FakeAgmend::start(console(vec![]));
    for check in [Check::Missing, Check::Stopped] {
        let started = Instant::now();
        let (observed, backend) = LiveRoot::connect_observed(
            &fake.path(),
            &ProjectChoice::Project(project()),
            None,
            true,
            Some(Instant::now() + Duration::from_millis(300)),
            Observation {
                check: check.clone(),
                running: false,
            },
        )
        .expect("the service diagnosis comes back");
        assert_eq!(observed.check, check);
        assert!(backend.is_none());
        assert!(started.elapsed() < Duration::from_millis(200));
    }
}

#[gpui::test]
fn launch_restores_the_last_project_directly(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("last-project");
    joined_folder(&folder);
    let mut launch = empty_launch(&fake);
    DesktopSettings {
        last_project: Some(project()),
        recent_projects: vec![folder.clone()],
        ..DesktopSettings::default()
    }
    .save(launch.settings.as_deref().unwrap())
    .unwrap();
    (launch.project, launch.root) =
        crate::project::choose_saved_project(launch.settings.as_deref());
    let (root, cx) = open(cx, launch);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    assert!(cx.debug_bounds("live:no-project").is_none());
    assert!(
        fake.state
            .lock()
            .unwrap()
            .snapshot_params
            .iter()
            .any(|params| params["root"] == folder.canonicalize().unwrap().to_str().unwrap())
    );
}

#[gpui::test]
fn stopped_foreign_service_keeps_bundle_diagnosis_and_retry(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let launch = LiveLaunch {
        socket: dir.path().join("agmend.sock"),
        root: None,
        project: ProjectChoice::None { why: None },
        settings: None,
    };
    let (root, cx) = open(cx, launch);
    let app = dir.path().join("Applications/agmen.app");
    let foreign = dir.path().join("missing-shell/agmend");
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.bundle = Some(Bundle {
                agmend: app.join("Contents/MacOS/agmend"),
                agmen: app.join("Contents/MacOS/agmen"),
                app,
            });
            root.connected(
                Ok((
                    Observation {
                        check: Check::Elsewhere {
                            exe: foreign.clone(),
                        },
                        running: false,
                    },
                    None,
                )),
                window,
                cx,
            );
        })
    });
    redraw(cx);
    root.read_with(cx, |root, _| {
        assert!(matches!(
            root.state(),
            LiveState::NeedsService(Check::Elsewhere { .. })
        ));
        let wording = Wording::of(root.state(), &root.launch.project);
        assert!(wording.body.contains(&foreign.display().to_string()));
        assert!(wording.body.contains("stopped"));
        assert_eq!(wording.command, None);
    });
    assert!(cx.debug_bounds("live:retry").is_some());
    assert!(cx.debug_bounds("live:setup-service").is_none());
    assert!(cx.debug_bounds("live:copy-service-command").is_none());
}

#[test]
fn bundled_service_screen_offers_setup_and_switch() {
    let wording = Wording::of(
        &LiveState::NeedsService(Check::Missing),
        &ProjectChoice::None { why: None },
    );
    assert_eq!(wording.heading, "Start agmend");
    assert_eq!(wording.command, None);
    let wording = Wording::of(
        &LiveState::NeedsService(Check::TemporaryBundle),
        &ProjectChoice::None { why: None },
    );
    assert_eq!(wording.heading, "Move to Applications");
    assert!(wording.body.contains("relaunch"));
}

#[gpui::test]
fn bundled_agmend_down_offers_in_app_setup_without_starting_service(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Applications/agmen.app");
    let agmend = app.join("Contents/MacOS/agmend");
    std::fs::create_dir_all(agmend.parent().unwrap()).unwrap();
    std::fs::write(&agmend, "must not execute before consent").unwrap();
    let launch = LiveLaunch {
        socket: dir.path().join("agmend.sock"),
        root: None,
        project: ProjectChoice::None { why: None },
        settings: None,
    };
    let (root, cx) = open(cx, launch);
    root.update(cx, |root, cx| {
        root.bundle = Some(Bundle {
            app,
            agmend: agmend.clone(),
            agmen: dir.path().join("agmen"),
        });
        root.state = LiveState::NeedsService(Check::Missing);
        cx.notify();
    });
    redraw(cx);
    assert!(cx.debug_bounds("live:setup-service").is_some());
    assert!(cx.debug_bounds("live:copy-service-command").is_some());
    assert!(cx.debug_bounds("live:command").is_some());
    root.read_with(cx, |root, _| {
        let wording = Wording::of(root.state(), &root.launch.project);
        assert_eq!(wording.command, None);
        assert!(wording.note.unwrap().contains("agmend.log"));
    });
    assert_eq!(
        std::fs::read_to_string(agmend).unwrap(),
        "must not execute before consent"
    );
    assert!(!dir.path().join("Library/LaunchAgents").exists());
}

/// A live window on `launch`, drawn once.
fn open(cx: &mut TestAppContext, launch: LiveLaunch) -> (Entity<LiveRoot>, &mut VisualTestContext) {
    init(cx);
    cx.update(crate::keymap::bind_all);
    // The live backend's threads wake the chat's update loop from outside
    // the test scheduler, which it flags as nondeterminism otherwise.
    cx.executor().allow_parking();
    let (root, cx) = cx.add_window_view(|window, cx| LiveRoot::new(launch, None, window, cx));
    cx.update(|window, cx| window.focus(&root.focus_handle(cx), cx));
    redraw(cx);
    (root, cx)
}

/// Runs the window until `done`, failing after 10 s: the backend's
/// threads send from outside the test's executor.
fn wait_until(cx: &mut VisualTestContext, mut done: impl FnMut(&mut VisualTestContext) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        redraw(cx);
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// What the status bar says about the link now.
fn link_text(root: &Entity<LiveRoot>, cx: &mut VisualTestContext) -> Option<String> {
    root.read_with(cx, |root, cx| {
        let workspace = root.workspace()?;
        workspace
            .read(cx)
            .status_link(cx)
            .map(|(text, _)| text.to_string())
    })
}

#[gpui::test]
fn live_without_agmend_says_so(cx: &mut TestAppContext) {
    let mut fake = FakeAgmend::start(console(vec![message(1)]));
    fake.stop();
    let settings = fake.dir.path().join("settings.toml");
    let launch = LiveLaunch {
        socket: fake.path(),
        root: None,
        project: ProjectChoice::Project(project()),
        settings: Some(settings.clone()),
    };
    let (root, cx) = open(cx, launch);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| matches!(root.state(), LiveState::AgmendDown))
    });
    assert!(cx.debug_bounds("live:agmend-down").is_some());
    assert!(cx.debug_bounds("live:command").is_some());
    assert!(cx.debug_bounds("live:retry").is_some());
    let copy = Wording::of(&LiveState::AgmendDown, &ProjectChoice::Project(project()));
    assert_eq!(copy.heading, "agmend isn't running");
    assert_eq!(copy.command, Some("agmend service install"));
    assert_eq!(copy.note, None);
    assert!(copy.retry);
    assert!(!copy.body.contains("service"), "{}", copy.body);
    // Never the fixture: no backend, no chat, no fake people.
    assert!(cx.update(|_, cx| cx.try_global::<BackendHandle>().is_none()));
    assert!(cx.debug_bounds("chat").is_none());
    assert!(cx.debug_bounds("agent:andy/fx1").is_none());
    assert!(
        DesktopSettings::load(&settings)
            .unwrap()
            .last_project
            .is_none()
    );

    // agmend starts; Retry opens the project's chat and remembers it.
    fake.restart();
    click(cx, "live:retry");
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| matches!(root.state(), LiveState::Open(_)))
    });
    assert!(cx.debug_bounds("chat").is_some());
    assert!(cx.debug_bounds("live:agmend-down").is_none());
    assert_eq!(
        DesktopSettings::load(&settings).unwrap().last_project,
        Some(project())
    );
}

#[gpui::test]
fn no_project_says_how_to_join(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let launch = LiveLaunch {
        socket: dir.path().join("agmend.sock"),
        root: None,
        project: ProjectChoice::None { why: None },
        settings: None,
    };
    let (root, cx) = open(cx, launch);
    root.read_with(cx, |root, _| {
        assert!(
            matches!(root.state(), LiveState::NoProject),
            "{:?}",
            root.state()
        );
    });
    assert!(cx.debug_bounds("live:no-project").is_some());
    assert!(cx.debug_bounds("chat").is_none());
    let copy = Wording::of(&LiveState::NoProject, &ProjectChoice::None { why: None });
    assert!(copy.body.contains("agmen join"), "{}", copy.body);
}

#[gpui::test]
fn status_bar_shows_reconnecting(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![message(1)]));
    let launch = LiveLaunch {
        socket: fake.path(),
        root: None,
        project: ProjectChoice::Project(project()),
        settings: None,
    };
    let (root, cx) = open(cx, launch);
    wait_until(cx, |cx| {
        link_text(&root, cx).as_deref() == Some("connected")
    });
    assert!(cx.debug_bounds("status-link").is_some());

    // The hub stream drops: agmend is up and retries it.
    fake.push(&ConsoleEvent::Hub {
        connected: false,
        error: None,
    });
    wait_until(cx, |cx| {
        link_text(&root, cx).as_deref() == Some("reconnecting")
    });

    // The hub refuses for a reason a retry will not fix: the status bar
    // says why.
    fake.push(&ConsoleEvent::Hub {
        connected: false,
        error: Some(ApiError {
            code: ErrorCode::Unauthorized,
            message: "signed out: run agmen login".into(),
        }),
    });
    wait_until(cx, |cx| {
        link_text(&root, cx).as_deref() == Some("offline: signed out: run agmen login")
    });

    // Final review fix 2: agmend ends the watch because the console fell
    // behind; the status bar says so, not "can't reach agmend".
    fake.push(&ConsoleEvent::Hub {
        connected: true,
        error: None,
    });
    wait_until(cx, |cx| {
        link_text(&root, cx).as_deref() == Some("connected")
    });
    fake.end_watches(ErrorCode::Hub, "this console fell 3 events behind");
    wait_until(cx, |cx| {
        link_text(&root, cx).as_deref() == Some("offline: this console fell 3 events behind")
    });

    // agmend itself goes away.
    fake.stop();
    wait_until(cx, |cx| {
        link_text(&root, cx).as_deref()
            == Some("offline: can't reach agmend: nothing answers at its socket")
    });
}

#[gpui::test]
fn the_window_draws_while_agmend_is_slow(cx: &mut TestAppContext) {
    // Review fix 2: the connect runs off the main thread, so the window
    // shows that it is connecting before agmend answers, then goes live.
    let fake = FakeAgmend::start(console(vec![message(1)]));
    fake.edit(|s| s.snapshot_delay = Duration::from_millis(300));
    let launch = LiveLaunch {
        socket: fake.path(),
        root: None,
        project: ProjectChoice::Project(project()),
        settings: None,
    };
    let (root, cx) = open(cx, launch);
    root.read_with(cx, |root, _| {
        assert!(
            matches!(root.state(), LiveState::Connecting),
            "{:?}",
            root.state()
        );
    });
    assert!(cx.debug_bounds("live:connecting").is_some());
    assert!(cx.update(|_, cx| cx.try_global::<BackendHandle>().is_none()));
    // Retry waits for the connect under way.
    let wording = Wording::of(&LiveState::Connecting, &ProjectChoice::Project(project()));
    assert!(!wording.retry);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| matches!(root.state(), LiveState::Open(_)))
    });
    assert!(cx.debug_bounds("chat").is_some());
    assert!(cx.update(|_, cx| cx.try_global::<BackendHandle>().is_some()));
    // The open window has its own selector, not a refusal's.
    assert!(cx.debug_bounds("live:open").is_some());
    assert!(cx.debug_bounds("live:refused").is_none());
    assert!(cx.debug_bounds("live:connecting").is_none());
}

#[gpui::test]
fn app_owned_quit_renders_choice_and_remember_checkbox(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let launch = LiveLaunch {
        socket: dir.path().join("agmend.sock"),
        root: None,
        project: ProjectChoice::None { why: None },
        settings: Some(dir.path().join("settings.toml")),
    };
    let (root, cx) = open(cx, launch);
    cx.update(|_, cx| {
        root.update(cx, |root, cx| {
            root.quit_choice = Some(service::QuitChoice::Ask);
            cx.notify();
        })
    });
    redraw(cx);
    assert!(cx.debug_bounds("quit:dialog").is_some());
    assert!(cx.debug_bounds("quit:keep").is_some());
    assert!(cx.debug_bounds("quit:everything").is_some());
    click(cx, "quit:remember");
    root.read_with(cx, |root, _| assert!(root.quit_remember));
}

#[gpui::test]
fn cancel_quit_from_launch_screen(cx: &mut TestAppContext) {
    check_cancel_quit(cx, false, false, false, false);
}

#[gpui::test]
fn escape_cancels_quit_from_launch_screen(cx: &mut TestAppContext) {
    check_cancel_quit(cx, false, true, false, false);
}

#[gpui::test]
fn cancel_quit_from_workspace(cx: &mut TestAppContext) {
    check_cancel_quit(cx, true, false, false, false);
}

#[gpui::test]
fn escape_cancels_quit_from_workspace(cx: &mut TestAppContext) {
    check_cancel_quit(cx, true, true, false, false);
}

#[gpui::test]
fn closing_window_then_cancel_quit_keeps_window(cx: &mut TestAppContext) {
    check_cancel_quit(cx, true, false, true, false);
}

#[gpui::test]
fn cancel_quit_from_unjoined_folder(cx: &mut TestAppContext) {
    check_cancel_quit(cx, true, false, false, true);
}

#[gpui::test]
fn escape_cancels_quit_from_unjoined_folder(cx: &mut TestAppContext) {
    check_cancel_quit(cx, true, true, false, true);
}

/// A bundled executable that only answers status, without a service manager.
fn status_only_bundle(dir: &std::path::Path) -> Bundle {
    use std::os::unix::fs::PermissionsExt as _;

    let app = dir.join("agmen.app");
    let agmend = app.join("Contents/MacOS/agmend");
    std::fs::create_dir_all(agmend.parent().unwrap()).unwrap();
    std::fs::write(
        &agmend,
        "#!/bin/sh\nprintf '%s\\n' '{\"state\":\"not installed\",\"program\":null}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&agmend, std::fs::Permissions::from_mode(0o700)).unwrap();
    Bundle {
        app,
        agmend,
        agmen: dir.join("agmen"),
    }
}

fn check_cancel_quit(
    cx: &mut TestAppContext,
    workspace: bool,
    escape: bool,
    close_window: bool,
    rejected: bool,
) {
    let dir = tempfile::tempdir().unwrap();
    let bundle = status_only_bundle(dir.path());
    let fake = FakeAgmend::start(console(vec![]));
    fake.edit(|state| {
        state.hello_answer = Some(
            serde_json::json!({"api":1,"version":env!("CARGO_PKG_VERSION"),"exe":bundle.agmend}),
        );
    });
    let path = dir.path().join("settings.toml");
    let launch = LiveLaunch {
        socket: fake.path(),
        root: None,
        project: if workspace {
            ProjectChoice::Project(project())
        } else {
            ProjectChoice::None { why: None }
        },
        settings: Some(path.clone()),
    };
    let (root, cx) = open(cx, launch);
    if workspace {
        wait_until(cx, |cx| {
            root.read_with(cx, |root, _| matches!(root.state(), LiveState::Open(_)))
        });
    }
    if rejected {
        cx.simulate_keystrokes("cmd-o");
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|_| Some(vec![fake.dir.path().to_owned()]));
        wait_until(cx, |cx| cx.debug_bounds("live:not-a-project").is_some());
    }
    DesktopSettings {
        last_project: Some(project()),
        ..DesktopSettings::default()
    }
    .save(&path)
    .unwrap();
    let settings_before = std::fs::read(&path).unwrap();
    root.update_in(cx, |root, _, _| root.bundle = Some(bundle));
    // The same quit handlers main installs for a live window.
    cx.update(|window, cx| super::install_quit_handlers(&root, window, cx));
    if close_window {
        assert!(!cx.simulate_close());
    } else {
        cx.simulate_keystrokes("cmd-q");
    }
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| {
            root.quit_choice == Some(service::QuitChoice::Ask)
        })
    });
    click(cx, "quit:remember");
    assert!(root.read_with(cx, |root, _| root.quit_remember));
    if escape {
        cx.simulate_keystrokes("escape");
        redraw(cx);
    } else {
        click(cx, "quit:cancel");
    }
    cx.run_until_parked();
    root.read_with(cx, |root, _| {
        assert!(root.quit_choice.is_none(), "cancel must dismiss the dialog");
        assert!(!root.quit_approved, "cancel must not approve quitting");
        assert!(!root.quit_busy);
        assert!(!root.quit_requested);
        assert!(!root.quit_remember, "cancel must discard the checkbox");
        assert!(root._quitting.is_none());
    });
    assert!(cx.debug_bounds("quit:dialog").is_none());
    assert_eq!(cx.debug_bounds("live:not-a-project").is_some(), rejected);
    assert_eq!(std::fs::read(&path).unwrap(), settings_before);
    fake.edit(|state| assert_eq!(state.shutdown_calls, 0));
    assert!(AgmendBackend::probe(&fake.path()).is_ok());
    assert_eq!(cx.windows().len(), 1);
    if workspace {
        assert!(cx.debug_bounds("chat").is_some());
    }
    // A second close still asks, with the remembered checkbox cleared.
    assert!(!cx.simulate_close());
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| {
            root.quit_choice == Some(service::QuitChoice::Ask)
        })
    });
    assert!(!root.read_with(cx, |root, _| root.quit_remember));
    // Return keeps its existing default: quit the app, leave agmend running.
    cx.simulate_keystrokes("enter");
    assert!(root.read_with(cx, |root, _| root.quit_approved));
    assert_eq!(std::fs::read(&path).unwrap(), settings_before);
    fake.edit(|state| assert_eq!(state.shutdown_calls, 0));
    assert!(AgmendBackend::probe(&fake.path()).is_ok());
}

#[gpui::test]
fn quit_path_checks_daemon_ownership_and_persists_keep_running(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let bundle = status_only_bundle(dir.path());
    let agmend = bundle.agmend.clone();
    let fake = FakeAgmend::start(console(vec![]));
    let path = dir.path().join("settings.toml");
    let launch = LiveLaunch {
        socket: fake.path(),
        root: None,
        project: ProjectChoice::None { why: None },
        settings: Some(path.clone()),
    };
    let (root, cx) = open(cx, launch);
    cx.update(|_, cx| {
        root.update(cx, |root, _| {
            root.bundle = Some(bundle);
        })
    });
    // Foreign and unknown identity skip the dialog even with remembered Stop.
    for exe in [serde_json::json!("/shell/agmend"), serde_json::Value::Null] {
        DesktopSettings {
            quit_keep_running: Some(false),
            ..DesktopSettings::default()
        }
        .save(&path)
        .unwrap();
        fake.edit(|state| {
            state.hello_answer =
                Some(serde_json::json!({"api":1,"version":env!("CARGO_PKG_VERSION"),"exe":exe}))
        });
        cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.quit_approved = false;
                root.request_quit(window, cx);
            })
        });
        wait_until(cx, |cx| root.read_with(cx, |root, _| root.quit_approved));
        assert!(root.read_with(cx, |root, _| root.quit_choice.is_none()));
        assert!(AgmendBackend::probe(&fake.path()).is_ok());
    }
    DesktopSettings::default().save(&path).unwrap();
    fake.edit(|state| {
        state.hello_answer =
            Some(serde_json::json!({"api":1,"version":env!("CARGO_PKG_VERSION"),"exe":agmend}))
    });
    cx.update(|window, cx| {
        root.update(cx, |root, cx| {
            root.quit_approved = false;
            root.request_quit(window, cx);
        })
    });
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| {
            root.quit_choice == Some(service::QuitChoice::Ask)
        })
    });
    redraw(cx);
    click(cx, "quit:remember");
    click(cx, "quit:keep");
    assert!(root.read_with(cx, |root, _| root.quit_approved));
    assert_eq!(
        DesktopSettings::load(&path).unwrap().quit_keep_running,
        Some(true)
    );
    assert!(AgmendBackend::probe(&fake.path()).is_ok());
}

fn membership_fixture(
    root: &Entity<LiveRoot>,
    cx: &mut VisualTestContext,
    folder: &std::path::Path,
    projects: serde_json::Value,
) {
    std::fs::create_dir_all(folder).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(folder)
            .status()
            .unwrap()
            .success()
    );
    let login = folder.parent().unwrap().join("member-login.json");
    std::fs::write(
        &login,
        r#"{"hub_url":"https://hub.example/","human_id":"andyandy","handle":"andy"}"#,
    )
    .unwrap();
    root.update_in(cx, |root, _, _| {
        root.onboarding.login_path = Some(login);
        root.onboarding.program = Some(PathBuf::from("/injected/agmen"));
        let folder = folder.to_owned();
        root.onboarding.helper = std::sync::Arc::new(move |_, args, timeout, cwd, input, _| {
            assert_eq!(timeout, Duration::from_secs(120));
            assert_eq!(cwd, Some(folder.as_path()));
            assert!(input.is_empty());
            let mut output = helper_success();
            if args == ["whoami", "--json"].map(std::ffi::OsString::from) {
                output.stdout = serde_json::json!({"projects":projects}).to_string().into_bytes();
            } else if args == ["project", "create", "new-folder"].map(std::ffi::OsString::from) {
                output.stdout = br#"{"project":"project1","name":"new-folder","tenant":"tenant01","role":"admin"}"#.to_vec();
            } else {
                assert_eq!(args, ["join", "--project", "project1", "--no-setup"].map(std::ffi::OsString::from));
                joined_folder(&folder);
            }
            Ok(output)
        });
    });
}

fn membership(role: &str) -> serde_json::Value {
    serde_json::json!({"tenant":"tenant01","project":"project1","name":"marked","role":role})
}

#[gpui::test]
fn join_helpers_skip_setup(cx: &mut TestAppContext) {
    for by_invite in [false, true] {
        let fake = FakeAgmend::start(console(vec![]));
        let folder = fake.dir.path().join("join-here");
        let (root, cx) = open(cx, empty_launch(&fake));
        membership_fixture(&root, cx, &folder, serde_json::json!([]));
        let joined = folder.clone();
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = called.clone();
        root.update_in(cx, |root, _, _| {
            root.onboarding.helper = std::sync::Arc::new(move |_, args, _, _, _, _| {
                if args == ["whoami", "--json"].map(std::ffi::OsString::from) {
                    let mut out = helper_success();
                    out.stdout = serde_json::json!({"projects": if by_invite { vec![] } else { vec![membership("member")] }}).to_string().into_bytes();
                    return Ok(out);
                }
                let expected = if by_invite {
                    vec!["join", "--stdin", "--no-setup"]
                } else {
                    vec!["join", "--project", "project1", "--no-setup"]
                };
                assert_eq!(args, expected.into_iter().map(std::ffi::OsString::from).collect::<Vec<_>>());
                observed.store(true, std::sync::atomic::Ordering::SeqCst);
                joined_folder(&joined);
                Ok(helper_success())
            });
        });
        pick(&root, cx, &folder);
        if by_invite {
            wait_until(cx, |cx| {
                root.read_with(cx, |root, _| root.onboarding.task.is_none())
            });
            root.update_in(cx, |root, window, cx| {
                root.onboarding
                    .invite
                    .update(cx, |input, cx| input.set_text("test-invite", window, cx));
            });
            click(cx, "live:join");
        }
        wait_until(cx, |cx| {
            root.read_with(cx, |root, _| root.workspace().is_some())
        });
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    }
}

#[gpui::test]
fn one_membership_auto_joins_and_opens(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("new-folder");
    let (root, cx) = open(cx, empty_launch(&fake));
    membership_fixture(&root, cx, &folder, serde_json::json!([membership("admin")]));
    pick(&root, cx, &folder);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    assert_eq!(
        root.read_with(cx, |root, _| root.service_notice.clone()),
        Some("Joined new-folder to marked".into())
    );
}

#[gpui::test]
fn several_memberships_offer_picker_and_join(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("new-folder");
    let (root, cx) = open(cx, empty_launch(&fake));
    let other = serde_json::json!({"tenant":"tenant01","project":"project2","name":"other","role":"member"});
    membership_fixture(
        &root,
        cx,
        &folder,
        serde_json::json!([membership("admin"), other]),
    );
    pick(&root, cx, &folder);
    wait_until(cx, |cx| cx.debug_bounds("live:project-choice:0").is_some());
    assert!(cx.debug_bounds("live:project-choice:1").is_some());
    assert!(!folder.join(".agmen/identity.json").exists());
    click(cx, "live:project-choice:0");
    click(cx, "live:join-project");
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
}

#[gpui::test]
fn no_memberships_offer_create_and_invite_then_create_joins_and_opens(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("new-folder");
    let (root, cx) = open(cx, empty_launch(&fake));
    membership_fixture(&root, cx, &folder, serde_json::json!([]));
    pick(&root, cx, &folder);
    wait_until(cx, |cx| cx.debug_bounds("live:create-project").is_some());
    assert!(cx.debug_bounds("live:invite-code").is_some());
    click(cx, "live:create-project");
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    assert!(cx.debug_bounds("live:invite-people").is_some());
}

#[gpui::test]
fn invite_people_admin_and_coordinator_get_code_and_copy_buttons_member_has_no_action(
    cx: &mut TestAppContext,
) {
    for role in ["admin", "coordinator", "member"] {
        let fake = FakeAgmend::start(console(vec![]));
        let folder = fake.dir.path().join("new-folder");
        let (root, cx) = open(cx, empty_launch(&fake));
        membership_fixture(&root, cx, &folder, serde_json::json!([membership(role)]));
        pick(&root, cx, &folder);
        wait_until(cx, |cx| {
            root.read_with(cx, |root, _| root.workspace().is_some())
        });
        if role == "member" {
            assert!(cx.debug_bounds("live:invite-people").is_none());
            continue;
        }
        root.update_in(cx, |root, _, _| {
            root.onboarding.helper = std::sync::Arc::new(|_, args, _, _, _, _| {
                assert_eq!(args, ["invite", "--project", "project1"].map(std::ffi::OsString::from));
                let mut out = helper_success();
                out.stdout = b"single-use invite to project project1 as member, open until tomorrow:\n  agmen join test-code\n".to_vec();
                Ok(out)
            });
        });
        click(cx, "live:invite-people");
        wait_until(cx, |cx| cx.debug_bounds("live:invite-result").is_some());
        assert!(cx.debug_bounds("live:copy-invite-code").is_some());
        assert!(cx.debug_bounds("live:copy-invite-command").is_some());
        // Clipboard writes are deliberately not exercised.
    }
}

#[gpui::test]
fn membership_lookup_failure_offers_retry_without_offering_create(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("new-folder");
    let (root, cx) = open(cx, empty_launch(&fake));
    membership_fixture(&root, cx, &folder, serde_json::json!([]));
    root.update_in(cx, |root, _, _| {
        root.onboarding.helper = std::sync::Arc::new(|_, _, _, _, _, _| {
            let mut out = helper_success();
            out.stdout = b"broken JSON".to_vec();
            Ok(out)
        });
    });
    pick(&root, cx, &folder);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.onboarding.task.is_none())
    });
    assert!(cx.debug_bounds("live:project-error").is_some());
    assert!(cx.debug_bounds("live:create-project").is_none());
    assert!(cx.debug_bounds("live:retry-projects").is_some());
}

#[gpui::test]
fn expired_membership_login_returns_to_sign_in(cx: &mut TestAppContext) {
    use std::os::unix::process::ExitStatusExt as _;
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("new-folder");
    let (root, cx) = open(cx, empty_launch(&fake));
    membership_fixture(&root, cx, &folder, serde_json::json!([]));
    root.update_in(cx, |root, _, _| {
        root.onboarding.helper = std::sync::Arc::new(|_, _, _, _, _, _| {
            let mut out = helper_success();
            out.status = std::process::ExitStatus::from_raw(3 << 8);
            out.stderr = b"sign in again with agmen login".to_vec();
            Ok(out)
        });
    });
    pick(&root, cx, &folder);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.onboarding.task.is_none())
    });
    assert!(cx.debug_bounds("live:sign-in").is_some());
    assert!(cx.debug_bounds("live:project-error").is_some());
}

#[gpui::test]
fn opening_an_already_joined_folder_loads_role_without_joining_again(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("joined-folder");
    let (root, cx) = open(cx, empty_launch(&fake));
    membership_fixture(&root, cx, &folder, serde_json::json!([]));
    joined_folder(&folder);
    root.update_in(cx, |root, _, _| {
        root.onboarding.helper = std::sync::Arc::new(|_, args, _, _, _, _| {
            assert_eq!(args, ["whoami", "--json"].map(std::ffi::OsString::from));
            let mut out = helper_success();
            out.stdout = serde_json::json!({"projects":[membership("admin")]})
                .to_string()
                .into_bytes();
            Ok(out)
        });
    });
    pick(&root, cx, &folder);
    wait_until(cx, |cx| cx.debug_bounds("live:invite-people").is_some());
    assert!(root.read_with(cx, |root, _| root.workspace().is_some()));
}

#[gpui::test]
fn project_from_settings_can_invite_without_a_folder(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let mut launch = empty_launch(&fake);
    launch.project = ProjectChoice::Project(project());
    let (root, cx) = open(cx, launch);
    root.update_in(cx, |root, window, cx| {
        root.onboarding.signed_in = true;
        root.onboarding.program = Some(PathBuf::from("/injected/agmen"));
        root.onboarding.helper = std::sync::Arc::new(|_, args, _, cwd, _, _| {
            assert!(cwd.is_none());
            assert_eq!(args, ["whoami", "--json"].map(std::ffi::OsString::from));
            let mut out = helper_success();
            out.stdout = serde_json::json!({"projects":[membership("admin")]})
                .to_string()
                .into_bytes();
            Ok(out)
        });
        root.list_memberships(window, cx);
    });
    wait_until(cx, |cx| cx.debug_bounds("live:invite-people").is_some());
}

#[gpui::test]
fn create_edits_name_and_tenant_protects_flags_and_keeps_folder_on_failure(
    cx: &mut TestAppContext,
) {
    use std::os::unix::process::ExitStatusExt as _;
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("new-folder");
    let (root, cx) = open(cx, empty_launch(&fake));
    membership_fixture(&root, cx, &folder, serde_json::json!([]));
    pick(&root, cx, &folder);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.onboarding.task.is_none())
    });
    root.update_in(cx, |root, window, cx| {
        assert_eq!(root.onboarding.project_name.read(cx).text(), "new-folder");
        root.onboarding
            .project_name
            .update(cx, |input, cx| input.set_text("--help", window, cx));
        root.onboarding
            .tenant
            .update(cx, |input, cx| input.set_text("tenant01", window, cx));
        root.onboarding.helper = std::sync::Arc::new(|_, args, _, _, _, _| {
            assert_eq!(
                args,
                ["project", "create", "--tenant", "tenant01", "--", "--help"]
                    .map(std::ffi::OsString::from)
            );
            let mut out = helper_success();
            out.status = std::process::ExitStatus::from_raw(1 << 8);
            out.stderr = b"a project with that name already exists".to_vec();
            Ok(out)
        });
    });
    click(cx, "live:create-project");
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.onboarding.task.is_none())
    });
    assert!(root.read_with(cx, |root, _| {
        root.service_error
            .as_ref()
            .is_some_and(|error| error.contains("already exists"))
    }));
    assert!(root.read_with(cx, |root, _| {
        root.rejected.as_ref().is_some_and(|f| f.folder == folder)
    }));
    assert!(!folder.join(".agmen/identity.json").exists());
}

#[gpui::test]
fn created_project_can_retry_join_without_creating_again(cx: &mut TestAppContext) {
    use std::os::unix::process::ExitStatusExt as _;
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("new-folder");
    let (root, cx) = open(cx, empty_launch(&fake));
    membership_fixture(&root, cx, &folder, serde_json::json!([]));
    pick(&root, cx, &folder);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.onboarding.task.is_none())
    });
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let joined = folder.clone();
    root.update_in(cx, |root, _, _| {
        root.onboarding.helper = std::sync::Arc::new(move |_, args, _, _, _, _| {
            let mut out = helper_success();
            if args == ["project", "create", "new-folder"].map(std::ffi::OsString::from) {
                out.stdout = serde_json::json!(membership("admin"))
                    .to_string()
                    .into_bytes();
            } else {
                assert_eq!(
                    args,
                    ["join", "--project", "project1", "--no-setup"].map(std::ffi::OsString::from)
                );
                if attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    out.status = std::process::ExitStatus::from_raw(1 << 8);
                    out.stderr = b"test join unavailable".to_vec();
                } else {
                    joined_folder(&joined);
                }
            }
            Ok(out)
        });
    });
    click(cx, "live:create-project");
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.onboarding.task.is_none())
    });
    assert!(cx.debug_bounds("live:create-project").is_none());
    assert!(cx.debug_bounds("live:join-project").is_some());
    click(cx, "live:join-project");
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
}

#[gpui::test]
fn switching_joined_projects_refreshes_membership_for_invites(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let first = fake.dir.path().join("first");
    let second = fake.dir.path().join("second");
    let (root, cx) = open(cx, empty_launch(&fake));
    membership_fixture(&root, cx, &first, serde_json::json!([membership("admin")]));
    pick(&root, cx, &first);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    joined_folder(&second);
    let path = second.join(".agmen/identity.json");
    let mut identity: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    identity["project"] = serde_json::json!("project2");
    std::fs::write(path, identity.to_string()).unwrap();
    root.update_in(cx, |root, _, _| {
        root.onboarding.helper = std::sync::Arc::new(|_, args, _, _, _, _| {
            assert_eq!(args, ["whoami", "--json"].map(std::ffi::OsString::from));
            let mut member = membership("admin");
            member["project"] = serde_json::json!("project2");
            let mut out = helper_success();
            out.stdout = serde_json::json!({"projects":[member]})
                .to_string()
                .into_bytes();
            Ok(out)
        });
    });
    root.update_in(cx, |root, window, cx| root.open_folder(second, window, cx));
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| {
            root.launch.project == ProjectChoice::Project("project2".parse().unwrap())
                && root.workspace().is_some()
        }) && cx.debug_bounds("live:invite-people").is_some()
    });
}

#[gpui::test]
fn dismissing_pending_auto_join_does_not_reopen_folder(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("new-folder");
    let (root, cx) = open(cx, empty_launch(&fake));
    membership_fixture(&root, cx, &folder, serde_json::json!([membership("admin")]));
    let (release, wait) = std::sync::mpsc::sync_channel(1);
    let wait = std::sync::Mutex::new(wait);
    let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let in_join = started.clone();
    let joined = folder.clone();
    root.update_in(cx, |root, _, _| {
        root.onboarding.helper = std::sync::Arc::new(move |_, args, _, _, _, _| {
            let mut out = helper_success();
            if args == ["whoami", "--json"].map(std::ffi::OsString::from) {
                out.stdout = serde_json::json!({"projects":[membership("admin")]})
                    .to_string()
                    .into_bytes();
            } else {
                in_join.store(true, std::sync::atomic::Ordering::SeqCst);
                wait.lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                joined_folder(&joined);
            }
            Ok(out)
        });
    });
    pick(&root, cx, &folder);
    wait_until(cx, |_| started.load(std::sync::atomic::Ordering::SeqCst));
    click(cx, "live:keep-project");
    release.send(()).unwrap();
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| {
            root.onboarding.task.is_none()
                && root._resolving.is_none()
                && root._connecting.is_none()
        })
    });
    assert!(root.read_with(cx, |root, _| root.workspace().is_none()));
}

#[gpui::test]
fn late_invite_join_resumes_membership_lookup_for_next_folder(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let first = fake.dir.path().join("first");
    let second = fake.dir.path().join("second");
    let (root, cx) = open(cx, empty_launch(&fake));
    membership_fixture(&root, cx, &first, serde_json::json!([]));
    std::fs::create_dir(&second).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&second)
            .status()
            .unwrap()
            .success()
    );
    pick(&root, cx, &first);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.onboarding.task.is_none())
    });
    let (release, wait) = std::sync::mpsc::sync_channel(1);
    let wait = std::sync::Mutex::new(wait);
    let old = first.clone();
    let current = second.clone();
    root.update_in(cx, |root, window, cx| {
        root.onboarding
            .invite
            .update(cx, |input, cx| input.set_text("test-invite", window, cx));
        root.onboarding.helper = std::sync::Arc::new(move |_, args, _, cwd, _, _| {
            let mut out = helper_success();
            if args == ["join", "--stdin", "--no-setup"].map(std::ffi::OsString::from) {
                wait.lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                joined_folder(&old);
            } else if args == ["whoami", "--json"].map(std::ffi::OsString::from) {
                out.stdout = serde_json::json!({"projects":[membership("admin")]})
                    .to_string()
                    .into_bytes();
            } else {
                assert_eq!(cwd, Some(current.as_path()));
                joined_folder(&current);
            }
            Ok(out)
        });
    });
    click(cx, "live:join");
    pick(&root, cx, &second);
    release.send(()).unwrap();
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    assert_eq!(
        root.read_with(cx, |root, _| root.launch.root.clone()),
        Some(second.canonicalize().unwrap())
    );
}

#[gpui::test]
fn late_login_does_not_lookup_or_join_non_git_folder(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("plain");
    std::fs::create_dir(&folder).unwrap();
    let (root, cx) = open(cx, empty_launch(&fake));
    pick(&root, cx, &folder);
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let called = calls.clone();
    root.update_in(cx, |root, window, cx| {
        root.onboarding.signed_in = true;
        root.onboarding.program = Some(PathBuf::from("/injected/agmen"));
        root.onboarding.helper = std::sync::Arc::new(move |_, _, _, _, _, _| {
            called.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(helper_projects())
        });
        root.list_memberships(window, cx);
    });
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.onboarding.task.is_none())
    });
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[gpui::test]
fn keeping_current_project_after_rejected_folder_restores_invite_action(cx: &mut TestAppContext) {
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("joined");
    let plain = fake.dir.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let (root, cx) = open(cx, empty_launch(&fake));
    membership_fixture(&root, cx, &folder, serde_json::json!([membership("admin")]));
    pick(&root, cx, &folder);
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    root.update_in(cx, |root, window, cx| {
        root.onboarding.helper = std::sync::Arc::new(|_, args, _, _, _, _| {
            assert_eq!(args, ["whoami", "--json"].map(std::ffi::OsString::from));
            let mut out = helper_success();
            out.stdout = serde_json::json!({"projects":[membership("admin")]})
                .to_string()
                .into_bytes();
            Ok(out)
        });
        root.open_folder(plain, window, cx);
    });
    wait_until(cx, |cx| cx.debug_bounds("live:not-git").is_some());
    click(cx, "live:keep-project");
    wait_until(cx, |cx| cx.debug_bounds("live:invite-people").is_some());
}

#[gpui::test]
fn reopening_same_folder_during_auto_join_opens_without_joining_twice(cx: &mut TestAppContext) {
    use std::os::unix::process::ExitStatusExt as _;
    let fake = FakeAgmend::start(console(vec![]));
    let folder = fake.dir.path().join("new-folder");
    let (root, cx) = open(cx, empty_launch(&fake));
    membership_fixture(&root, cx, &folder, serde_json::json!([membership("admin")]));
    let (release, wait) = std::sync::mpsc::sync_channel(1);
    let wait = std::sync::Mutex::new(wait);
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = attempts.clone();
    let joined = folder.clone();
    root.update_in(cx, |root, _, _| {
        root.onboarding.helper = std::sync::Arc::new(move |_, args, _, _, _, _| {
            let mut out = helper_success();
            if args == ["whoami", "--json"].map(std::ffi::OsString::from) {
                out.stdout = serde_json::json!({"projects":[membership("admin")]})
                    .to_string()
                    .into_bytes();
            } else if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                wait.lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                joined_folder(&joined);
            } else {
                out.status = std::process::ExitStatus::from_raw(1 << 8);
                out.stderr = b"this worktree is already joined".to_vec();
            }
            Ok(out)
        });
    });
    pick(&root, cx, &folder);
    wait_until(cx, |_| {
        attempts.load(std::sync::atomic::Ordering::SeqCst) == 1
    });
    pick(&root, cx, &folder);
    release.send(()).unwrap();
    wait_until(cx, |cx| {
        root.read_with(cx, |root, _| root.workspace().is_some())
    });
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
}
