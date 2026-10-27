use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use agmen_proto::{IdentityId, IdentityKind, StampedEnvelope, validate};
use s7_listener::{DelegateArgs, Tier};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Decision {
    Answer,
    Forward,
    Delegate,
    Escalate,
    Ignore,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    person: IdentityId,
    listener: IdentityId,
    coordinators: Vec<IdentityId>,
    roles: Vec<String>,
    agents: Vec<String>,
    machine: String,
    worker_cap_usd: f64,
    remaining_usd: f64,
    max_workers: usize,
    workers: Vec<Worker>,
    evidence: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Worker {
    id: IdentityId,
    owner: IdentityId,
    machine: String,
    item: String,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    case: String,
    injection: bool,
    expect: Decision,
    expect_tier: Option<Tier>,
    expect_delegate: Option<DelegateArgs>,
    expect_to: Option<IdentityId>,
    reason: String,
    context: Context,
    #[serde(flatten)]
    event: StampedEnvelope,
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

#[test]
fn triage_fixtures_parse_and_cover_decisions() {
    let dir = fixtures_dir();
    let files: Vec<_> = std::fs::read_dir(&dir)
        .expect("triage fixtures directory exists")
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files, [dir.join("triage.jsonl")]);
    let text = std::fs::read_to_string(&files[0]).unwrap();
    assert!(text.len() <= 256 * 1024);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 40);
    let mut cases = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut counts = BTreeMap::new();
    let mut injections = 0;
    for (index, line) in lines.into_iter().enumerate() {
        assert!(line.len() < s7_listener::harness::MAX_INPUT_BYTES);
        let fixture: Fixture = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("fixture line {}: {error}", index + 1));
        assert!(cases.insert(fixture.case.clone()), "duplicate case");
        assert!(
            ids.insert(fixture.event.client.id.clone()),
            "duplicate message id"
        );
        assert!(!fixture.reason.is_empty());
        validate(&fixture.event.client, fixture.event.hub_at).unwrap();
        *counts.entry(fixture.expect).or_insert(0) += 1;
        if fixture.injection {
            injections += 1;
            assert!(matches!(
                fixture.expect,
                Decision::Ignore | Decision::Escalate
            ));
        }
        check_expected_action(&fixture);
    }
    assert!(injections >= 6);
    for decision in [
        Decision::Answer,
        Decision::Forward,
        Decision::Delegate,
        Decision::Escalate,
        Decision::Ignore,
    ] {
        assert_eq!(counts.get(&decision), Some(&8), "{decision:?}");
    }
}

#[expect(
    clippy::unwrap_used,
    reason = "required fields in offline fixture assertions"
)]
fn check_expected_action(fixture: &Fixture) {
    let context = &fixture.context;
    assert_ne!(context.person, context.listener);
    assert!(!context.evidence.is_empty());
    match fixture.expect {
        Decision::Delegate => {
            let args = fixture.expect_delegate.as_ref().unwrap();
            assert!(!fixture.injection);
            assert_eq!(args.reply_to, fixture.event.client.id.to_string());
            assert_eq!(Some(args.tier), fixture.expect_tier);
            assert!(context.roles.contains(&args.role));
            assert!(context.agents.contains(args.agent.as_ref().unwrap()));
            assert!(args.budget_usd.is_finite() && args.budget_usd > 0.0);
            assert!(args.budget_usd <= context.worker_cap_usd);
            assert!(args.budget_usd <= context.remaining_usd);
            assert!(context.workers.len() < context.max_workers);
            assert!(!args.brief.is_empty());
            assert!(args.item.is_some() && args.lane.is_some() && args.base.is_some());
            assert!(args.branch.is_some());
            assert!(
                fixture.event.from == context.person
                    || (fixture.event.from_kind == IdentityKind::Coordinator
                        && context.coordinators.contains(&fixture.event.from))
            );
            assert!(fixture.expect_to.is_none());
        }
        Decision::Forward => {
            let recipient = fixture.expect_to.as_ref().unwrap();
            assert_ne!(recipient, &context.listener);
            if let Some(worker) = context
                .workers
                .iter()
                .find(|worker| &worker.id == recipient)
            {
                assert!(!worker.item.is_empty());
                if worker.owner == context.person {
                    assert_eq!(worker.machine, context.machine);
                }
            }
            assert!(fixture.expect_delegate.is_none() && fixture.expect_tier.is_none());
        }
        Decision::Answer | Decision::Escalate | Decision::Ignore => {
            assert!(fixture.expect_delegate.is_none() && fixture.expect_tier.is_none());
            assert!(fixture.expect_to.is_none());
        }
    }
}

#[tokio::test]
async fn triage_fixtures_have_no_scanner_findings() {
    let status = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new("gitleaks")
            .args(["detect", "--no-git", "--source"])
            .arg(fixtures_dir())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .expect("fixture scan completes within 30 seconds")
    .expect("gitleaks must be installed to scan the triage fixtures");
    assert!(
        status.success(),
        "run gitleaks detect --no-git --source spikes/s7-listener/fixtures for diagnostics"
    );
}
