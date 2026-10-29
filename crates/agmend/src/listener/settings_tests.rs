use super::*;

fn project() -> ProjectId {
    "project1".parse().unwrap()
}

fn file() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    (dir, path)
}

fn per_delegation(cents: u64) -> ListenerSettings {
    ListenerSettings {
        per_delegation: UsdCents(cents),
        ..ListenerSettings::default()
    }
}

#[test]
fn settings_keeps_other_tables() {
    let (_dir, path) = file();
    let prefix = "# hand written\n[appearance]\ntheme = 'dark' # theme\n\n\n[desktop]\nvim_mode=true\n\n# other listener\n[listener.project2]\nmodel='other'\n\n";
    std::fs::write(&path, prefix).unwrap();
    let settings = ListenerSettings {
        model: "custom-model".into(),
        effort: Some("high".into()),
        per_delegation: UsdCents(345),
        daily: UsdCents(1234),
        max_workers: 2,
        listener_daily: UsdCents(123),
        accept_from: vec!["identity1".into()],
        worker_roles: vec!["reviewer".into()],
        reviews: Reviews::Off,
    };
    save(&path, &project(), &settings).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.split("[listener.project1]").next().unwrap(), prefix);
    assert_eq!(load(&path, &project()).unwrap(), settings);

    let suffix = "\n# after our table\n[unrelated]\nvalue = [ 1, 2 ] # keep\n\n";
    std::fs::write(&path, format!("{text}{suffix}")).unwrap();
    save(&path, &project(), &ListenerSettings::default()).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with(prefix));
    assert!(text.ends_with(suffix));
    assert_eq!(
        load(&path, &project()).unwrap(),
        ListenerSettings::default()
    );
    assert!(!text.contains("effort ="));
}

#[test]
fn missing_file_and_table_give_defaults() {
    let (_dir, path) = file();
    let defaults = ListenerSettings::default();
    assert_eq!(defaults.model, "claude-haiku-5-5");
    assert_eq!(defaults.effort, None);
    assert_eq!(defaults.per_delegation, UsdCents(500));
    assert_eq!(defaults.daily, UsdCents(2000));
    assert_eq!(defaults.max_workers, 3);
    assert_eq!(defaults.listener_daily, UsdCents(200));
    assert!(defaults.accept_from.is_empty());
    assert_eq!(defaults.worker_roles, ["engineer"]);
    assert_eq!(defaults.reviews, Reviews::On);
    assert_eq!(load(&path, &project()).unwrap(), defaults);
    assert!(!path.exists());
    for text in [
        "[appearance]\n",
        "[listener.project2]\n",
        "[listener.project1]\n",
    ] {
        std::fs::write(&path, text).unwrap();
        assert_eq!(load(&path, &project()).unwrap(), defaults);
    }
}

#[test]
fn bad_values_name_the_key() {
    let (_dir, path) = file();
    for value in [
        "-1", "abc", "10000.01", "NaN", "inf", "1.001", "1e2", "", "+1", "1.",
    ] {
        std::fs::write(
            &path,
            format!("[listener.project1]\nper_delegation_usd = '{value}'\n"),
        )
        .unwrap();
        let err = load(&path, &project()).unwrap_err();
        assert!(matches!(err, SettingsError::BadValue { .. }));
        assert!(
            err.to_string().contains("per_delegation_usd"),
            "{value}: {err}"
        );
    }
}

#[test]
fn unparsable_file_is_not_replaced() {
    let (_dir, path) = file();
    for bytes in [b"[broken\n".as_slice(), b"# invalid utf8\n\xff"] {
        std::fs::write(&path, bytes).unwrap();
        assert!(load(&path, &project()).is_err());
        assert!(save(&path, &project(), &ListenerSettings::default()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn coordinator_cannot_be_removed() {
    let (_dir, path) = file();
    let before = ListenerSettings {
        accept_from: vec!["coordid1".into()],
        ..ListenerSettings::default()
    };
    save(&path, &project(), &before).unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut changes = Changes::default();
    let err = changes
        .propose(
            &path,
            &project(),
            "person01",
            &["coordid1"],
            ListenerSettings::default(),
        )
        .unwrap_err();
    assert!(matches!(err, SettingsError::CoordinatorRemoval));
    assert_eq!(err.to_string(), "coordinators always reach your listener");
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(changes.audit(&project()).count(), 0);
}

#[test]
fn narrowing_applies_with_undo() {
    let (_dir, path) = file();
    let mut changes = Changes::default();
    let Applied::Now { undo } = changes
        .propose(&path, &project(), "person01", &[], per_delegation(300))
        .unwrap()
    else {
        panic!("narrowing should apply now");
    };
    assert_eq!(undo.0.len(), 16);
    assert!(
        undo.0
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(load(&path, &project()).unwrap(), per_delegation(300));
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("per_delegation_usd = \"3.00\"")
    );
    changes.undo(&path, &project(), &undo).unwrap();
    assert_eq!(load(&path, &project()).unwrap(), per_delegation(500));
    assert!(changes.undo(&path, &project(), &undo).is_err());
    let rows: Vec<_> = changes.audit(&project()).collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].state, ChangeState::Applied);
    assert_eq!(rows[1].state, ChangeState::Undone);
}

#[test]
fn widening_waits_for_apply() {
    let (_dir, path) = file();
    save(&path, &project(), &ListenerSettings::default()).unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut changes = Changes::default();
    let Applied::Pending { apply, widens } = changes
        .propose(&path, &project(), "person01", &[], per_delegation(800))
        .unwrap()
    else {
        panic!("widening should wait");
    };
    assert_eq!(widens, [Field::PerDelegation]);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    changes.decide(&path, &project(), &apply, true).unwrap();
    assert_eq!(load(&path, &project()).unwrap(), per_delegation(800));
    let rows: Vec<_> = changes.audit(&project()).collect();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.id, apply);
        assert_eq!(row.direction, Direction::Widen(vec![Field::PerDelegation]));
        assert_eq!(row.widens, [Field::PerDelegation]);
    }
    assert_eq!(rows[0].state, ChangeState::Pending);
    assert_eq!(rows[1].state, ChangeState::Applied);
    assert!(changes.decide(&path, &project(), &apply, true).is_err());
    changes.undo(&path, &project(), &apply).unwrap();
    assert_eq!(load(&path, &project()).unwrap(), per_delegation(500));
}

#[test]
fn cancel_leaves_settings_unchanged() {
    let (_dir, path) = file();
    let mut changes = Changes::default();
    let Applied::Pending { apply, .. } = changes
        .propose(&path, &project(), "person01", &[], per_delegation(800))
        .unwrap()
    else {
        panic!("widening should wait");
    };
    assert!(changes.undo(&path, &project(), &apply).is_err());
    changes.decide(&path, &project(), &apply, false).unwrap();
    assert!(!path.exists());
    assert!(changes.decide(&path, &project(), &apply, true).is_err());
    assert!(changes.undo(&path, &project(), &apply).is_err());
    assert_eq!(
        changes.audit(&project()).last().unwrap().state,
        ChangeState::Cancelled
    );
}

#[test]
fn settings_rejects_message_outside_batch() {
    assert_eq!(
        check_request("person01", "person01", false),
        Err(ErrorCode::StaleRequest)
    );
    assert_eq!(
        check_request("coordid1", "person01", false),
        Err(ErrorCode::StaleRequest)
    );
}

#[test]
fn settings_rejects_other_person() {
    assert_eq!(
        check_request("coordid1", "person01", true),
        Err(ErrorCode::Forbidden)
    );
    assert_eq!(check_request("person01", "person01", true), Ok(()));
}

#[test]
fn pending_changes_are_capped() {
    let (_dir, path) = file();
    let mut changes = Changes::default();
    let mut ids = Vec::new();
    for _ in 0..300 {
        let Applied::Pending { apply, .. } = changes
            .propose(&path, &project(), "person01", &[], per_delegation(800))
            .unwrap()
        else {
            panic!("widening should wait");
        };
        ids.push(apply);
    }
    let rows: Vec<_> = changes.audit(&project()).collect();
    assert_eq!(rows.len(), 256);
    assert_eq!(rows.first().unwrap().id, ids[44]);
    assert_eq!(rows.last().unwrap().id, ids[299]);
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(
            changes.decide(&path, &project(), id, false).is_ok(),
            index >= 44
        );
    }
    assert_eq!(changes.audit(&project()).count(), 256);
    assert!(!path.exists());
}

#[test]
fn settings_refuses_oversized_files_and_output() {
    let (_dir, path) = file();
    let text = format!("#{}", " ".repeat(1024 * 1024 - 1));
    std::fs::write(&path, &text).unwrap();
    assert_eq!(
        load(&path, &project()).unwrap(),
        ListenerSettings::default()
    );
    assert!(matches!(
        save(&path, &project(), &ListenerSettings::default()),
        Err(SettingsError::TooLarge { .. })
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    let text = format!("{text} ");
    std::fs::write(&path, &text).unwrap();
    assert!(matches!(
        load(&path, &project()),
        Err(SettingsError::TooLarge { .. })
    ));
    assert!(save(&path, &project(), &ListenerSettings::default()).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
}

#[test]
fn settings_limits_lists_and_validates_typed_values() {
    let (_dir, path) = file();
    for (key, value) in [
        ("model", "1"),
        ("effort", "false"),
        ("max_workers", "-1"),
        ("max_workers", "1.5"),
        ("reviews", "'maybe'"),
        ("daily_usd", "20.0"),
        ("listener_daily_usd", "'10000.01'"),
        ("accept_from", "[1]"),
        ("worker_roles", "'engineer'"),
        ("accept_from", &format!("[{}]", "'id',".repeat(65))),
        ("worker_roles", &format!("[{}]", "'role',".repeat(33))),
    ] {
        std::fs::write(&path, format!("[listener.project1]\n{key} = {value}\n")).unwrap();
        assert!(
            load(&path, &project())
                .unwrap_err()
                .to_string()
                .contains(key),
            "{key}"
        );
    }
    let mut settings = ListenerSettings {
        accept_from: vec!["id".into(); 64],
        worker_roles: vec!["role".into(); 32],
        per_delegation: UsdCents(1_000_000),
        daily: UsdCents(0),
        ..ListenerSettings::default()
    };
    save(&path, &project(), &settings).unwrap();
    assert_eq!(load(&path, &project()).unwrap(), settings);
    let original = std::fs::read(&path).unwrap();
    settings.accept_from.push("extra".into());
    assert!(
        save(&path, &project(), &settings)
            .unwrap_err()
            .to_string()
            .contains("accept_from")
    );
    settings.accept_from.pop();
    settings.worker_roles.push("extra".into());
    assert!(
        save(&path, &project(), &settings)
            .unwrap_err()
            .to_string()
            .contains("worker_roles")
    );
    settings.worker_roles.pop();
    settings.daily = UsdCents(1_000_001);
    assert!(
        save(&path, &project(), &settings)
            .unwrap_err()
            .to_string()
            .contains("daily_usd")
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn settings_rereads_file_and_keeps_unknown_keys() {
    let (_dir, path) = file();
    save(&path, &project(), &per_delegation(500)).unwrap();
    let mut settings = load(&path, &project()).unwrap();
    std::fs::write(&path, "# desktop changed\n[desktop]\nvim_mode = true\n\n[listener.project1]\nfuture = 'keep' # unknown\n").unwrap();
    settings.daily = UsdCents(1000);
    save(&path, &project(), &settings).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# desktop changed\n[desktop]\nvim_mode = true\n\n"));
    assert!(text.contains("future = 'keep' # unknown\n"));
}

#[test]
fn settings_decisions_are_project_scoped_and_retryable_on_io_error() {
    let (_dir, path) = file();
    let mut changes = Changes::default();
    let Applied::Pending { apply, .. } = changes
        .propose(&path, &project(), "person01", &[], per_delegation(800))
        .unwrap()
    else {
        panic!("widening should wait");
    };
    let other = "project2".parse().unwrap();
    assert!(changes.decide(&path, &other, &apply, true).is_err());
    assert_eq!(changes.audit(&other).count(), 0);
    std::fs::write(&path, "[broken").unwrap();
    assert!(changes.decide(&path, &project(), &apply, true).is_err());
    assert_eq!(changes.audit(&project()).count(), 1);
    std::fs::remove_file(&path).unwrap();
    changes.decide(&path, &project(), &apply, true).unwrap();
    assert_eq!(load(&path, &project()).unwrap(), per_delegation(800));
}

#[test]
fn settings_preserves_crlf_and_no_final_newline_in_other_tables() {
    let (_dir, path) = file();
    for text in [
        "# windows\r\n[appearance]\r\ntheme = 'dark'\r\n\r\n",
        "[desktop]\nvim_mode = true",
    ] {
        std::fs::write(&path, text).unwrap();
        save(&path, &project(), &ListenerSettings::default()).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().starts_with(text));
    }
    let suffix = "\r\n# last section\r\n[desktop]\r\nvim_mode = true";
    std::fs::write(
        &path,
        format!("[listener.project1]\r\nper_delegation_usd='5.00'\r\n{suffix}"),
    )
    .unwrap();
    save(&path, &project(), &per_delegation(300)).unwrap();
    assert!(std::fs::read_to_string(&path).unwrap().ends_with(suffix));
}

#[test]
fn settings_classifies_all_listener_policy_fields() {
    let (_dir, path) = file();
    let mut changes = Changes::default();
    let after = ListenerSettings {
        per_delegation: UsdCents(300),
        daily: UsdCents(3000),
        max_workers: 4,
        listener_daily: UsdCents(300),
        accept_from: vec!["otherid1".into()],
        worker_roles: vec!["engineer".into(), "reviewer".into()],
        ..ListenerSettings::default()
    };
    let Applied::Pending { widens, .. } = changes
        .propose(&path, &project(), "person01", &[], after)
        .unwrap()
    else {
        panic!("any widening must wait");
    };
    assert_eq!(
        widens,
        [
            Field::Daily,
            Field::MaxWorkers,
            Field::AcceptFrom,
            Field::WorkerRoles,
            Field::ListenerDaily
        ]
    );
    let after = ListenerSettings {
        accept_from: vec!["person01".into(), "coordid1".into()],
        ..ListenerSettings::default()
    };
    assert!(matches!(
        changes
            .propose(&path, &project(), "person01", &["coordid1"], after)
            .unwrap(),
        Applied::Now { .. }
    ));
}

#[test]
fn settings_io_errors_keep_the_path_and_leave_no_temp_file() {
    let (dir, path) = file();
    std::fs::create_dir(&path).unwrap();
    for err in [
        load(&path, &project()).unwrap_err(),
        save(&path, &project(), &ListenerSettings::default()).unwrap_err(),
    ] {
        assert!(matches!(&err, SettingsError::Io { path: error_path, .. } if error_path == &path));
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    std::fs::remove_dir(&path).unwrap();
    let nested = dir.path().join("nested/settings.toml");
    save(&nested, &project(), &ListenerSettings::default()).unwrap();
    assert_eq!(
        std::fs::read_dir(nested.parent().unwrap()).unwrap().count(),
        1
    );
}

#[test]
fn settings_edits_inline_and_dotted_tables() {
    let (_dir, path) = file();
    for text in [
        "listener.project1.per_delegation_usd = '5.00'\n# root\nother = true",
        "[listener]\nproject1.per_delegation_usd = '5.00'\nproject2 = {model='keep'}\n",
        "[listener]\nproject1 = { per_delegation_usd = '5.00', effort = 'high' } # keep\nproject2 = {model='keep'}\n",
        "listener = { project1 = { per_delegation_usd = '5.00' }, project2 = {model='keep'} }\n# end",
        "listener = { project2 = {model='keep'} }\n# end",
        "[listener.project1.future]\nvalue = true\n",
        "listener.project1.future.value = [\n 'keep'\n]\n[desktop]\nvim_mode = true",
        "[listener]\nproject1.future.value = true\n",
        "listener = { project1.model = 'old', project1.effort = 'high', project2 = {model='keep'} }\n# end",
        "listener = { project2 = {model='keep'}, project1.model = 'old', project1.effort = 'high' }\n# end",
    ] {
        std::fs::write(&path, text).unwrap();
        save(&path, &project(), &per_delegation(300)).unwrap();
        assert_eq!(
            load(&path, &project()).unwrap(),
            per_delegation(300),
            "{text}"
        );
        let saved = std::fs::read_to_string(&path).unwrap();
        if text.contains("project2") {
            assert!(saved.contains("model='keep'"), "{saved}");
        }
        if text.ends_with("# end") {
            assert!(saved.ends_with("# end"));
        }
        if text.ends_with("other = true") {
            assert!(saved.contains("# root\nother = true"));
        }
    }
}

#[test]
fn settings_refuses_tables_in_place_of_owned_scalar_fields() {
    let (_dir, path) = file();
    for key in ["model", "effort", "accept_from"] {
        for text in [
            format!("[listener.project1.{key}]\nx = 'bad shape'\n"),
            format!("listener = {{ project1.{key}.x = 'bad shape' }}\n"),
        ] {
            std::fs::write(&path, &text).unwrap();
            assert!(
                save(&path, &project(), &ListenerSettings::default())
                    .unwrap_err()
                    .to_string()
                    .contains(key)
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
    }
}

#[test]
fn settings_undo_entries_and_audit_are_capped_per_project() {
    let (_dir, path) = file();
    let other = "project2".parse().unwrap();
    let mut changes = Changes::default();
    let mut ids = Vec::new();
    let Applied::Now { undo: other_id } = changes
        .propose(&path, &other, "person01", &[], per_delegation(0))
        .unwrap()
    else {
        panic!("narrowing should apply now");
    };
    for _ in 0..300 {
        let Applied::Now { undo } = changes
            .propose(&path, &project(), "person01", &[], per_delegation(0))
            .unwrap()
        else {
            panic!("equal or narrowing should apply now");
        };
        ids.push(undo);
    }
    assert_eq!(changes.audit(&project()).count(), 256);
    assert_eq!(changes.audit(&other).count(), 1);
    assert!(changes.undo(&path, &project(), &ids[43]).is_err());
    changes.undo(&path, &project(), &ids[44]).unwrap();
    changes.undo(&path, &other, &other_id).unwrap();
    assert_eq!(load(&path, &other).unwrap(), ListenerSettings::default());
    assert_eq!(load(&path, &project()).unwrap(), per_delegation(0));
}

#[test]
fn settings_inline_parent_keeps_other_projects_byte_for_byte() {
    let (_dir, path) = file();
    let other = "\r\n# another project\r\nproject2 = {model='keep'},\r\n";
    for target in ["", "project1 = {per_delegation_usd='5.00'},"] {
        let text = format!("listener = {{{target}{other}}}\r\n# end");
        std::fs::write(&path, text).unwrap();
        save(&path, &project(), &per_delegation(300)).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains(other), "{saved:?}");
        assert_eq!(load(&path, &project()).unwrap(), per_delegation(300));
    }
}

#[test]
fn settings_rejects_oversized_pending_payloads() {
    let (_dir, path) = file();
    let mut changes = Changes::default();
    for model in ["x".repeat(2 * 1024 * 1024), "\u{1}".repeat(200_000)] {
        let after = ListenerSettings {
            model,
            ..per_delegation(800)
        };
        assert!(matches!(
            changes.propose(&path, &project(), "person01", &[], after),
            Err(SettingsError::TooLarge { .. })
        ));
        assert_eq!(changes.audit(&project()).count(), 0);
        assert!(!path.exists());
    }
}

fn narrow_daily(cents: u64) -> ListenerSettings {
    ListenerSettings {
        daily: UsdCents(cents),
        ..ListenerSettings::default()
    }
}

#[test]
fn apply_keeps_a_narrowing_made_after_the_proposal() {
    let (_dir, path) = file();
    let mut changes = Changes::default();
    let Applied::Pending { apply, .. } = changes
        .propose(&path, &project(), "person01", &[], per_delegation(800))
        .unwrap()
    else {
        panic!("widening should wait");
    };
    assert!(matches!(
        changes
            .propose(&path, &project(), "person01", &[], narrow_daily(1000))
            .unwrap(),
        Applied::Now { .. }
    ));
    changes.decide(&path, &project(), &apply, true).unwrap();
    let saved = load(&path, &project()).unwrap();
    assert_eq!(saved.per_delegation, UsdCents(800));
    assert_eq!(saved.daily, UsdCents(1000), "daily widened back unasked");
}

#[test]
fn apply_refuses_a_field_that_changed_since_the_proposal() {
    let (_dir, path) = file();
    let mut changes = Changes::default();
    let Applied::Pending { apply, .. } = changes
        .propose(&path, &project(), "person01", &[], per_delegation(800))
        .unwrap()
    else {
        panic!("widening should wait");
    };
    changes
        .propose(&path, &project(), "person01", &[], per_delegation(300))
        .unwrap();
    assert!(matches!(
        changes.decide(&path, &project(), &apply, true),
        Err(SettingsError::StaleChange(_))
    ));
    assert_eq!(load(&path, &project()).unwrap(), per_delegation(300));
    changes.decide(&path, &project(), &apply, false).unwrap();
}

#[test]
fn undo_keeps_a_change_made_after_the_undone_one() {
    let (_dir, path) = file();
    let mut changes = Changes::default();
    let Applied::Now { undo } = changes
        .propose(&path, &project(), "person01", &[], per_delegation(300))
        .unwrap()
    else {
        panic!("narrowing should apply now");
    };
    let later = ListenerSettings {
        per_delegation: UsdCents(300),
        daily: UsdCents(1000),
        ..ListenerSettings::default()
    };
    changes
        .propose(&path, &project(), "person01", &[], later)
        .unwrap();
    changes.undo(&path, &project(), &undo).unwrap();
    let saved = load(&path, &project()).unwrap();
    assert_eq!(saved.per_delegation, UsdCents(500));
    assert_eq!(saved.daily, UsdCents(1000), "daily widened back unasked");
}

#[test]
fn undo_refuses_a_field_that_changed_since() {
    let (_dir, path) = file();
    let mut changes = Changes::default();
    let Applied::Now { undo } = changes
        .propose(&path, &project(), "person01", &[], per_delegation(300))
        .unwrap()
    else {
        panic!("narrowing should apply now");
    };
    changes
        .propose(&path, &project(), "person01", &[], per_delegation(100))
        .unwrap();
    assert!(matches!(
        changes.undo(&path, &project(), &undo),
        Err(SettingsError::StaleChange(_))
    ));
    assert_eq!(load(&path, &project()).unwrap(), per_delegation(100));
}

#[test]
fn undo_cannot_remove_a_coordinator() {
    let (_dir, path) = file();
    let mut changes = Changes::default();
    let with_coordinator = ListenerSettings {
        accept_from: vec!["coordid1".into()],
        ..ListenerSettings::default()
    };
    let Applied::Now { undo } = changes
        .propose(
            &path,
            &project(),
            "person01",
            &["coordid1"],
            with_coordinator.clone(),
        )
        .unwrap()
    else {
        panic!("adding a coordinator is not a widening");
    };
    assert!(matches!(
        changes.undo(&path, &project(), &undo),
        Err(SettingsError::CoordinatorRemoval)
    ));
    assert_eq!(load(&path, &project()).unwrap(), with_coordinator);
}

fn saved(settings: &ListenerSettings) -> (tempfile::TempDir, std::path::PathBuf) {
    let (dir, path) = file();
    save(&path, &project(), settings).unwrap();
    (dir, path)
}

fn effort(level: Option<&str>) -> ListenerSettings {
    ListenerSettings {
        effort: level.map(str::to_owned),
        ..ListenerSettings::default()
    }
}

fn waits_for(from: &ListenerSettings, to: ListenerSettings, field: Field) {
    let (_dir, path) = saved(from);
    let original = std::fs::read(&path).unwrap();
    let mut changes = Changes::default();
    let Applied::Pending { widens, .. } = changes
        .propose(&path, &project(), "person01", &[], to)
        .unwrap()
    else {
        panic!("{field:?} change should wait for the owner");
    };
    assert_eq!(widens, [field]);
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

fn applies_now(from: &ListenerSettings, to: ListenerSettings) {
    let (_dir, path) = saved(from);
    let mut changes = Changes::default();
    let Applied::Now { undo } = changes
        .propose(&path, &project(), "person01", &[], to.clone())
        .unwrap()
    else {
        panic!("narrowing should apply now");
    };
    assert_eq!(load(&path, &project()).unwrap(), to);
    changes.undo(&path, &project(), &undo).unwrap();
    assert_eq!(load(&path, &project()).unwrap(), *from);
}

#[test]
fn reviews_off_waits_for_apply() {
    let off = ListenerSettings {
        reviews: Reviews::Off,
        ..ListenerSettings::default()
    };
    waits_for(&ListenerSettings::default(), off, Field::Reviews);
}

#[test]
fn reviews_on_applies_now() {
    let off = ListenerSettings {
        reviews: Reviews::Off,
        ..ListenerSettings::default()
    };
    applies_now(&off, ListenerSettings::default());
}

#[test]
fn model_change_waits_for_apply_in_both_directions() {
    // No model-to-tier order exists in agmen-roles, so no change is provably lower.
    let named = |model: &str| ListenerSettings {
        model: model.into(),
        ..ListenerSettings::default()
    };
    waits_for(
        &named("claude-haiku-5-5"),
        named("claude-opus-5-5"),
        Field::Model,
    );
    waits_for(
        &named("claude-opus-5-5"),
        named("claude-haiku-5-5"),
        Field::Model,
    );
}

#[test]
fn effort_raised_waits_for_apply() {
    waits_for(&effort(Some("low")), effort(Some("high")), Field::Effort);
    waits_for(&effort(Some("xhigh")), effort(Some("max")), Field::Effort);
}

#[test]
fn effort_lowered_applies_now() {
    applies_now(&effort(Some("high")), effort(Some("low")));
    applies_now(&effort(Some("high")), effort(Some("high")));
    applies_now(&effort(None), effort(None));
}

#[test]
fn effort_without_a_provable_order_waits_for_apply() {
    waits_for(&effort(None), effort(Some("low")), Field::Effort);
    waits_for(&effort(Some("low")), effort(None), Field::Effort);
    waits_for(&effort(Some("low")), effort(Some("turbo")), Field::Effort);
    waits_for(&effort(Some("turbo")), effort(Some("low")), Field::Effort);
}

#[test]
fn a_field_classify_does_not_know_waits_for_apply() {
    // Simulates a field added to `ListenerSettings` without a classification:
    // the carry step forgets max_workers, so the probe still differs from `after`.
    fn forgets_max_workers(to: &mut ListenerSettings, from: &ListenerSettings) {
        let kept = to.max_workers;
        carry_classified(to, from);
        to.max_workers = kept;
    }
    let (_dir, path) = saved(&ListenerSettings::default());
    let original = std::fs::read(&path).unwrap();
    let narrower = ListenerSettings {
        max_workers: 2,
        ..ListenerSettings::default()
    };
    let mut changes = Changes::default();
    let Applied::Pending { widens, .. } = changes
        .propose_with(
            forgets_max_workers,
            &path,
            &project(),
            "person01",
            &[],
            narrower.clone(),
        )
        .unwrap()
    else {
        panic!("an unclassified difference must wait for the owner");
    };
    assert_eq!(widens, [Field::Unclassified]);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    // The real carry step knows every field, so the same change narrows.
    assert!(matches!(
        changes
            .propose(&path, &project(), "person01", &[], narrower)
            .unwrap(),
        Applied::Now { .. }
    ));
}

#[test]
fn listener_caps_follow_the_project_table() {
    let (_dir, path) = file();
    // No file, or only another project's table: the defaults `load` gives a
    // running listener, marked as not set by the person.
    let defaults = listener_caps(&path, &project()).unwrap();
    assert!(!defaults.configured);
    assert_eq!(
        (
            defaults.model.as_str(),
            defaults.per_delegation_usd.as_str(),
            defaults.workers_daily_usd.as_str(),
            defaults.max_workers,
            defaults.listener_daily_usd.as_str(),
        ),
        ("claude-haiku-5-5", "5.00", "20.00", 3, "2.00")
    );
    assert!(defaults.accept_from.is_empty());
    std::fs::write(&path, "[listener.project2]\nmodel = 'other'\n").unwrap();
    assert!(!listener_caps(&path, &project()).unwrap().configured);
    // Each key lands in its own field: every value differs from the others
    // and from the defaults, so a swapped pair fails here.
    let settings = ListenerSettings {
        model: "custom-model".into(),
        effort: Some("high".into()),
        per_delegation: UsdCents(345),
        daily: UsdCents(1234),
        max_workers: 2,
        listener_daily: UsdCents(123),
        accept_from: vec!["sean".into(), "adam/coord".into()],
        worker_roles: vec!["reviewer".into()],
        reviews: Reviews::Off,
    };
    save(&path, &project(), &settings).unwrap();
    assert_eq!(
        listener_caps(&path, &project()).unwrap(),
        ListenerCaps {
            configured: true,
            model: "custom-model".into(),
            per_delegation_usd: "3.45".into(),
            workers_daily_usd: "12.34".into(),
            max_workers: 2,
            listener_daily_usd: "1.23".into(),
            accept_from: vec!["sean".into(), "adam/coord".into()],
        }
    );
    // A file that does not parse is none: the snapshot keeps the chat.
    std::fs::write(&path, "[listener\n").unwrap();
    assert!(listener_caps(&path, &project()).is_none());
}

#[test]
fn published_changes_follow_propose_decide_and_undo() {
    let (_dir, path) = file();
    let mut changes = Changes::default();
    let Applied::Pending { apply, .. } = changes
        .propose(&path, &project(), "person01", &[], per_delegation(800))
        .unwrap()
    else {
        panic!("widening should wait");
    };
    let cards = published_changes(&path, &project());
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].id, apply.0);
    assert_eq!(cards[0].requested_by, "person01");
    assert!(cards[0].widens);
    assert!(cards[0].message_id.is_none());
    assert_eq!(cards[0].state, SettingsChangeState::Pending);
    assert_eq!(
        cards[0].rows,
        vec![SettingsChangeRow {
            target: "listener".into(),
            setting: "per_delegation_usd".into(),
            before: "5.00".into(),
            after: "8.00".into(),
        }]
    );
    changes.decide(&path, &project(), &apply, true).unwrap();
    assert_eq!(
        published_changes(&path, &project())[0].state,
        SettingsChangeState::Applied
    );
    changes.undo(&path, &project(), &apply).unwrap();
    assert_eq!(
        published_changes(&path, &project())[0].state,
        SettingsChangeState::Undone
    );
    assert_eq!(
        load(&path, &project()).unwrap().per_delegation,
        UsdCents(500)
    );
}
