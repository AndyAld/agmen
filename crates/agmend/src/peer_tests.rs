//! Socket probes shared by the worker and listener ancestry regressions.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agmen_local::{Outcome, Request, Response, frame};
use serde_json::json;

pub(crate) const WAIT: Duration = Duration::from_secs(10);

// Each fork's parent waits, preserving ancestry while Perl's POSIX::setsid
// changes the child's process group without a platform-specific executable.
const CLIENT: &str = r#"#!/usr/bin/perl
use strict;
use warnings;
use POSIX qw(setsid);
use IO::Socket::UNIX;
use JSON::PP;
use FindBin;
my $dir = $FindBin::Bin;
my $person = @ARGV && $ARGV[0] eq '--person';
unless ($person) {
    open my $depth_file, '<', "$dir/depth" or die $!;
    my $depth = <$depth_file>;
    for (1 .. $depth) {
        my $child = fork();
        defined $child or die $!;
        if ($child) {
            waitpid($child, 0);
            exit($? >> 8);
        }
        setsid() >= 0 or die "setsid: $!";
    }
}
my $socket = IO::Socket::UNIX->new(Type => SOCK_STREAM, Peer => "$dir/peer.sock") or die $!;
$socket->autoflush(1);
open my $input, '<', "$dir/requests.json" or die $!;
my $requests = decode_json(do { local $/; <$input> });
sub read_exact {
    my ($size) = @_;
    my $bytes = '';
    while (length($bytes) < $size) {
        my $count = sysread($socket, my $part, $size - length($bytes));
        defined($count) && $count > 0 or die 'short frame';
        $bytes .= $part;
    }
    return $bytes;
}
my @answers;
for my $request (@$requests) {
    my $body = encode_json($request);
    print $socket pack('N', length($body)), $body or die $!;
    push @answers, decode_json(read_exact(unpack('N', read_exact(4))));
}
close $socket;
open my $output, '>', "$dir/answers.json" or die $!;
print $output encode_json(\@answers);
close $output;
unless ($person) {
    select(undef, undef, undef, 0.01) until -e "$dir/finish" || !-d $dir;
}
"#;

pub(crate) fn client(dir: &Path, depth: usize) -> std::path::PathBuf {
    let path = dir.join("fake-claude");
    std::fs::write(&path, CLIENT).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(dir.join("depth"), depth.to_string()).unwrap();
    path
}

pub(crate) fn requests(dir: &Path, project: &agmen_proto::ProjectId) {
    let post = json!({"project":project,"envelope":{
        "v":1,"id":agmen_core::new_message_id(agmen_proto::Timestamp::now()).unwrap(),
        "verb":"FYI","data":{},"body":"ancestry probe", "to":[],
        "sent_at":agmen_proto::Timestamp::now()
    }});
    let requests = [
        Request::new(1, "hello", json!({"api":1})),
        Request::new(2, "agent_list", json!({})),
        Request::new(3, "post", post),
    ];
    std::fs::write(
        dir.join("requests.json"),
        serde_json::to_vec(&requests).unwrap(),
    )
    .unwrap();
}

pub(crate) async fn accept(
    listener: &tokio::net::UnixListener,
    core: Arc<crate::Core>,
) -> tokio::task::JoinHandle<()> {
    let (stream, _) = tokio::time::timeout(WAIT, listener.accept())
        .await
        .unwrap()
        .unwrap();
    tokio::spawn(crate::api::serve(stream, core))
}

pub(crate) async fn answers(dir: &Path) -> Vec<Response> {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Ok(bytes) = tokio::fs::read(dir.join("answers.json")).await
                && let Ok(answers) = serde_json::from_slice(&bytes)
            {
                return answers;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

pub(crate) fn assert_refused(answers: &[Response]) {
    assert!(matches!(answers[0].outcome, Outcome::Ok { .. }));
    assert!(
        answers[1..]
            .iter()
            .all(|answer| matches!(answer.outcome, Outcome::Err { .. })),
        "agent escaped containment: {answers:?}"
    );
    for response in &answers[1..] {
        let Outcome::Err { error } = &response.outcome else {
            panic!("agent escaped containment: {response:?}");
        };
        assert_eq!(error.code, agmen_local::ErrorCode::Forbidden);
        assert_eq!(error.message, "agents cannot call owner-only methods");
    }
}

pub(crate) async fn human_controls(
    core: Arc<crate::Core>,
    dir: &Path,
    listener: &tokio::net::UnixListener,
    project: &agmen_proto::ProjectId,
) {
    let (mut person, server) = tokio::net::UnixStream::pair().unwrap();
    let task = tokio::spawn(crate::api::serve(server, core.clone()));
    let calls: Vec<Request> =
        serde_json::from_slice(&std::fs::read(dir.join("requests.json")).unwrap()).unwrap();
    for call in calls {
        frame::write_async(&mut person, &call).await.unwrap();
        let answer: Response = tokio::time::timeout(WAIT, frame::read_async(&mut person))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(answer.outcome, Outcome::Ok { .. }), "{answer:?}");
    }
    drop(person);
    tokio::time::timeout(WAIT, task).await.unwrap().unwrap();

    let calls: Vec<_> = [
        ("hello", json!({"api":1})),
        ("state", json!({"project":project,"key":"roster"})),
        ("inbox", json!({"project":project})),
        ("agent_list", json!({})),
        ("watch", json!({"project":project})),
    ]
    .into_iter()
    .enumerate()
    .map(|(id, (method, params))| Request::new(id as u64 + 1, method, params))
    .collect();
    std::fs::write(
        dir.join("requests.json"),
        serde_json::to_vec(&calls).unwrap(),
    )
    .unwrap();
    let mut person = tokio::process::Command::new(dir.join("fake-claude"))
        .arg("--person")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let task = accept(listener, core).await;
    assert!(
        tokio::time::timeout(WAIT, person.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    for answer in answers(dir).await {
        assert!(matches!(answer.outcome, Outcome::Ok { .. }), "{answer:?}");
    }
    tokio::time::timeout(WAIT, task).await.unwrap().unwrap();
}
