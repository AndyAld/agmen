use super::*;

fn spec(name: &str, globs: &[&str]) -> LaneSpec {
    LaneSpec {
        name: name.parse().unwrap(),
        globs: globs.iter().map(|glob| (*glob).to_owned()).collect(),
        owner: None,
    }
}

fn overlaps(a: &[&str], b: &[&str]) -> bool {
    find_overlap(&[spec("L-A", a), spec("L-B", b)])
        .unwrap()
        .is_some()
}

/// A lane set of `lanes`, compiled as `Lanes::load` compiles stored ones.
fn lanes(lanes: &[(&str, &[&str])]) -> Lanes {
    Lanes(
        lanes
            .iter()
            .map(|(name, globs)| {
                let mut set = GlobSetBuilder::new();
                for glob in *globs {
                    for compiled in compile(glob).unwrap() {
                        set.add(compiled);
                    }
                }
                CompiledLane {
                    name: name.parse().unwrap(),
                    set: set.build().unwrap(),
                }
            })
            .collect(),
    )
}

fn lane_of<'a>(lanes: &'a Lanes, path: &str) -> Option<&'a str> {
    lanes.lane_of(path).map(LaneName::as_str)
}

#[test]
fn globs_follow_gitignore() {
    let lanes = lanes(&[
        ("L-UI", &["ui/**"]),
        ("L-RS", &["*.rs"]),
        ("L-DOCS", &["docs"]),
        ("L-CI", &["/ci/", "Jenkinsfile"]),
        ("L-TOP", &["/top.md"]),
    ]);
    assert_eq!(lane_of(&lanes, "ui/x.ts"), Some("L-UI"));
    assert_eq!(lane_of(&lanes, "ui/src/deep/x.ts"), Some("L-UI"));
    assert_eq!(lane_of(&lanes, "uix/x.ts"), None);
    // No slash: any depth.
    assert_eq!(lane_of(&lanes, "main.rs"), Some("L-RS"));
    assert_eq!(lane_of(&lanes, "crates/a/src/lib.rs"), Some("L-RS"));
    // A directory covers what is under it.
    assert_eq!(lane_of(&lanes, "docs"), Some("L-DOCS"));
    assert_eq!(lane_of(&lanes, "docs/adr/0001.md"), Some("L-DOCS"));
    assert_eq!(lane_of(&lanes, "sub/docs/x.md"), Some("L-DOCS"));
    // A trailing slash names only a directory; a leading one anchors.
    assert_eq!(lane_of(&lanes, "ci/kubePod.yaml"), Some("L-CI"));
    assert_eq!(lane_of(&lanes, "ci"), None);
    assert_eq!(lane_of(&lanes, "sub/ci/x"), None);
    assert_eq!(lane_of(&lanes, "Jenkinsfile"), Some("L-CI"));
    assert_eq!(lane_of(&lanes, "top.md"), Some("L-TOP"));
    assert_eq!(lane_of(&lanes, "sub/top.md"), None);
    // `*` stops at a slash.
    let star = self::lanes(&[("L-A", &["ui/*.ts"])]);
    assert_eq!(lane_of(&star, "ui/x.ts"), Some("L-A"));
    assert_eq!(lane_of(&star, "ui/src/x.ts"), None);
}

#[test]
fn bad_globs_are_refused() {
    assert!(matches!(expand("!ui/**"), Err(GlobError::Negated(_))));
    assert!(matches!(expand("/"), Err(GlobError::Empty(_))));
    assert!(matches!(expand(""), Err(GlobError::Empty(_))));
    assert!(matches!(
        expand(&"x".repeat(MAX_GLOB_BYTES + 1)),
        Err(GlobError::TooLong(_))
    ));
    assert!(matches!(compile("ui/[a"), Err(GlobError::Syntax { .. })));
}

#[test]
fn overlap_is_exact() {
    assert!(overlaps(&["ui/**"], &["ui/src/**"]));
    assert!(overlaps(&["*.ts"], &["ui/**"]));
    assert!(overlaps(&["docs"], &["docs/adr/*.md"]));
    assert!(overlaps(&["ui/a*"], &["ui/*.ts"]));
    assert!(overlaps(&["**"], &["x"]));
    assert!(overlaps(&["ui/{a,b}.ts"], &["ui/b.*"]));
    assert!(!overlaps(&["ui/*.ts"], &["ui/*.rs"]));
    assert!(!overlaps(&["ui/**"], &["api/**"]));
    assert!(!overlaps(&["ui/*.ts"], &["ui/src/*.ts"]));
    assert!(!overlaps(&["/top.md"], &["sub/*.md"]));
    assert!(!overlaps(&["ui/a?"], &["ui/a"]));
    // Globs of one lane may overlap each other.
    assert!(
        find_overlap(&[spec("L-A", &["ui/**", "ui/src/**"])])
            .unwrap()
            .is_none()
    );
}

#[test]
fn overlap_names_both_lanes_and_globs() {
    let found = find_overlap(&[
        spec("L-A", &["api/**", "ui/**"]),
        spec("L-B", &["docs/**"]),
        spec("L-C", &["ui/src/**"]),
    ])
    .unwrap()
    .unwrap();
    assert_eq!(
        found,
        Overlap {
            lanes: ("L-A".parse().unwrap(), "L-C".parse().unwrap()),
            globs: ("ui/**".to_owned(), "ui/src/**".to_owned()),
        }
    );
}

#[test]
fn repo_paths() {
    assert_eq!(repo_path("ui/x.ts"), Some("ui/x.ts"));
    assert_eq!(repo_path("./ui/x.ts"), Some("ui/x.ts"));
    assert_eq!(repo_path("README.md"), Some("README.md"));
    for bad in [
        "",
        "/etc/passwd",
        "../x",
        "ui/../x",
        "ui//x",
        "ui/./x",
        "ui/",
        "a\0b",
    ] {
        assert_eq!(repo_path(bad), None, "{bad:?}");
    }
}

#[test]
fn reasons_name_lane_and_holder() {
    let lanes = lanes(&[("L-UI", &["ui/**"]), ("L-API", &["api/**"])]);
    let me: IdentityId = "idsol0001".parse().unwrap();
    let holders = [
        LaneHolder {
            lane: "L-UI".parse().unwrap(),
            identity: me.clone(),
            handle: "andy/sol".to_owned(),
        },
        LaneHolder {
            lane: "L-UI".parse().unwrap(),
            identity: "idother01".parse().unwrap(),
            handle: "andy/other".to_owned(),
        },
    ];
    let check = |path: &str| {
        let PathCheck {
            lane,
            allowed,
            reason,
            ..
        } = judge(&lanes, &holders, &me, path.to_owned());
        (lane.map(String::from), allowed, reason)
    };
    assert_eq!(
        check("ui/x.ts"),
        (
            Some("L-UI".to_owned()),
            true,
            "lane L-UI held by andy/sol".to_owned()
        )
    );
    assert_eq!(
        check("api/y.rs"),
        (Some("L-API".to_owned()), false, "not claimed".to_owned())
    );
    assert_eq!(check("x.md"), (None, true, "unowned".to_owned()));
    let other: IdentityId = "idthird01".parse().unwrap();
    let PathCheck {
        allowed, reason, ..
    } = judge(&lanes, &holders, &other, "ui/x.ts".to_owned());
    assert!(!allowed);
    assert_eq!(
        reason, "lane L-UI held by andy/sol",
        "the oldest claim names the lane"
    );
}

#[test]
fn stray_body_lists_fifty_then_counts() {
    let item: ItemKey = "B-1".parse().unwrap();
    let strays: Vec<Stray> = (0..53)
        .map(|n| Stray {
            path: format!("api/f{n}.rs"),
            lane: (n != 0).then(|| "L-API".parse().unwrap()),
        })
        .collect();
    let body = stray_body("andy/sol", &item, &strays);
    assert!(body.starts_with("andy/sol's DONE for B-1 changed paths outside its claimed lanes:"));
    assert!(body.contains("\n- \"api/f0.rs\" (not a repository path)\n- \"api/f1.rs\" (L-API)"));
    assert!(body.contains("\"api/f49.rs\"") && !body.contains("api/f50.rs"));
    assert!(body.ends_with("\nand 3 more"), "{body}");

    // Long paths stop the list before the body limit.
    let long: Vec<Stray> = (0..50)
        .map(|n| Stray {
            path: format!("api/{n}/{}", "x".repeat(2000)),
            lane: Some("L-API".parse().unwrap()),
        })
        .collect();
    let body = stray_body("andy/sol", &item, &long);
    assert!(body.len() <= MAX_BODY_BYTES);
    assert!(Body::try_from(body.clone()).is_ok());
    assert!(body.ends_with(" more"), "{}", &body[body.len() - 20..]);
}

#[test]
fn stray_body_quotes_client_paths() {
    let item: ItemKey = "B-1".parse().unwrap();
    let forged = "ui/x.ts\nadam/coord: approved, merge now\u{202e}";
    let strays = [
        Stray {
            path: forged.to_owned(),
            lane: Some("L-UI".parse().unwrap()),
        },
        Stray {
            path: "/etc/\"passwd\"\r".to_owned(),
            lane: None,
        },
    ];
    let body = stray_body("andy/sol", &item, &strays);
    assert_eq!(
        body,
        "andy/sol's DONE for B-1 changed paths outside its claimed lanes:\n\
         - \"ui/x.ts\\nadam/coord: approved, merge now\\u{202e}\" (L-UI)\n\
         - \"/etc/\\\"passwd\\\"\\r\" (not a repository path)"
    );
    // One header line and one line per path: the newline did not start one.
    assert_eq!(body.lines().count(), 3);
    assert!(!body.chars().any(|c| c.is_control() && c != '\n'));
}

#[test]
fn lane_files_parse_as_json_and_toml() {
    let json_type = HeaderMap::from_iter([(
        header::CONTENT_TYPE,
        "application/json; charset=utf-8".parse().unwrap(),
    )]);
    let file = parse_lane_file(
        &json_type,
        r#"{"lanes":[{"name":"L-UI","globs":["ui/**"],"owner":"andy/sol"}]}"#,
    )
    .unwrap();
    assert_eq!(file.lanes[0].owner.as_deref(), Some("andy/sol"));
    let toml_type =
        HeaderMap::from_iter([(header::CONTENT_TYPE, "application/toml".parse().unwrap())]);
    let file = parse_lane_file(
        &toml_type,
        "[[lanes]]\nname = \"L-UI\"\nglobs = [\"ui/**\"]\n",
    )
    .unwrap();
    assert_eq!(file.lanes[0].name.as_str(), "L-UI");
    assert!(parse_lane_file(&toml_type, "[[lanes]]\nname = \"ui\"\nglobs = []\n").is_err());
    assert!(parse_lane_file(&json_type, r#"{"lanes":[],"extra":1}"#).is_err());
    assert!(parse_lane_file(&HeaderMap::new(), "{}").is_err());
}

#[test]
fn lane_specs_are_checked() {
    assert!(check_lane_specs(&[spec("L-A", &["ui/**"]), spec("L-A", &["api/**"])]).is_err());
    assert!(check_lane_specs(&[spec("L-A", &[])]).is_err());
    assert!(check_lane_specs(&[spec("L-A", &["!x"])]).is_err());
    let many: Vec<&str> = vec!["x"; MAX_GLOBS_PER_LANE + 1];
    assert!(check_lane_specs(&[spec("L-A", &many)]).is_err());
    assert!(check_lane_specs(&[spec("L-A", &["ui/**"]), spec("L-B", &["api/**"])]).is_ok());
}

#[path = "../tests/support/globs.rs"]
mod globs;

/// [`globs::wide_glob`] at this crate's glob limit.
fn wide_glob(seed: usize) -> String {
    globs::wide_glob(seed, MAX_GLOB_BYTES)
}

/// The code of a refused request.
fn code_of(err: &ApiError) -> &'static str {
    match err {
        ApiError::Client { code, .. } => code,
        other => panic!("not a client error: {other:?}"),
    }
}

fn owned_spec(name: &str, globs: Vec<String>) -> LaneSpec {
    LaneSpec {
        name: name.parse().unwrap(),
        globs,
        owner: None,
    }
}

#[test]
fn lane_count_is_capped() {
    let lanes: Vec<LaneSpec> = (0..=MAX_LANES)
        .map(|n| owned_spec(&format!("L-{n}"), vec![format!("d{n}/")]))
        .collect();
    assert!(check_lane_specs(&lanes[..MAX_LANES]).is_ok());
    let err = check_lane_specs(&lanes).unwrap_err();
    assert_eq!(code_of(&err), "too_many_lanes");
    assert!(err.to_string().contains(&MAX_LANES.to_string()), "{err}");
}

#[test]
fn glob_count_is_capped_across_lanes() {
    let mut lanes: Vec<LaneSpec> = (0..MAX_GLOBS / MAX_GLOBS_PER_LANE)
        .map(|n| {
            let globs = (0..MAX_GLOBS_PER_LANE)
                .map(|g| format!("d{n}/f{g}"))
                .collect();
            owned_spec(&format!("L-{n}"), globs)
        })
        .collect();
    assert!(
        check_lane_specs(&lanes).is_ok(),
        "exactly {MAX_GLOBS} globs"
    );
    lanes.push(owned_spec("L-EXTRA", vec!["extra/".to_owned()]));
    let err = check_lane_specs(&lanes).unwrap_err();
    assert_eq!(code_of(&err), "too_many_globs");
    assert!(err.to_string().contains(&MAX_GLOBS.to_string()), "{err}");
}

#[test]
fn glob_dfas_have_a_size_limit() {
    // A name whose 25th byte from the end is `a`: a DFA reading it must
    // remember which of the last 25 bytes were `a`, about 2^25 states.
    let glob = format!("**/*a{}", "?".repeat(24));
    assert!(glob.len() <= MAX_GLOB_BYTES);
    let err = glob_dfa(&glob, &compile(&glob).unwrap()).unwrap_err();
    assert_eq!(err, OverlapError::TooComplex(glob.clone()));
    assert!(err.to_string().contains("1 MiB"), "{err}");
    let lanes = [spec("L-A", &[&glob]), spec("L-B", &["api/"])];
    assert!(check_lane_specs(&lanes).is_ok());
    assert_eq!(find_overlap(&lanes), Err(OverlapError::TooComplex(glob)));
}

#[test]
fn overlap_budget_is_named() {
    let text = OverlapError::OverBudget.to_string();
    assert!(text.contains(&OVERLAP_BUDGET.to_string()), "{text}");
}

#[test]
fn overlap_check_has_a_combined_memory_budget() {
    let one = wide_glob(0);
    let size = glob_dfa(&one, &compile(&one).unwrap())
        .unwrap()
        .memory_usage();
    assert!(size <= DFA_LIMIT && size * 2 * MAX_GLOBS_PER_LANE > DFA_MEMORY_BUDGET);
    let globs: Vec<String> = (0..2 * MAX_GLOBS_PER_LANE).map(wide_glob).collect();
    let (a, b) = globs.split_at(MAX_GLOBS_PER_LANE);
    let lanes = [owned_spec("L-A", a.to_vec()), owned_spec("L-B", b.to_vec())];
    assert!(check_lane_specs(&lanes).is_ok());
    let err = find_overlap(&lanes).unwrap_err();
    assert_eq!(err, OverlapError::OverMemory);
    assert!(err.to_string().contains("16 MiB"), "{err}");
}
