//! `ds-stall`: spike S5 (Task 15s). Starts fresh projects at once, each with
//! watch-like persistent sessions (clean start off, a session expiry, the
//! watch's four subscriptions) and a hub-like publisher, and measures the
//! time from each publish to each session's receipt. Run it against two
//! brokers that differ only in `durable_sessions.enable` to compare them.
//!
//! ```text
//! set -a; . deploy/dev/.secrets/dev.env; set +a
//! cargo run -p s5-ds-stall --release -- --mqtt localhost:2883 \
//!     --projects 32 --sessions 2 --rounds 5 --deadline-s 20 --trials 10
//! ```
//!
//! With durable sessions on, stalls appear only while the broker's DS
//! storage is still learning the topic structure, so start from a fresh
//! broker data directory to reproduce them (Task 15s report).
//!
//! Every session it opens is closed with a session expiry of 0, so it
//! leaves nothing at the broker.

mod auth;
mod probe;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};

use crate::probe::{Api, Delivery, Kind, Trial};

/// A message slower than this counts as stalled.
const DEFAULT_STALL_MS: u64 = 2000;

const USAGE: &str = "usage: ds-stall [--mqtt host:port] [--api URL] [--projects N] \
[--sessions N] [--rounds N] [--interval-ms N] [--deadline-s N] [--stall-ms N] \
[--trials N] [--expiry-s N] [--nudge-s N]
--mqtt defaults to AGMEN_IT_MQTT. --api snapshots a session that misses a message; \
its key is DS_STALL_API_KEY and DS_STALL_API_SECRET, else AGMEN_IT_EMQX_API_KEY and \
AGMEN_IT_EMQX_API_SECRET. --nudge-s: after the deadline, a project whose sessions \
still miss messages publishes one more round and waits up to N s for them.";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (trial, trials, stall) = parse(std::env::args().skip(1))?;
    let signer = Arc::new(auth::Signer::from_env()?);
    println!(
        "broker {}:{} projects {} sessions/project {} rounds {} interval {:?} deadline {:?} expiry {}s trials {trials}",
        trial.broker.0,
        trial.broker.1,
        trial.projects,
        trial.sessions,
        trial.rounds,
        trial.interval,
        trial.deadline,
        trial.expiry,
    );
    let mut all = Vec::new();
    for n in 1..=trials {
        let outcome = trial.run(&signer).await?;
        println!("trial {n}: {}", summary(&outcome.deliveries, stall));
        for error in &outcome.errors {
            println!("  error {error}");
        }
        for snapshot in &outcome.snapshots {
            println!("  {snapshot}");
        }
        all.extend(outcome.deliveries);
    }
    println!("all: {}", summary(&all, stall));
    Ok(())
}

/// Counts, stall rate by session and latency percentiles.
fn summary(deliveries: &[Delivery], stall: Duration) -> String {
    let mut latencies: Vec<Duration> = deliveries.iter().filter_map(|d| d.latency).collect();
    latencies.sort_unstable();
    let missing = deliveries.len() - latencies.len();
    let late = latencies.iter().filter(|l| **l > stall).count();
    let mut sessions: Vec<&str> = deliveries.iter().map(|d| d.client_id.as_str()).collect();
    sessions.sort_unstable();
    sessions.dedup();
    let mut stalled: Vec<&str> = deliveries
        .iter()
        .filter(|d| d.latency.is_none_or(|l| l > stall))
        .map(|d| d.client_id.as_str())
        .collect();
    stalled.sort_unstable();
    stalled.dedup();
    let mut projects: Vec<&str> = deliveries.iter().map(|d| d.project.as_str()).collect();
    projects.sort_unstable();
    projects.dedup();
    let stalled_projects = projects
        .iter()
        .filter(|project| {
            deliveries
                .iter()
                .any(|d| d.project == **project && d.latency.is_none_or(|l| l > stall))
        })
        .count();
    let first_round_bad = deliveries
        .iter()
        .filter(|d| d.round == 0 && d.latency.is_none_or(|l| l > stall))
        .count();
    let by_kind: Vec<String> = [Kind::Feed, Kind::Inbox, Kind::Ack]
        .iter()
        .map(|kind| {
            let bad = deliveries
                .iter()
                .filter(|d| d.kind == *kind && d.latency.is_none_or(|l| l > stall))
                .count();
            format!("{kind:?}={bad}")
        })
        .collect();
    format!(
        "messages {} delivered {} late(>{stall:?}) {late} missing {missing} (round 0: {first_round_bad}; {}) \
         sessions stalled {}/{} projects stalled {stalled_projects}/{} p50 {} p99 {} max {}",
        deliveries.len(),
        latencies.len(),
        by_kind.join(" "),
        stalled.len(),
        sessions.len(),
        projects.len(),
        percentile(&latencies, 50),
        percentile(&latencies, 99),
        percentile(&latencies, 100),
    )
}

/// The `p`th percentile of sorted `latencies`, in milliseconds.
fn percentile(latencies: &[Duration], p: usize) -> String {
    match latencies.len() {
        0 => "-".to_owned(),
        n => {
            let index = (n * p).div_ceil(100).clamp(1, n) - 1;
            latencies
                .get(index)
                .map_or_else(|| "-".to_owned(), |l| format!("{}ms", l.as_millis()))
        }
    }
}

fn parse(mut args: impl Iterator<Item = String>) -> anyhow::Result<(Trial, u32, Duration)> {
    let mut broker = None;
    let mut api = None;
    let mut trial = Trial {
        broker: (String::new(), 0),
        projects: 8,
        sessions: 2,
        rounds: 5,
        interval: Duration::from_millis(500),
        deadline: Duration::from_secs(30),
        expiry: 86_400,
        api: None,
        nudge: None,
    };
    let mut trials = 1;
    let mut stall = Duration::from_millis(DEFAULT_STALL_MS);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .with_context(|| format!("{flag} needs a value\n{USAGE}"))?;
        let number = || -> anyhow::Result<u64> {
            value
                .parse()
                .with_context(|| format!("{flag}: {value} is not a number"))
        };
        match flag.as_str() {
            "--mqtt" => broker = Some(value.clone()),
            "--api" => api = Some(value.clone()),
            "--projects" => trial.projects = usize::try_from(number()?)?,
            "--sessions" => trial.sessions = usize::try_from(number()?)?,
            "--rounds" => trial.rounds = u32::try_from(number()?)?,
            "--interval-ms" => trial.interval = Duration::from_millis(number()?),
            "--deadline-s" => trial.deadline = Duration::from_secs(number()?),
            "--stall-ms" => stall = Duration::from_millis(number()?),
            "--trials" => trials = u32::try_from(number()?)?,
            "--expiry-s" => trial.expiry = u32::try_from(number()?)?,
            "--nudge-s" => trial.nudge = Some(Duration::from_secs(number()?)),
            _ => bail!("unknown flag {flag}\n{USAGE}"),
        }
    }
    let broker = match broker {
        Some(broker) => broker,
        None => std::env::var("AGMEN_IT_MQTT")
            .context("no --mqtt and AGMEN_IT_MQTT is unset")?
            .trim_start_matches("mqtt://")
            .to_owned(),
    };
    let (host, port) = broker
        .rsplit_once(':')
        .with_context(|| format!("{broker} is not host:port"))?;
    trial.broker = (host.to_owned(), port.parse().context("the broker port")?);
    trial.api = api
        .map(|base| -> anyhow::Result<Api> {
            let var = |own: &str, shared: &str| {
                std::env::var(own)
                    .or_else(|_| std::env::var(shared))
                    .with_context(|| format!("--api needs {own} or {shared}"))
            };
            Ok(Api {
                base,
                key: var("DS_STALL_API_KEY", "AGMEN_IT_EMQX_API_KEY")?,
                secret: var("DS_STALL_API_SECRET", "AGMEN_IT_EMQX_API_SECRET")?,
            })
        })
        .transpose()?;
    Ok((trial, trials, stall))
}
