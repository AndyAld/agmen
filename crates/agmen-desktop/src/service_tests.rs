use super::*;
use crate::bundle::Bundle;
use agmen_local::{API, HelloAnswer, ReadyAnswer};
use std::ffi::OsString;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn bundle(root: &Path) -> Bundle {
    let app = root.join("Applications/agmen.app");
    let agmend = app.join("Contents/MacOS/agmend");
    std::fs::create_dir_all(agmend.parent().unwrap()).unwrap();
    std::fs::write(&agmend, "fixture").unwrap();
    Bundle {
        agmen: app.join("Contents/MacOS/agmen"),
        app,
        agmend,
    }
}

fn answer(code: i32, text: &str) -> std::process::Output {
    std::process::Output {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: text.as_bytes().to_vec(),
        stderr: b"manager failure".to_vec(),
    }
}

fn hello(exe: &Path, version: &str) -> HelloAnswer {
    HelloAnswer {
        api: API,
        version: Some(version.into()),
        build: Some("test".into()),
        exe: Some(exe.into()),
    }
}

#[test]
fn first_run_installs_service_only_after_consent() {
    let home = tempfile::tempdir().unwrap();
    let app = bundle(home.path());
    assert_eq!(classify(Some(&app), None, None, None, None), Check::Missing);
    let mut calls = Vec::new();
    let mut runner = |program: &Path, args: &[OsString], _: Duration| {
        calls.push((program.to_path_buf(), args.to_vec()));
        Ok(answer(0, r#"{"state":"not installed","program":null}"#))
    };
    install_checked(&app, &Check::Missing, None, &mut runner).unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, app.agmend);
    assert_eq!(
        calls[0].1,
        vec![OsString::from("service"), "status".into(), "--json".into()]
    );
    assert_eq!(
        calls[1].1,
        vec![
            OsString::from("service"),
            "install".into(),
            "--program".into(),
            app.agmend.into_os_string()
        ]
    );
}

#[test]
fn service_install_failure_shows_command_and_log() {
    let home = tempfile::tempdir().unwrap();
    let app = bundle(home.path());
    let error = install_checked(&app, &Check::Missing, None, &mut |_, args, _| {
        Ok(if args[1] == "status" {
            answer(0, r#"{"state":"not installed","program":null}"#)
        } else {
            answer(1, "")
        })
    })
    .unwrap_err();
    let text = error.to_string();
    assert!(text.contains("manager failure"));
    assert!(text.contains("service install"));
    assert!(text.contains("Library/Logs/agmen/agmend.log"));
}

#[test]
fn skewed_agmend_offers_switch_and_keeps_one_service() {
    let home = tempfile::tempdir().unwrap();
    let app = bundle(home.path());
    let check = classify(
        Some(&app),
        Some(&hello(Path::new("/foreign/agmend"), "old")),
        None,
        None,
        None,
    );
    assert!(matches!(check, Check::Skewed { .. }));
    let mut calls = 0;
    install_checked(&app, &check, None, &mut |_, args, _| {
        if args[1] == "install" {
            calls += 1;
        }
        assert_eq!(args[0], "service");
        Ok(answer(
            0,
            r#"{"state":"stopped","program":"/foreign/agmend"}"#,
        ))
    })
    .unwrap();
    assert_eq!(calls, 1);
}

#[test]
fn same_version_elsewhere_connects_with_a_notice() {
    let home = tempfile::tempdir().unwrap();
    let app = bundle(home.path());
    let ready = ReadyAnswer {
        ready: true,
        schemas: Default::default(),
    };
    assert_eq!(
        classify(
            Some(&app),
            Some(&hello(
                Path::new("/foreign/agmend"),
                env!("CARGO_PKG_VERSION")
            )),
            Some(&ready),
            None,
            None
        ),
        Check::Elsewhere {
            exe: "/foreign/agmend".into()
        }
    );
}

#[test]
fn moved_app_reregisters_service() {
    let home = tempfile::tempdir().unwrap();
    let app = bundle(home.path());
    let status = Status {
        state: "stopped".into(),
        program: Some(home.path().join("Gone.app/Contents/MacOS/agmend")),
    };
    let check = classify(Some(&app), None, None, Some(&status), None);
    assert!(matches!(check, Check::Moved { .. }));
    install_checked(&app, &check, None, &mut |_, _, _| {
        Ok(answer(
            0,
            r#"{"state":"stopped","program":"/old/agmen.app/Contents/MacOS/agmend"}"#,
        ))
    })
    .unwrap();
}

#[test]
fn mounted_dmg_refuses_service_setup() {
    let app = Bundle {
        app: "/Volumes/agmen/agmen.app".into(),
        agmend: "/Volumes/agmen/agmen.app/Contents/MacOS/agmend".into(),
        agmen: PathBuf::new(),
    };
    assert_eq!(
        classify(Some(&app), None, None, None, None),
        Check::TemporaryBundle
    );
    let error = install_checked(&app, &Check::Missing, None, &mut |_, _, _| {
        panic!("must not run")
    })
    .unwrap_err();
    assert!(error.to_string().contains("Move to Applications"));
}

#[test]
fn translocated_app_refuses_service_setup() {
    let app = Bundle {
        app: "/private/var/AppTranslocation/id/agmen.app".into(),
        agmend: "/private/var/AppTranslocation/id/agmen.app/Contents/MacOS/agmend".into(),
        agmen: PathBuf::new(),
    };
    assert_eq!(
        classify(Some(&app), None, None, None, None),
        Check::TemporaryBundle
    );
    assert!(
        install_checked(&app, &Check::Missing, None, &mut |_, _, _| panic!(
            "must not run"
        ))
        .is_err()
    );
}

#[test]
fn temporary_bundle_offers_move_when_running_daemon_is_incompatible() {
    let app = Bundle {
        app: "/Volumes/agmen/agmen.app".into(),
        agmend: "/Volumes/agmen/agmen.app/Contents/MacOS/agmend".into(),
        agmen: PathBuf::new(),
    };
    for version in ["old", env!("CARGO_PKG_VERSION")] {
        let check = classify(
            Some(&app),
            Some(&hello(Path::new("/foreign/agmend"), version)),
            None,
            None,
            None,
        );
        assert_eq!(check, Check::TemporaryBundle);
        assert_eq!(check.action(), Some("Move to Applications"));
    }
}

#[test]
fn same_version_foreign_service_is_not_repointed() {
    let home = tempfile::tempdir().unwrap();
    let app = bundle(home.path());
    let check = Check::Elsewhere {
        exe: "/foreign/agmend".into(),
    };
    assert!(
        install_checked(&app, &check, None, &mut |_, _, _| panic!(
            "must not repoint"
        ))
        .is_err()
    );
}

#[test]
fn stopped_foreign_receipt_keeps_its_service() {
    let home = tempfile::tempdir().unwrap();
    let app = bundle(home.path());
    let program = home.path().join("shell/agmend");
    std::fs::create_dir_all(program.parent().unwrap()).unwrap();
    std::fs::write(&program, "fixture").unwrap();
    let receipt = agmen_client::InstallReceipt {
        channel: "shell".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        owned_paths: vec![program.clone()],
    };
    let status = Status {
        state: "stopped".into(),
        program: Some(program.clone()),
    };
    let check = classify(Some(&app), None, None, Some(&status), Some(&receipt));
    assert_eq!(check, Check::Elsewhere { exe: program });
    assert!(
        install_checked(&app, &check, Some(&receipt), &mut |_, _, _| panic!(
            "must not repoint"
        ))
        .is_err()
    );
    std::fs::remove_file(&receipt.owned_paths[0]).unwrap();
    let check = classify(Some(&app), None, None, Some(&status), Some(&receipt));
    assert!(matches!(check, Check::Elsewhere { .. }));
    assert!(
        install_checked(&app, &check, Some(&receipt), &mut |_, _, _| panic!(
            "must not repoint missing foreign service"
        ))
        .is_err()
    );
}

#[test]
fn same_version_foreign_unready_service_is_not_repointed() {
    let home = tempfile::tempdir().unwrap();
    let app = bundle(home.path());
    let check = classify(
        Some(&app),
        Some(&hello(
            Path::new("/foreign/agmend"),
            env!("CARGO_PKG_VERSION"),
        )),
        None,
        None,
        None,
    );
    assert!(
        install_checked(&app, &check, None, &mut |_, _, _| panic!(
            "must not repoint unready service"
        ))
        .is_err()
    );
}

#[test]
fn configured_foreign_receipt_blocks_repoint_even_when_bundled_process_answers() {
    let home = tempfile::tempdir().unwrap();
    let app = bundle(home.path());
    let foreign = home.path().join("shell/agmend");
    let receipt = agmen_client::InstallReceipt {
        channel: "shell".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        owned_paths: vec![foreign.clone()],
    };
    let check = Check::Skewed {
        running: Some(env!("CARGO_PKG_VERSION").into()),
        ours: env!("CARGO_PKG_VERSION").into(),
        exe: Some(app.agmend.clone()),
    };
    let mut installs = 0;
    let result = install_checked(&app, &check, Some(&receipt), &mut |_, args, _| {
        if args[1] == "install" {
            installs += 1;
        }
        Ok(answer(
            0,
            &serde_json::json!({"state":"stopped","program":foreign}).to_string(),
        ))
    });
    assert!(result.is_err());
    assert_eq!(installs, 0);
}

#[test]
fn service_check_reads_stopped_configured_program_without_hello() {
    let home = tempfile::tempdir().unwrap();
    let app = bundle(home.path());
    let fake = crate::backend::fake_agmend::FakeAgmend::start(
        crate::backend::fake_agmend::console(vec![]),
    );
    fake.stop();
    let missing = home.path().join("Moved.app/Contents/MacOS/agmend");
    let mut calls = 0;
    let check = check_with_receipt(&app, &fake.path(), None, &mut |program, args, _| {
        calls += 1;
        assert_eq!(program, app.agmend);
        assert_eq!(
            args,
            &[OsString::from("service"), "status".into(), "--json".into()]
        );
        Ok(answer(
            0,
            &serde_json::json!({"state":"stopped","program":missing}).to_string(),
        ))
    })
    .unwrap();
    assert_eq!(
        check.check,
        Check::Moved {
            configured: missing
        }
    );
    assert_eq!(calls, 1);
}

#[test]
fn service_check_reuses_healthy_foreign_socket_without_manager_calls() {
    let home = tempfile::tempdir().unwrap();
    let app = bundle(home.path());
    let fake = crate::backend::fake_agmend::FakeAgmend::start(
        crate::backend::fake_agmend::console(vec![]),
    );
    let check = check_with_receipt(&app, &fake.path(), None, &mut |_, _, _| {
        panic!("healthy foreign service must be reused")
    })
    .unwrap();
    assert_eq!(
        check.check,
        Check::Elsewhere {
            exe: "/fake/agmend".into()
        }
    );
}
