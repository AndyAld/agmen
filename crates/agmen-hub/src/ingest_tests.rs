use super::*;

fn id(name: &str) -> IdentityId {
    format!("{name:0<8}").parse().unwrap()
}

fn member(name: &str, kind: IdentityKind, owner: Option<&str>, role: Option<Role>) -> Member {
    Member {
        id: id(name),
        kind,
        owner: owner.map(id),
        role,
    }
}

/// andy (admin) owns sol and coord; adam (coordinator role) owns w3f.
fn roster() -> Roster {
    Roster(vec![
        member("andy", IdentityKind::Human, None, Some(Role::Admin)),
        member("adam", IdentityKind::Human, None, Some(Role::Coordinator)),
        member("sol", IdentityKind::Agent, Some("andy"), None),
        member("coord", IdentityKind::Coordinator, Some("andy"), None),
        member("w3f", IdentityKind::Agent, Some("adam"), None),
    ])
}

fn sender(name: &str, kind: IdentityKind, owner: Option<&str>) -> Sender {
    Sender {
        id: id(name),
        kind,
        handle: name.to_owned(),
        owner: owner.map(id),
    }
}

fn ids(names: &[&str]) -> BTreeSet<IdentityId> {
    names.iter().map(|name| id(name)).collect()
}

fn inboxes(names: &[&str]) -> BTreeSet<Stream> {
    names.iter().map(|name| Stream::Inbox(id(name))).collect()
}

fn envelope(verb: &str, data: &str, extra: &str) -> ClientEnvelope {
    let raw = format!(
        r#"{{"v":1,"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS","verb":"{verb}","data":{data},"body":"hi","sent_at":"2026-10-25T16:04:11Z"{extra}}}"#
    );
    ClientEnvelope::from_json(raw.as_bytes()).unwrap()
}

#[test]
fn hub_identity_can_never_be_minted() {
    let hub = hub_identity();
    assert_eq!(hub.as_str(), HUB_IDENTITY);
    // db::new_id mints from an alphabet without 0, 1, l and o.
    assert!(HUB_IDENTITY.contains('0'));
}

#[test]
fn verb_table_follows_protocol_section_4() {
    use IdentityKind::{Agent, Coordinator, Hub, Human, Listener};
    for verb in [Verb::Ruling, Verb::PushOk, Verb::PushNo] {
        assert!(!may_send(Agent, verb), "agent {verb}");
        assert!(!may_send(Listener, verb), "listener {verb}");
    }
    for verb in [Verb::Claim, Verb::Done, Verb::Lend] {
        assert!(!may_send(Listener, verb), "listener {verb}");
    }
    assert!(may_send(Human, Verb::Ruling) && may_send(Human, Verb::PushOk));
    assert!(may_send(Coordinator, Verb::Ruling) && !may_send(Coordinator, Verb::PushOk));
    assert!(may_send(Agent, Verb::Claim) && !may_send(Human, Verb::Claim));
    assert!(may_send(Coordinator, Verb::Assign) && !may_send(Agent, Verb::Assign));
    assert!(!may_send(Agent, Verb::Rollcall) && may_send(Human, Verb::Rollcall));
    assert!(may_send(Listener, Verb::Delegated) && !may_send(Agent, Verb::Delegated));
    let every_role = [
        Verb::Hello,
        Verb::Bye,
        Verb::Fyi,
        Verb::Ask,
        Verb::Answer,
        Verb::Ack,
        Verb::Status,
        Verb::Dm,
        Verb::Alert,
        Verb::Found,
    ];
    for kind in [Human, Agent, Coordinator, Listener] {
        for verb in every_role {
            assert!(may_send(kind, verb), "{kind} {verb}");
        }
    }
    for verb in Verb::ALL {
        assert!(!may_send(Hub, verb), "hub {verb} through ingest");
    }
    for verb in [
        Verb::Grant,
        Verb::Deny,
        Verb::Item,
        Verb::State,
        Verb::Review,
        Verb::Plan,
    ] {
        for kind in [Human, Agent, Coordinator, Listener] {
            assert!(!may_send(kind, verb), "{kind} {verb}");
        }
    }
}

#[test]
fn group_mentions_expand_without_the_sender() {
    let andy = sender("andy", IdentityKind::Human, None);
    let roster = roster();
    assert_eq!(
        expand_mentions(&andy, &[Mention::All], &roster).unwrap(),
        ids(&["adam", "sol", "coord", "w3f"])
    );
    assert_eq!(
        expand_mentions(&andy, &[Mention::Agents], &roster).unwrap(),
        ids(&["sol", "coord", "w3f"])
    );
    assert_eq!(
        expand_mentions(&andy, &[Mention::Humans], &roster).unwrap(),
        ids(&["adam"])
    );
    let coord = sender("coord", IdentityKind::Coordinator, Some("andy"));
    assert_eq!(
        expand_mentions(
            &coord,
            &[Mention::Humans, Mention::Identity(id("sol"))],
            &roster
        )
        .unwrap(),
        ids(&["andy", "adam", "sol"])
    );
}

#[test]
fn mention_rules_by_kind() {
    let roster = roster();
    let sol = sender("sol", IdentityKind::Agent, Some("andy"));
    for group in [Mention::All, Mention::Agents, Mention::Humans] {
        assert_eq!(
            expand_mentions(&sol, &[group], &roster),
            Err(NackReason::ForbiddenMention)
        );
    }
    let lane = Mention::Lane("L-UI".parse().unwrap());
    let role = Mention::Role("reviewer".parse().unwrap());
    assert_eq!(
        expand_mentions(&sol, std::slice::from_ref(&lane), &roster),
        Err(NackReason::ForbiddenMention)
    );
    let andy = sender("andy", IdentityKind::Human, None);
    // Ruling R14: no lane or role expansion in the MVP.
    assert_eq!(
        expand_mentions(&andy, &[lane], &roster),
        Err(NackReason::Schema)
    );
    assert_eq!(
        expand_mentions(&andy, &[role], &roster),
        Err(NackReason::Schema)
    );
    assert_eq!(
        expand_mentions(&sol, &[Mention::Identity(id("coord"))], &roster).unwrap(),
        ids(&["coord"])
    );
    assert_eq!(
        expand_mentions(&sol, &[Mention::Identity(id("stranger"))], &roster),
        Err(NackReason::ForbiddenMention)
    );
}

#[test]
fn alerts_mention_owners_and_coordinators() {
    let roster = roster();
    let w3f = sender("w3f", IdentityKind::Agent, Some("adam"));
    let alert = envelope("ALERT", r#"{"need":"help"}"#, "");
    // adam owns w3f and holds the coordinator role; coord is a coordinator
    // identity; andy is an admin, which R35 leaves out.
    assert_eq!(
        mentioned(Verb::Alert, &alert, &w3f, &roster, None).unwrap(),
        vec![id("adam"), id("coord")]
    );
    let fyi = envelope("FYI", "{}", "");
    assert!(
        mentioned(Verb::Fyi, &fyi, &w3f, &roster, None)
            .unwrap()
            .is_empty()
    );
    let coords: Vec<_> = roster.coordinators().cloned().collect();
    assert_eq!(coords, vec![id("adam"), id("coord")]);
}

#[test]
fn reply_to_a_human_mentions_them() {
    let roster = roster();
    let sol = sender("sol", IdentityKind::Agent, Some("andy"));
    let answer = envelope(
        "ANSWER",
        "{}",
        r#","reply_to":"01JBX6Q4T7W3C2M8Y9K5N0P1RT""#,
    );
    let parent = |from: &str, from_kind| Parent {
        from: id(from),
        from_kind,
        streams: BTreeSet::from([Stream::Feed]),
    };
    assert_eq!(
        mentioned(
            Verb::Answer,
            &answer,
            &sol,
            &roster,
            Some(&parent("andy", IdentityKind::Human))
        )
        .unwrap(),
        vec![id("andy")]
    );
    assert!(
        mentioned(
            Verb::Answer,
            &answer,
            &sol,
            &roster,
            Some(&parent("coord", IdentityKind::Coordinator))
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn dms_fan_out_to_recipients_sender_and_owners() {
    let roster = roster();
    let sol = sender("sol", IdentityKind::Agent, Some("andy"));
    let dm = envelope("DM", "{}", &format!(r#","to":["{}"]"#, id("w3f")));
    assert_eq!(
        route(Verb::Dm, &dm, &sol, &roster, None).unwrap(),
        inboxes(&["sol", "andy", "w3f", "adam"])
    );
    let ask = envelope("ASK", "{}", &format!(r#","to":["{}"]"#, id("coord")));
    assert_eq!(
        route(Verb::Ask, &ask, &sol, &roster, None).unwrap(),
        inboxes(&["sol", "andy", "coord"])
    );
    let stranger = envelope("DM", "{}", &format!(r#","to":["{}"]"#, id("stranger")));
    assert_eq!(
        route(Verb::Dm, &stranger, &sol, &roster, None),
        Err(NackReason::Schema)
    );
    let open_ask = envelope("ASK", "{}", "");
    assert_eq!(
        route(Verb::Ask, &open_ask, &sol, &roster, None).unwrap(),
        BTreeSet::from([Stream::Feed])
    );
}

#[test]
fn answers_follow_their_parent() {
    let roster = roster();
    let w3f = sender("w3f", IdentityKind::Agent, Some("adam"));
    let answer = envelope(
        "ANSWER",
        "{}",
        r#","reply_to":"01JBX6Q4T7W3C2M8Y9K5N0P1RT""#,
    );
    let private = Parent {
        from: id("sol"),
        from_kind: IdentityKind::Agent,
        streams: inboxes(&["sol", "andy", "w3f", "adam"]),
    };
    assert_eq!(
        route(Verb::Answer, &answer, &w3f, &roster, Some(&private)).unwrap(),
        private.streams
    );
    let public = Parent {
        streams: BTreeSet::from([Stream::Feed]),
        ..private
    };
    assert_eq!(
        route(Verb::Answer, &answer, &w3f, &roster, Some(&public)).unwrap(),
        BTreeSet::from([Stream::Feed])
    );
    assert_eq!(
        route(Verb::Answer, &answer, &w3f, &roster, None).unwrap(),
        BTreeSet::from([Stream::Feed])
    );
}

#[test]
fn assign_goes_to_feed_and_assignee() {
    let roster = roster();
    let coord = sender("coord", IdentityKind::Coordinator, Some("andy"));
    let assign = envelope(
        "ASSIGN",
        &format!(r#"{{"item":"B-1","to":"{}"}}"#, id("w3f")),
        "",
    );
    let streams = route(Verb::Assign, &assign, &coord, &roster, None).unwrap();
    assert_eq!(
        streams,
        BTreeSet::from([Stream::Feed, Stream::Inbox(id("w3f"))])
    );
    assert_eq!(ack_stream(&streams, &coord.id), Some(Stream::Feed));
}

#[test]
fn ack_reports_the_sender_visible_copy() {
    let dm = inboxes(&["sol", "andy", "w3f", "adam"]);
    assert_eq!(ack_stream(&dm, &id("sol")), Some(Stream::Inbox(id("sol"))));
    let grant = inboxes(&["w3f"]);
    assert_eq!(
        ack_stream(&grant, &hub_identity()),
        Some(Stream::Inbox(id("w3f")))
    );
    assert_eq!(ack_stream(&BTreeSet::new(), &id("sol")), None);
}

#[test]
fn rate_limit_allows_a_burst_of_30_then_10_per_second() {
    let mut limiter = RateLimiter::default();
    let sol = id("sol");
    let start = Instant::now();
    for n in 0..30 {
        assert_eq!(limiter.take(&sol, start), Ok(()), "publish {n}");
    }
    let wait = limiter.take(&sol, start).unwrap_err().unwrap();
    assert!(
        wait > Duration::from_millis(90) && wait <= Duration::from_millis(100),
        "{wait:?}"
    );
    let later = start + Duration::from_millis(100);
    assert_eq!(limiter.take(&sol, later), Ok(()));
    assert!(limiter.take(&sol, later).is_err());
    // Another sender has a bucket of its own.
    assert_eq!(limiter.take(&id("w3f"), start), Ok(()));
}

#[test]
fn rate_limiter_stays_bounded() {
    let mut limiter = RateLimiter::default();
    let start = Instant::now();
    for n in 0..MAX_TRACKED {
        let sender: IdentityId = format!("s{n:0>9}").parse().unwrap();
        limiter.take(&sender, start).unwrap();
    }
    // Every bucket is still refilling, so a new sender has no room.
    assert_eq!(limiter.take(&id("late"), start), Err(None));
    // Once the buckets refill they are forgotten and the new sender fits.
    let refilled = start + Duration::from_secs(1);
    assert_eq!(limiter.take(&id("late"), refilled), Ok(()));
    assert_eq!(limiter.buckets.len(), 1);
}

#[test]
fn hub_message_ids_are_ulids_in_time_order() {
    let now: Timestamp = "2026-10-25T16:04:11.402Z".parse().unwrap();
    let first = new_message_id(now).unwrap();
    let second = new_message_id(now.checked_add(Duration::from_millis(1)).unwrap()).unwrap();
    assert_eq!(first.as_str().len(), 26);
    assert!(first < second, "{first} {second}");
    assert_ne!(new_message_id(now).unwrap(), first);
}

#[test]
fn message_id_is_read_from_a_bad_envelope() {
    assert_eq!(
        message_id(br#"{"id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS","verb":"NOPE"}"#)
            .unwrap()
            .as_str(),
        "01JBX6Q4T7W3C2M8Y9K5N0P1RS"
    );
    assert_eq!(message_id(br#"{"verb":"FYI"}"#), None);
    assert_eq!(message_id(b"not json"), None);
}

#[test]
fn envelope_hash_ignores_spacing() {
    let a = envelope("FYI", "{}", "");
    let spaced = ClientEnvelope::from_json(
        br#"{ "sent_at":"2026-10-25T16:04:11Z", "body":"hi", "data":{}, "verb":"FYI",
             "id":"01JBX6Q4T7W3C2M8Y9K5N0P1RS", "v":1 }"#,
    )
    .unwrap();
    assert_eq!(envelope_hash(&a).unwrap(), envelope_hash(&spaced).unwrap());
    let other = envelope("FYI", "{}", r#","items":["B-1"]"#);
    assert_ne!(envelope_hash(&a).unwrap(), envelope_hash(&other).unwrap());
}

#[test]
fn never_mentions_the_sender() {
    let roster = roster();
    let andy = sender("andy", IdentityKind::Human, None);
    let own = Parent {
        from: id("andy"),
        from_kind: IdentityKind::Human,
        streams: BTreeSet::from([Stream::Feed]),
    };
    let reply = envelope(
        "ANSWER",
        "{}",
        r#","reply_to":"01JBX6Q4T7W3C2M8Y9K5N0P1RT","mentions":["all"]"#,
    );
    assert_eq!(
        mentioned(Verb::Answer, &reply, &andy, &roster, Some(&own)).unwrap(),
        // Sorted by id.
        vec![id("adam"), id("coord"), id("sol"), id("w3f")]
    );
    // adam holds the coordinator role and raises an ALERT himself.
    let adam = sender("adam", IdentityKind::Human, None);
    let alert = envelope("ALERT", r#"{"need":"help"}"#, "");
    assert_eq!(
        mentioned(Verb::Alert, &alert, &adam, &roster, None).unwrap(),
        vec![id("coord")]
    );
}
