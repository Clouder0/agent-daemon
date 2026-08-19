//! End-to-end tests (whitepaper §21.2): the real `agentd` binary against a
//! real `nats-server` with JetStream, both spawned as child processes.
//!
//! Gated: runs only when `AGENTD_E2E=1` and a `nats-server` binary is on
//! PATH (CI installs it; locally `PATH=/tmp/nats-bin:$PATH`). Each test gets
//! its own server, ports, config, and handler scripts; tests run
//! sequentially (nextest default is parallel — the harness allocates unique
//! ports per test to tolerate it).
//!
//! Documented deviations (approved): exact ack-loss injection is folded into
//! the dedup cases; the crash-window test asserts the documented 1–2 run
//! range, not exactly 1.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use agent_daemon::control::{Request, Response};

const AGENTD_BIN: &str = env!("CARGO_BIN_EXE_agentd");

fn e2e_enabled() -> bool {
    std::env::var("AGENTD_E2E").ok().as_deref() == Some("1")
}

fn nats_server() -> Option<PathBuf> {
    if !e2e_enabled() {
        return None;
    }
    let bin = which_nats_server()?;
    Some(bin)
}

fn which_nats_server() -> Option<PathBuf> {
    let path = std::env::var("PATH").ok()?;
    std::env::split_paths(&path)
        .map(|p| p.join("nats-server"))
        .find(|p| p.is_file())
}

struct Server {
    child: Child,
    #[allow(dead_code)]
    store: PathBuf,
    port: u16,
}

impl Server {
    /// Wait until the server accepts TCP connections (spawn → listen gap).
    fn wait_listening(port: u16) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("nats-server did not listen on {port}");
    }

    fn start(dir: &Path, port: u16) -> Self {
        let store = dir.join("js");
        std::fs::create_dir_all(&store).unwrap();
        let bin = which_nats_server().expect("nats-server on PATH");
        let child = Command::new(bin)
            .arg("-js")
            .arg("-sd")
            .arg(&store)
            .arg("-p")
            .arg(port.to_string())
            .arg("-a")
            .arg("127.0.0.1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn nats-server");
        Self::wait_listening(port);
        Self { child, store, port }
    }

    fn url(&self) -> String {
        format!("nats://127.0.0.1:{}", self.port)
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.kill();
    }
}

struct Daemon {
    child: Child,
    dir: PathBuf,
    socket: PathBuf,
}

impl Daemon {
    async fn start(
        dir: &Path,
        port: u16,
        extra_config: &str,
        agents: &[(&str, &str, u32)],
    ) -> Self {
        let agents_dir = dir.join("agents.d");
        std::fs::create_dir_all(&agents_dir).unwrap();
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();

        for (id, handler, concurrency) in agents {
            let cfg = format!(
                "agent_id = \"{id}\"\nhandler = \"{handler}\"\nmax_concurrency = {concurrency}\nworking_directory = \"{}\"\nenabled = true\n",
                work.display()
            );
            std::fs::write(agents_dir.join(format!("{id}.toml")), cfg).unwrap();
        }

        let config = format!(
            "nats_url = \"nats://127.0.0.1:{port}\"\nstream_name = \"AGENT_EVENTS\"\nagents_dir = \"{}\"\ndedup_path = \"{}\"\ncontrol_socket = \"{}\"\n{extra_config}\n",
            agents_dir.display(),
            dir.join("dedup.db").display(),
            dir.join("control.sock").display()
        );
        std::fs::write(dir.join("agentd.toml"), config).unwrap();

        let child = Command::new(AGENTD_BIN)
            .arg("--config")
            .arg(dir.join("agentd.toml"))
            .arg("run")
            .stdout(std::fs::File::create(dir.join("daemon.log")).unwrap())
            .stderr(Stdio::null())
            .env("OUT", &work)
            .spawn()
            .expect("spawn agentd");
        let daemon = Self {
            child,
            dir: dir.to_path_buf(),
            socket: dir.join("control.sock"),
        };
        daemon.wait_ready().await;
        daemon
    }

    /// Wait until the control socket answers (daemon fully wired).
    async fn wait_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok(r) = self.rpc(Request::List).await
                && r.ok
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("agentd did not become ready; log: {}", self.log());
    }

    async fn rpc(&self, request: Request) -> Result<Response, String> {
        {
            let stream = UnixStream::connect(&self.socket)
                .await
                .map_err(|e| format!("connect: {e}"))?;
            let (reader, mut writer) = stream.into_split();
            let mut line = serde_json::to_string(&request).map_err(|e| e.to_string())?;
            line.push('\n');
            writer
                .write_all(line.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            writer.flush().await.map_err(|e| e.to_string())?;
            let mut lines = BufReader::new(reader).lines();
            let response = lines
                .next_line()
                .await
                .map_err(|e| e.to_string())?
                .ok_or("closed")?;
            serde_json::from_str(&response).map_err(|e| e.to_string())
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("daemon.log")).unwrap_or_default()
    }

    fn kill9(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn terminate(&mut self) -> std::process::ExitStatus {
        let pid = self.child.id();
        libc_kill(pid as i32, 15);
        self.child.wait().expect("wait agentd")
    }
}

// Minimal kill without a libc dep: use Command (`kill`) to avoid linking.
fn libc_kill(pid: i32, sig: i32) {
    let _ = Command::new("kill")
        .arg(format!("-{sig}"))
        .arg(pid.to_string())
        .status();
}

struct Env {
    dir: PathBuf,
    work: PathBuf,
    server: Server,
    daemon: Daemon,
}

impl Env {
    async fn new(tag: &str, agents: &[(&str, &str, u32)]) -> Self {
        Self::with_config(tag, agents, "").await
    }

    async fn with_config(tag: &str, agents: &[(&str, &str, u32)], extra: &str) -> Self {
        Self::numbered(tag, agents, extra, next_port()).await
    }

    async fn numbered(tag: &str, agents: &[(&str, &str, u32)], extra: &str, port: u16) -> Self {
        let mut dir = std::env::temp_dir();
        dir.push(format!("agentd-e2e-{tag}-{}-{port}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("work")).unwrap();
        let server = Server::start(&dir, port);
        // Operator-time step, as `agentdctl init` does in production: the
        // stream must exist before the daemon binds consumers.
        let client = async_nats::connect(server.url()).await.unwrap();
        let js = async_nats::jetstream::new(client);
        agent_daemon::relay::ensure_stream(&js, &agent_daemon::config::DaemonConfig::default())
            .await
            .unwrap();
        let daemon = Daemon::start(&dir, port, extra, agents).await;
        Self {
            work: dir.join("work"),
            dir,
            server,
            daemon,
        }
    }

    fn handler(dir: &Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.display().to_string()
    }

    async fn publish(&self, subject: &str, payload: &str) {
        let payload = payload.to_string();
        let client = async_nats::connect(self.server.url()).await.unwrap();
        client
            .publish(subject.to_string(), payload.into())
            .await
            .unwrap();
        client.flush().await.unwrap();
    }

    fn work(&self) -> &Path {
        &self.work
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        self.daemon.kill9();
        self.server.kill();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// Ask the OS for a free port (bind :0, read, drop). Tests run one Env
/// per process under nextest, so a static counter would collide.
fn next_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(4622)
}

/// Wait until `file` exists and its content contains `needle`; panic with
/// the daemon log on timeout.
fn await_file(env: &Env, name: &str, needle: &str, timeout: Duration) -> String {
    let path = env.work().join(name);
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(content) = std::fs::read_to_string(&path)
            && content.contains(needle)
        {
            return content;
        }
        if Instant::now() > deadline {
            panic!(
                "timed out waiting for {name} to contain {needle:?}\n--- daemon log ---\n{}",
                env.daemon.log()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn envelope(event_id: &str, agent: &str) -> String {
    format!(
        r#"{{"version":1,"event_id":"{event_id}","agent_id":"{agent}","type":"e2e.test","created_at":"2026-08-20T00:00:00Z","payload":{{}}}}"#
    )
}

// ---------------------------------------------------------------------------
// Delivery semantics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn offline_delivery() {
    let Some(_) = nats_server() else {
        eprintln!("skipping (no AGENTD_E2E/nats-server)");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-offline-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(&dir, "h", r#"cat > out.bin"#);
    let port = next_port();

    // Start the server first, then publish while the daemon is down.
    let mut pre_server = Server::start(&dir, port);
    let client = async_nats::connect(format!("nats://127.0.0.1:{port}"))
        .await
        .unwrap();
    let js = async_nats::jetstream::new(client.clone());
    agent_daemon::relay::ensure_stream(
        &js,
        &agent_daemon::config::DaemonConfig {
            stream_name: "AGENT_EVENTS".into(),
            max_event_bytes: 256 * 1024,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    client
        .publish(
            "agent.events.offline_agent".to_string(),
            envelope("off-1", "offline_agent").into(),
        )
        .await
        .unwrap();
    client.flush().await.unwrap();

    // Now start the daemon; the retained event must dispatch.
    let env = Env {
        dir: dir.clone(),
        work: dir.join("work"),
        server: std::mem::replace(&mut pre_server, Server::start(&dir, port)),
        daemon: Daemon::start(&dir, port, "", &[("offline_agent", &h, 1)]).await,
    };
    let content = await_file(&env, "out.bin", "off-1", Duration::from_secs(20));
    assert!(content.contains("off-1"));
}

#[tokio::test]
async fn relay_restart_recovers_within_bound() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-recon-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(&dir, "h", r#"cat > out.bin"#);
    let port = next_port();
    let mut env = Env::numbered("recon", &[("recon_agent", &h, 1)], "", port).await;

    // Kill and restart the server; publish after restart; dispatch must
    // resume within the FETCH_CAP bound (~10s) + slack.
    env.server.kill();
    std::thread::sleep(Duration::from_secs(1));
    env.server = Server::start(&env.dir, port);
    std::thread::sleep(Duration::from_secs(1));
    env.publish(
        "agent.events.recon_agent",
        &envelope("recon-1", "recon_agent"),
    )
    .await;
    await_file(&env, "out.bin", "recon-1", Duration::from_secs(25));
}

#[tokio::test]
async fn multi_agent_routing_no_crosstalk() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-multi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let ha = Env::handler(&dir, "ha", r#"cat > a.bin"#);
    let hb = Env::handler(&dir, "hb", r#"cat > b.bin"#);
    let env = Env::new("multi", &[("alpha_agent", &ha, 1), ("beta_agent", &hb, 1)]).await;
    env.publish("agent.events.alpha_agent", &envelope("m-a", "alpha_agent"))
        .await;
    env.publish("agent.events.beta_agent", &envelope("m-b", "beta_agent"))
        .await;
    let a = await_file(&env, "a.bin", "m-a", Duration::from_secs(20));
    let b = await_file(&env, "b.bin", "m-b", Duration::from_secs(20));
    assert!(!a.contains("m-b") && !b.contains("m-a"));
}

#[tokio::test]
async fn stdin_is_byte_exact() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-bytes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(&dir, "h", r#"cat > out.bin"#);
    let env = Env::new("bytes", &[("bytes_agent", &h, 1)]).await;
    let payload = envelope("bytes-1", "bytes_agent");
    env.publish("agent.events.bytes_agent", &payload).await;
    let got = await_file(&env, "out.bin", "bytes-1", Duration::from_secs(20));
    assert_eq!(got.trim(), payload, "stdin must be the original bytes");
}

// ---------------------------------------------------------------------------
// Ordering & concurrency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn serial_gate_no_overlap() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-serial-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(
        &dir,
        "h",
        r#"echo start >> marks; sleep 0.3; echo stop >> marks"#,
    );
    let env = Env::new("serial", &[("ser_agent", &h, 1)]).await;
    for i in 0..3 {
        env.publish(
            "agent.events.ser_agent",
            &envelope(&format!("s-{i}"), "ser_agent"),
        )
        .await;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let marks = std::fs::read_to_string(env.work().join("marks")).unwrap_or_default();
        if marks.lines().count() >= 6 {
            let seq: Vec<&str> = marks.lines().collect();
            assert_eq!(seq, &["start", "stop", "start", "stop", "start", "stop"]);
            return;
        }
        assert!(
            Instant::now() < deadline,
            "serial gate timeout\n{}",
            env.daemon.log()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[tokio::test]
async fn concurrent_gate_up_to_n() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-par-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(
        &dir,
        "h",
        r#"cat > /dev/null; sleep 0.5; echo done >> done"#,
    );
    let env = Env::new("par", &[("par_agent", &h, 3)]).await;
    let started = Instant::now();
    for i in 0..3 {
        env.publish(
            "agent.events.par_agent",
            &envelope(&format!("p-{i}"), "par_agent"),
        )
        .await;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let done = std::fs::read_to_string(env.work().join("done")).unwrap_or_default();
        if done.lines().count() >= 3 {
            break;
        }
        assert!(Instant::now() < deadline, "timeout\n{}", env.daemon.log());
        std::thread::sleep(Duration::from_millis(100));
    }
    let elapsed = started.elapsed();
    // Serial would be >= 1.5s (3 × 0.5s); concurrent lands near 0.5-0.8s.
    assert!(
        elapsed < Duration::from_millis(1300),
        "three 0.5s handlers should run concurrently, took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// Terminal / no-retry
// ---------------------------------------------------------------------------

#[tokio::test]
async fn nonzero_exit_acks_once() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-exit1-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(&dir, "h", r#"cat > /dev/null; echo ran >> count; exit 1"#);
    let env = Env::new("exit1", &[("exit_agent", &h, 1)]).await;
    env.publish("agent.events.exit_agent", &envelope("x-1", "exit_agent"))
        .await;
    let count = await_file(&env, "count", "ran", Duration::from_secs(20));
    assert_eq!(count.lines().count(), 1, "handler runs exactly once");
    // No retry: still one line after a grace period.
    std::thread::sleep(Duration::from_secs(3));
    let count = std::fs::read_to_string(env.work().join("count")).unwrap();
    assert_eq!(count.lines().count(), 1);
}

#[tokio::test]
async fn spawn_failure_is_terminal() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-spawn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let missing = dir.join("no-such-handler").display().to_string();
    let env = Env::new("spawn", &[("spawn_agent", &missing, 1)]).await;
    env.publish("agent.events.spawn_agent", &envelope("sp-1", "spawn_agent"))
        .await;
    std::thread::sleep(Duration::from_secs(3));
    let log = env.daemon.log();
    assert!(
        log.contains("handler spawn failed"),
        "spawn failure logged: {log}"
    );
}

#[tokio::test]
async fn invalid_envelope_is_terminal() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-invalid-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(&dir, "h", r#"echo ran >> count"#);
    let env = Env::new("invalid", &[("inv_agent", &h, 1)]).await;
    env.publish("agent.events.inv_agent", "{not json").await;
    env.publish(
        "agent.events.inv_agent",
        &envelope("iv", "inv_agent").replace("\"version\":1", "\"version\":2"),
    )
    .await;
    env.publish(
        "agent.events.inv_agent",
        &envelope("iv2", "other_agent"), // envelope agent mismatch
    )
    .await;
    std::thread::sleep(Duration::from_secs(4));
    let log = env.daemon.log();
    assert!(log.contains("invalid event"));
    assert!(
        !env.work().join("count").exists(),
        "handler must never run; log:\n{log}"
    );
}

#[tokio::test]
async fn oversize_event_is_terminal() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-oversize-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(&dir, "h", r#"cat > out.bin"#);
    let env = Env::with_config(
        "oversize",
        &[("big_agent", &h, 1)],
        "max_event_bytes = 128\n",
    )
    .await;
    let padding = "x".repeat(1024);
    let payload = format!(
        r#"{{"version":1,"event_id":"big-1","agent_id":"big_agent","type":"t","created_at":"t","payload":{{"b":"{padding}"}}}}"#
    );
    env.publish("agent.events.big_agent", &payload).await;
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        !env.work().join("out.bin").exists(),
        "oversize event must not dispatch"
    );
    assert!(env.daemon.log().contains("exceeds size limit"));
}

// ---------------------------------------------------------------------------
// Effectively-once (ADR-0001/0005)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn completed_dedup_across_restart_and_republish() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-dedup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(&dir, "h", r#"cat > /dev/null; echo ran >> count"#);
    let port = next_port();
    let mut env = Env::numbered("dedup", &[("dd_agent", &h, 1)], "", port).await;

    env.publish("agent.events.dd_agent", &envelope("dd-1", "dd_agent"))
        .await;
    await_file(&env, "count", "ran", Duration::from_secs(20));

    // Restart the daemon (same dedup db); republish the SAME event_id —
    // the dedup store must suppress a second handler run.
    env.daemon.kill9();
    env.daemon = Daemon::start(&env.dir, port, "", &[("dd_agent", &h, 1)]).await;
    env.publish("agent.events.dd_agent", &envelope("dd-1", "dd_agent"))
        .await;
    std::thread::sleep(Duration::from_secs(4));
    let count = std::fs::read_to_string(env.work().join("count")).unwrap();
    assert_eq!(count.lines().count(), 1, "dedup across restart + republish");
    assert!(env.daemon.log().contains("dedup hit"));
}

#[tokio::test]
async fn in_flight_redelivery_runs_handler_once() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-inflight-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(&dir, "h", r#"cat > /dev/null; sleep 2; echo ran >> count"#);
    // Concurrency > 1 leaves a free slot, so the duplicate copy is fetched
    // while the original handler runs — exercising the ADR-0001 in-flight
    // drop directly (a serial agent would just queue it behind the slot).
    let env = Env::new("inflight", &[("if_agent", &h, 4)]).await;
    let payload = envelope("if-1", "if_agent");
    let a = env.publish("agent.events.if_agent", &payload);
    let b = env.publish("agent.events.if_agent", &payload);
    tokio::join!(a, b);
    std::thread::sleep(Duration::from_secs(5));
    let count = std::fs::read_to_string(env.work().join("count")).unwrap_or_default();
    assert_eq!(
        count.lines().count(),
        1,
        "duplicate copy during flight suppressed: {count}"
    );
    let log = env.daemon.log();
    assert!(log.contains("in-flight duplicate dropped"), "log: {log}");
}

#[tokio::test]
async fn crash_window_allows_documented_duplicate() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-crash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(&dir, "h", r#"cat > /dev/null; sleep 2; echo ran >> count"#);
    let port = next_port();
    let mut env = Env::numbered("crash", &[("cr_agent", &h, 1)], "", port).await;

    env.publish("agent.events.cr_agent", &envelope("cr-1", "cr_agent"))
        .await;
    // Wait for the handler to start, then SIGKILL the daemon mid-handler.
    std::thread::sleep(Duration::from_millis(1500));
    env.daemon.kill9();

    // Restart; the event redelivers (crash window) and may run 1-2 times
    // total (§10.4). It must settle — no infinite loop.
    env.daemon = Daemon::start(&env.dir, port, "", &[("cr_agent", &h, 1)]).await;
    std::thread::sleep(Duration::from_secs(8));
    let count = std::fs::read_to_string(env.work().join("count")).unwrap_or_default();
    let runs = count.lines().count();
    assert!(
        (1..=3).contains(&runs),
        "documented duplicate window: expected 1-3 total runs (first attempt may or may not have completed), got {runs}"
    );
    // Settled: no further growth.
    std::thread::sleep(Duration::from_secs(3));
    let count2 = std::fs::read_to_string(env.work().join("count")).unwrap_or_default();
    assert_eq!(count2.lines().count(), runs, "no infinite redelivery");
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_register_dispatches_without_restart() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(&dir, "h", r#"cat > out.bin"#);
    let env = Env::new("live", &[]).await; // no agents at start
    let r = env
        .daemon
        .rpc(Request::Register {
            agent: agent_daemon::registry::AgentConfig {
                agent_id: agent_daemon::agent_id::AgentId::parse("live_agent").unwrap(),
                handler: h.into(),
                max_concurrency: 1,
                working_directory: Some(env.work().to_path_buf()),
                enabled: true,
            },
        })
        .await
        .unwrap();
    assert!(r.ok, "{r:?}");
    env.publish("agent.events.live_agent", &envelope("lv-1", "live_agent"))
        .await;
    await_file(&env, "out.bin", "lv-1", Duration::from_secs(20));
}

#[tokio::test]
async fn sighup_disable_stops_consuming_and_drains() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-hup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(
        &dir,
        "h",
        r#"cat > /dev/null; sleep 0.5; echo done >> done"#,
    );
    let env = Env::new("hup", &[("hup_agent", &h, 1)]).await;
    env.publish("agent.events.hup_agent", &envelope("hp-1", "hup_agent"))
        .await;
    await_file(&env, "done", "done", Duration::from_secs(20));

    // Disable via disk edit + SIGHUP (mirrors an operator flow).
    let cfg_path = env.dir.join("agents.d/hup_agent.toml");
    let cfg = std::fs::read_to_string(&cfg_path)
        .unwrap()
        .replace("enabled = true", "enabled = false");
    std::fs::write(&cfg_path, cfg).unwrap();
    libc_kill(env.daemon.child.id() as i32, 1); // SIGHUP
    std::thread::sleep(Duration::from_secs(2));

    env.publish("agent.events.hup_agent", &envelope("hp-2", "hup_agent"))
        .await;
    std::thread::sleep(Duration::from_secs(3));
    let done = std::fs::read_to_string(env.work().join("done")).unwrap();
    assert_eq!(done.lines().count(), 1, "disabled agent stops consuming");
}

#[tokio::test]
async fn sigterm_drains_inflight_and_exits_zero() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-term-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(
        &dir,
        "h",
        r#"cat > /dev/null; sleep 1.5; echo done >> done"#,
    );
    let mut env = Env::new("term", &[("tm_agent", &h, 1)]).await;
    env.publish("agent.events.tm_agent", &envelope("tm-1", "tm_agent"))
        .await;
    std::thread::sleep(Duration::from_millis(800)); // handler in flight
    let status = env.daemon.terminate();
    assert!(status.success(), "graceful exit code, got {status:?}");
    let done = std::fs::read_to_string(env.work().join("done")).unwrap_or_default();
    assert_eq!(
        done.lines().count(),
        1,
        "in-flight handler finished and recorded"
    );
}

// ---------------------------------------------------------------------------
// §5.4: the in-progress keepalive must actually prevent redelivery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn keepalive_prevents_redelivery() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-ka-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(&dir, "h", r#"cat > /dev/null; sleep 4; echo ran >> count"#);
    // AckWait 3s < handler 4s: only the keepalive (progress every 1s) keeps
    // the delivery lease alive. A free second slot means a redelivered copy
    // WOULD be pulled while the handler runs — and logged as an in-flight
    // drop — if the keepalive failed.
    let env = Env::with_config(
        "ka",
        &[("ka_agent", &h, 2)],
        "ack_wait_secs = 3\nack_progress_interval_secs = 1\n",
    )
    .await;
    env.publish("agent.events.ka_agent", &envelope("ka-1", "ka_agent"))
        .await;
    await_file(&env, "count", "ran", Duration::from_secs(20));
    // Margin beyond AckWait so a failing keepalive would have redelivered.
    std::thread::sleep(Duration::from_secs(2));
    let count = std::fs::read_to_string(env.work().join("count")).unwrap();
    assert_eq!(count.lines().count(), 1, "handler runs exactly once");
    let log = env.daemon.log();
    assert_eq!(
        log.matches("handler spawned").count(),
        1,
        "exactly one handler lifecycle: {log}"
    );
    assert!(
        !log.contains("in-flight duplicate dropped"),
        "keepalive failed: the server redelivered while the handler ran: {log}"
    );
}

// ---------------------------------------------------------------------------
// §21.3: running handlers survive a reload; future events use the new
// handler after an update
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reload_does_not_interrupt_inflight() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-hupif-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(&dir, "h", r#"cat > /dev/null; sleep 2; echo done >> done"#);
    let env = Env::new("hupif", &[("hif_agent", &h, 1)]).await;
    env.publish("agent.events.hif_agent", &envelope("hi-1", "hif_agent"))
        .await;
    // Handler now in flight (2s sleep). Change concurrency on disk + SIGHUP
    // so the reload does real work (dispatcher state recreated).
    std::thread::sleep(Duration::from_millis(800));
    let cfg_path = env.dir.join("agents.d/hif_agent.toml");
    let cfg = std::fs::read_to_string(&cfg_path)
        .unwrap()
        .replace("max_concurrency = 1", "max_concurrency = 2");
    std::fs::write(&cfg_path, cfg).unwrap();
    libc_kill(env.daemon.child.id() as i32, 1); // SIGHUP

    await_file(&env, "done", "done", Duration::from_secs(15));
    let done = std::fs::read_to_string(env.work().join("done")).unwrap();
    assert_eq!(
        done.lines().count(),
        1,
        "in-flight handler finished, not killed"
    );
    let log = env.daemon.log();
    assert!(log.contains("handler exited"), "{log}");
}

#[tokio::test]
async fn update_handler_future_events_use_new() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-updh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h1 = Env::handler(&dir, "h1", r#"cat > a.bin"#);
    let h2 = Env::handler(&dir, "h2", r#"cat > b.bin"#);
    let env = Env::new("updh", &[("uh_agent", &h1, 1)]).await;

    env.publish("agent.events.uh_agent", &envelope("uh-1", "uh_agent"))
        .await;
    await_file(&env, "a.bin", "uh-1", Duration::from_secs(20));

    // Swap the handler via the control socket.
    let r = env
        .daemon
        .rpc(Request::Update {
            agent: agent_daemon::registry::AgentConfig {
                agent_id: agent_daemon::agent_id::AgentId::parse("uh_agent").unwrap(),
                handler: h2.into(),
                max_concurrency: 1,
                working_directory: Some(env.work().to_path_buf()),
                enabled: true,
            },
        })
        .await
        .unwrap();
    assert!(r.ok, "{r:?}");

    env.publish("agent.events.uh_agent", &envelope("uh-2", "uh_agent"))
        .await;
    let b = await_file(&env, "b.bin", "uh-2", Duration::from_secs(20));
    let a = std::fs::read_to_string(env.work().join("a.bin")).unwrap();
    assert!(b.contains("uh-2") && !b.contains("uh-1"), "{b}");
    assert!(
        a.contains("uh-1") && !a.contains("uh-2"),
        "old handler saw the new event: {a}"
    );
}

// ---------------------------------------------------------------------------
// Operator path: agentdctl init against the live server
// ---------------------------------------------------------------------------

#[tokio::test]
async fn agentdctl_init_is_idempotent() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let _env = Env::new("init", &[]).await; // server up, daemon indifferent

    for _ in 0..2 {
        let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_agentdctl"))
            .arg("--config")
            .arg(_env.dir.join("agentd.toml"))
            .arg("init")
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("ready"), "{stdout}");
    }

    // The stream exists on the server.
    let client = async_nats::connect(_env.server.url()).await.unwrap();
    let js = async_nats::jetstream::new(client);
    js.get_stream("AGENT_EVENTS").await.expect("stream exists");
}

// ---------------------------------------------------------------------------
// §9.1 under load: delivery order holds for a serial agent at 20 events,
// and across a reload + live registration mid-stream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn order_preserved_under_load() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-ordr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let h = Env::handler(&dir, "h", r#"echo "$AGENTD_EVENT_ID" >> order"#);
    let env = Env::new("ordr", &[("ordr_agent", &h, 1)]).await;
    for i in 0..20 {
        env.publish(
            "agent.events.ordr_agent",
            &envelope(&format!("ol-{i:02}"), "ordr_agent"),
        )
        .await;
    }
    let order = await_file(&env, "order", "ol-19", Duration::from_secs(60));
    let got: Vec<&str> = order.lines().collect();
    let want: Vec<String> = (0..20).map(|i| format!("ol-{i:02}")).collect();
    assert_eq!(
        got,
        want.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        "exact publish order"
    );
}

#[tokio::test]
async fn mixed_load_reload_and_live_register() {
    let Some(_) = nats_server() else {
        eprintln!("skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("agentd-e2e-mix-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    let hm = Env::handler(&dir, "hm", r#"echo "$AGENTD_EVENT_ID" >> mx"#);
    let hn = Env::handler(&dir, "hn", r#"echo "$AGENTD_EVENT_ID" >> nx"#);
    let env = Env::new("mix", &[("mx_agent", &hm, 1)]).await;

    // First burst.
    for i in 0..5 {
        env.publish(
            "agent.events.mx_agent",
            &envelope(&format!("mx-{i:02}"), "mx_agent"),
        )
        .await;
    }
    std::thread::sleep(Duration::from_millis(300));

    // Mid-flight: real reload (concurrency bump) + live-register a second agent.
    let cfg_path = env.dir.join("agents.d/mx_agent.toml");
    let cfg = std::fs::read_to_string(&cfg_path)
        .unwrap()
        .replace("max_concurrency = 1", "max_concurrency = 2");
    std::fs::write(&cfg_path, cfg).unwrap();
    libc_kill(env.daemon.child.id() as i32, 1); // SIGHUP
    let r = env
        .daemon
        .rpc(Request::Register {
            agent: agent_daemon::registry::AgentConfig {
                agent_id: agent_daemon::agent_id::AgentId::parse("nx_agent").unwrap(),
                handler: hn.into(),
                max_concurrency: 1,
                working_directory: Some(env.work().to_path_buf()),
                enabled: true,
            },
        })
        .await
        .unwrap();
    assert!(r.ok, "{r:?}");
    env.publish("agent.events.nx_agent", &envelope("nx-00", "nx_agent"))
        .await;

    // Second burst for the first agent.
    for i in 5..10 {
        env.publish(
            "agent.events.mx_agent",
            &envelope(&format!("mx-{i:02}"), "mx_agent"),
        )
        .await;
    }

    let mx = await_file(&env, "mx", "mx-09", Duration::from_secs(60));
    let nx = await_file(&env, "nx", "nx-00", Duration::from_secs(30));
    let got: Vec<&str> = mx.lines().collect();
    let want: Vec<String> = (0..10).map(|i| format!("mx-{i:02}")).collect();
    assert_eq!(
        got,
        want.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        "order holds across reload + concurrent agent"
    );
    assert!(nx.contains("nx-00"), "{nx}");
}
