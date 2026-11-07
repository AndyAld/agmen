//! An isolated local origin, reusing the CLI's homes and git worktrees.

use std::process::Output;
use std::time::Duration;

use agmen_client::IdentityFile;
use agmen_local::{API, Outcome, Request, Response, frame};
use agmend::{Agmend, Config};
use serde_json::{Value, json};
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::support::{Home, Worktree};

const CALL_WAIT: Duration = Duration::from_secs(10);
const COMMAND_WAIT: Duration = Duration::from_secs(15);
const STOP_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct Local {
    pub home: Home,
    pub project: Value,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Local {
    pub async fn new() -> Self {
        let home = Home::new_in(std::path::Path::new("/tmp"));
        let config = Config::new(
            home.state_base().join("agmend.sock"),
            home.login_file(),
            home.tokens(),
        );
        let daemon = Agmend::bind(config).unwrap();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(daemon.serve_until(async {
            stopped.await.unwrap();
        }));
        let mut local = Self {
            home,
            project: Value::Null,
            stop: Some(stop),
            task,
        };
        local.project = local
            .call("local_project_create", json!({"name": "cli-local"}))
            .await["project"]
            .clone();
        local
    }

    pub async fn call(&self, method: &str, params: Value) -> Value {
        tokio::time::timeout(CALL_WAIT, async {
            let mut socket = UnixStream::connect(self.home.state_base().join("agmend.sock"))
                .await
                .unwrap();
            frame::write_async(&mut socket, &Request::new(1, "hello", json!({"api": API})))
                .await
                .unwrap();
            let hello: Response = frame::read_async(&mut socket).await.unwrap();
            assert!(matches!(hello.outcome, Outcome::Ok { .. }));
            frame::write_async(&mut socket, &Request::new(2, method, params))
                .await
                .unwrap();
            let response: Response = frame::read_async(&mut socket).await.unwrap();
            assert_eq!(response.id, 2);
            match response.outcome {
                Outcome::Ok { result } => result,
                Outcome::Err { error } => panic!("{method}: {error:?}"),
            }
        })
        .await
        .unwrap()
    }

    pub async fn member(&self, name: &str) -> Worktree {
        let identity: IdentityFile = serde_json::from_value(
            self.call(
                "local_mint",
                json!({"project": self.project, "name": name, "kind": "coordinator"}),
            )
            .await,
        )
        .unwrap();
        let worktree = Worktree::new(&self.home);
        let exclude = worktree.git(&self.home, &["rev-parse", "--git-path", "info/exclude"]);
        std::fs::write(worktree.path().join(exclude.trim()), ".agmen/\n").unwrap();
        let dir = worktree.path().join(".agmen");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("identity.json");
        std::fs::write(&path, serde_json::to_vec(&identity).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        worktree
    }

    pub async fn run(&self, worktree: &Worktree, args: &[&str]) -> Output {
        self.run_env(worktree, args, &[]).await
    }

    pub async fn run_env(
        &self,
        worktree: &Worktree,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Output {
        let mut command = self.home.agmen(&worktree.path(), args);
        command.envs(env.iter().copied());
        tokio::time::timeout(
            COMMAND_WAIT,
            tokio::process::Command::from(command)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap()
    }

    pub async fn stop(mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        tokio::time::timeout(STOP_WAIT, &mut self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

impl Drop for Local {
    fn drop(&mut self) {
        self.task.abort();
    }
}
