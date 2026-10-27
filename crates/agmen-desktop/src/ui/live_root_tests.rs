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

/// A live window on `launch`, drawn once.
fn open(cx: &mut TestAppContext, launch: LiveLaunch) -> (Entity<LiveRoot>, &mut VisualTestContext) {
    init(cx);
    // The live backend's threads wake the chat's update loop from outside
    // the test scheduler, which it flags as nondeterminism otherwise.
    cx.executor().allow_parking();
    let (root, cx) = cx.add_window_view(|window, cx| LiveRoot::new(launch, window, cx));
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
    // Final review fix 3: `agmend service install` is not built, and
    // agmend refuses it; the command that works is plain `agmend`.
    assert_eq!(copy.command, Some("agmend"));
    assert_eq!(
        copy.note.as_deref(),
        Some("Run agmend in a terminal and leave it open.")
    );
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
}
