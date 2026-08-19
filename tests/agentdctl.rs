//! `agentdctl` client-logic tests: the real binary against a real control
//! socket (backed by a FakeBackend relay) — no NATS needed.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use agent_daemon::agent_id::AgentId;
use agent_daemon::control::{self, DaemonHandle, RelayBackend};
use agent_daemon::dispatcher::Dispatcher;
use agent_daemon::error::AgentdError;
use agent_daemon::registry::{Change, Registry};

const CTL_BIN: &str = env!("CARGO_BIN_EXE_agentdctl");

#[derive(Default)]
struct FakeBackend;

impl RelayBackend for FakeBackend {
    async fn apply_changes(&self, _changes: &[Change]) -> Result<(), AgentdError> {
        Ok(())
    }

    async fn consumer_backlog(&self, _id: &AgentId) -> Option<(u64, u64)> {
        None
    }
}

struct Fixture {
    dir: PathBuf,
    socket: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "agentd-ctl-bin-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("agents.d")).unwrap();
        let registry = Arc::new(Registry::load(&dir.join("agents.d")).unwrap());
        let dispatcher = Arc::new(Dispatcher::new(
            registry.clone(),
            Arc::new(agent_daemon::dedup::DedupStore::open_in_memory().unwrap()),
            std::time::Duration::from_secs(3600),
            256 * 1024,
        ));
        let handle = Arc::new(DaemonHandle::new(
            registry,
            dispatcher,
            Arc::new(FakeBackend),
            Arc::new(AtomicBool::new(true)),
        ));
        let socket = dir.join("control.sock");
        let listener = control::bind(&socket).unwrap();
        tokio::spawn(control::serve(handle, listener));
        Self { dir, socket }
    }

    async fn ctl(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = tokio::process::Command::new(CTL_BIN);
        cmd.arg("--socket").arg(&self.socket).args(args);
        cmd.output().await.expect("run agentdctl")
    }

    fn cleanup(&self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[tokio::test]
async fn register_persists_and_lists() {
    let f = Fixture::new("reg");
    let out = f
        .ctl(&["register", "--id", "t_one", "--handler", "/bin/true"])
        .await;
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("ok"));
    assert!(f.dir.join("agents.d/t_one.toml").exists());

    let out = f.ctl(&["list"]).await;
    let list = stdout(&out);
    assert!(list.contains("t_one"), "{list}");
    assert!(list.contains("enabled"), "{list}");
    assert!(list.contains("/bin/true"), "{list}");
    f.cleanup();
}

#[tokio::test]
async fn duplicate_register_fails_loudly() {
    let f = Fixture::new("dup");
    f.ctl(&["register", "--id", "t_one", "--handler", "/bin/true"])
        .await;
    let out = f
        .ctl(&["register", "--id", "t_one", "--handler", "/bin/false"])
        .await;
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("already registered"),
        "{}",
        stderr(&out)
    );
    f.cleanup();
}

#[tokio::test]
async fn update_disable_shows_in_list() {
    let f = Fixture::new("upd");
    f.ctl(&["register", "--id", "t_one", "--handler", "/bin/true"])
        .await;
    let out = f.ctl(&["update", "t_one", "--disable"]).await;
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("updated t_one"));

    let list = stdout(&f.ctl(&["list"]).await);
    assert!(list.contains("disabled"), "{list}");

    // Re-enable and swap concurrency in one update.
    let out = f
        .ctl(&["update", "t_one", "--enable", "--max-concurrency", "4"])
        .await;
    assert!(out.status.success());
    let list = stdout(&f.ctl(&["list"]).await);
    assert!(list.contains("enabled") && list.contains("4"), "{list}");
    f.cleanup();
}

#[tokio::test]
async fn update_missing_agent_errors() {
    let f = Fixture::new("miss");
    let out = f.ctl(&["update", "ghost_agent", "--disable"]).await;
    assert!(!out.status.success());
    assert!(stderr(&out).contains("not registered"), "{}", stderr(&out));
    f.cleanup();
}

#[tokio::test]
async fn status_shape() {
    let f = Fixture::new("stat");
    f.ctl(&["register", "--id", "t_one", "--handler", "/bin/true"])
        .await;
    let out = f.ctl(&["status"]).await;
    assert!(out.status.success());
    let status = stdout(&out);
    assert!(status.contains("daemon: v"), "{status}");
    assert!(status.contains("nats: connected"), "{status}");
    assert!(status.contains("t_one"), "{status}");
    assert!(status.contains("0"), "in_flight column present: {status}");
    f.cleanup();
}

#[tokio::test]
async fn unregister_empties_list_and_reload_ok() {
    let f = Fixture::new("unreg");
    f.ctl(&["register", "--id", "t_one", "--handler", "/bin/true"])
        .await;
    let out = f.ctl(&["unregister", "t_one"]).await;
    assert!(out.status.success());
    assert!(!f.dir.join("agents.d/t_one.toml").exists());

    let list = stdout(&f.ctl(&["list"]).await);
    assert!(list.contains("no agents"), "{list}");

    let out = f.ctl(&["reload"]).await;
    assert!(out.status.success());
    f.cleanup();
}

#[tokio::test]
async fn ctl_without_daemon_reports_connection_error() {
    let dir = Path::new("/nonexistent-agentd-ctl-test");
    let out = tokio::process::Command::new(CTL_BIN)
        .arg("--socket")
        .arg(dir.join("control.sock"))
        .arg("list")
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("is agentd running?"),
        "{}",
        stderr(&out)
    );
}
