//! Control-plane tests: real Unix socket, real registry/dispatcher, fake
//! relay backend (the `RelayBackend` seam). No NATS needed.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use agent_daemon::agent_id::AgentId;
use agent_daemon::control::{self, DaemonHandle, Request, Response};
use agent_daemon::dispatcher::Dispatcher;
use agent_daemon::error::AgentdError;
use agent_daemon::registry::{AgentConfig, Change, Registry};

/// Recording fake: captures applied changes, serves optional backlog.
#[derive(Default)]
struct FakeBackend {
    changes: Mutex<Vec<Change>>,
    backlog: Mutex<std::collections::HashMap<AgentId, (u64, u64)>>,
}

impl agent_daemon::control::RelayBackend for FakeBackend {
    async fn apply_changes(&self, changes: &[Change]) -> Result<(), AgentdError> {
        self.changes.lock().unwrap().extend(changes.iter().cloned());
        Ok(())
    }

    async fn consumer_backlog(&self, id: &AgentId) -> Option<(u64, u64)> {
        self.backlog.lock().unwrap().get(id).copied()
    }
}

struct Fixture {
    dir: PathBuf,
    socket: PathBuf,
    backend: Arc<FakeBackend>,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "agentd-ctl-{tag}-{}-{}",
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
        let backend = Arc::new(FakeBackend::default());
        let handle = Arc::new(DaemonHandle::new(
            registry,
            dispatcher,
            backend.clone(),
            Arc::new(AtomicBool::new(true)),
        ));
        let socket = dir.join("control.sock");
        let listener = control::bind(&socket).unwrap();
        tokio::spawn(control::serve(handle.clone(), listener));
        Self {
            dir,
            socket,
            backend,
        }
    }

    async fn rpc(&self, request: Request) -> Response {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let mut stream = tokio::net::UnixStream::connect(&self.socket).await.unwrap();
        let mut line = serde_json::to_string(&request).unwrap();
        line.push('\n');
        stream.write_all(line.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        let mut reader = tokio::io::BufReader::new(stream);
        let mut response = String::new();
        reader.read_line(&mut response).await.unwrap();
        serde_json::from_str(&response).unwrap()
    }

    fn cleanup(&self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn agent(id: &str, handler: &str, enabled: bool) -> AgentConfig {
    AgentConfig {
        agent_id: AgentId::parse(id).unwrap(),
        handler: PathBuf::from(handler),
        max_concurrency: 1,
        working_directory: None,
        enabled,
    }
}

#[tokio::test]
async fn socket_is_created_with_mode_0600() {
    let f = Fixture::new("mode");
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&f.socket).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "control socket must be 0600 (§7.3)");
    f.cleanup();
}

#[tokio::test]
async fn register_persists_and_applies_added_change() {
    let f = Fixture::new("register");
    let r = f
        .rpc(Request::Register {
            agent: agent("a_test", "/bin/true", true),
        })
        .await;
    assert!(r.ok, "{r:?}");
    // File persisted with the id name.
    assert!(f.dir.join("agents.d/a_test.toml").exists());
    // The Added change reached the backend (consumer would bind).
    let changes = f.backend.changes.lock().unwrap().clone();
    assert!(matches!(&changes[..], [Change::Added(c)] if c.agent_id.as_str() == "a_test"));
    f.cleanup();
}

#[tokio::test]
async fn duplicate_register_is_an_error() {
    let f = Fixture::new("dup");
    f.rpc(Request::Register {
        agent: agent("a_test", "/bin/true", true),
    })
    .await;
    let r = f
        .rpc(Request::Register {
            agent: agent("a_test", "/bin/other", true),
        })
        .await;
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("already registered"));
    f.cleanup();
}

#[tokio::test]
async fn update_disable_unregister_flow() {
    let f = Fixture::new("flow");
    f.rpc(Request::Register {
        agent: agent("a_test", "/bin/true", true),
    })
    .await;

    // Update (new handler).
    let r = f
        .rpc(Request::Update {
            agent: agent("a_test", "/bin/other", true),
        })
        .await;
    assert!(r.ok);
    assert!(matches!(
        &f.backend.changes.lock().unwrap()[..],
        [Change::Added(_), Change::Updated(c)] if c.handler == std::path::Path::new("/bin/other")
    ));

    // Disable.
    let r = f
        .rpc(Request::Update {
            agent: agent("a_test", "/bin/other", false),
        })
        .await;
    assert!(r.ok);
    assert!(matches!(
        &f.backend.changes.lock().unwrap()[..],
        [_, _, Change::Updated(c)] if !c.enabled
    ));

    // List reflects the state.
    let r = f.rpc(Request::List).await;
    let agents = r.agents.unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].handler, PathBuf::from("/bin/other"));
    assert!(!agents[0].enabled);

    // Unregister removes the file and emits Removed.
    let r = f
        .rpc(Request::Unregister {
            agent_id: AgentId::parse("a_test").unwrap(),
        })
        .await;
    assert!(r.ok);
    assert!(!f.dir.join("agents.d/a_test.toml").exists());
    assert!(matches!(
        f.backend.changes.lock().unwrap().last(),
        Some(Change::Removed(id)) if id.as_str() == "a_test"
    ));
    f.cleanup();
}

#[tokio::test]
async fn reload_applies_disk_diff() {
    let f = Fixture::new("reload");
    // An agent appears on disk (e.g. hand-written file) while running.
    let cfg = agent("disk_agent", "/bin/true", true);
    std::fs::write(
        f.dir.join("agents.d/disk_agent.toml"),
        toml::to_string_pretty(&cfg).unwrap(),
    )
    .unwrap();
    let r = f.rpc(Request::Reload).await;
    assert!(r.ok);
    assert!(matches!(
        &f.backend.changes.lock().unwrap()[..],
        [Change::Added(c)] if c.agent_id.as_str() == "disk_agent"
    ));
    f.cleanup();
}

#[tokio::test]
async fn status_reports_agents_and_backlog() {
    let f = Fixture::new("status");
    f.rpc(Request::Register {
        agent: agent("a_test", "/bin/true", true),
    })
    .await;
    f.backend
        .backlog
        .lock()
        .unwrap()
        .insert(AgentId::parse("a_test").unwrap(), (3, 1));

    let r = f.rpc(Request::Status).await;
    assert!(r.ok);
    let status = r.status.unwrap();
    assert!(status.nats_connected);
    assert_eq!(status.agents.len(), 1);
    let a = &status.agents[0];
    assert_eq!(a.agent_id.as_str(), "a_test");
    assert_eq!(a.in_flight, 0);
    assert_eq!(a.num_pending, Some(3));
    assert_eq!(a.num_ack_pending, Some(1));
    f.cleanup();
}

#[tokio::test]
async fn malformed_request_returns_error_not_disconnect() {
    let f = Fixture::new("malformed");
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let mut stream = tokio::net::UnixStream::connect(&f.socket).await.unwrap();
    stream.write_all(b"this is not json\n").await.unwrap();
    stream.flush().await.unwrap();
    let mut reader = tokio::io::BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).await.unwrap();
    let parsed: Response = serde_json::from_str(&response).unwrap();
    assert!(!parsed.ok);
    assert!(parsed.error.unwrap().contains("malformed request"));
    f.cleanup();
}

#[tokio::test]
async fn stale_socket_file_is_replaced_at_bind() {
    let f = Fixture::new("stale");
    // Simulate a leftover from a crash: a plain file (or garbage) where the
    // socket should be. The next bind must replace it and still serve.
    drop(f);
    let dir = std::env::temp_dir().join(format!(
        "agentd-ctl-stale2-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.join("agents.d")).unwrap();
    let socket = dir.join("control.sock");
    std::fs::write(&socket, b"leftover garbage").unwrap();

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
        Arc::new(FakeBackend::default()),
        Arc::new(AtomicBool::new(true)),
    ));
    let listener = control::bind(&socket).expect("bind over stale file");
    tokio::spawn(control::serve(handle, listener));

    let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let mut line = serde_json::to_string(&Request::List).unwrap();
    line.push('\n');
    stream.write_all(line.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
    let mut response = String::new();
    let mut reader = tokio::io::BufReader::new(stream);
    reader.read_line(&mut response).await.unwrap();
    let parsed: Response = serde_json::from_str(&response).unwrap();
    assert!(
        parsed.ok,
        "serves after replacing the stale file: {parsed:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}
