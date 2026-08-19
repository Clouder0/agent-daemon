//! Dispatcher integration tests: fake `Acker`, fake `DedupCheck`, real
//! shell-script handler executables — no NATS needed (whitepaper §8/§21).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use agent_daemon::agent_id::AgentId;
use agent_daemon::dedup::DedupStore;
use agent_daemon::dispatcher::{Acker, DedupCheck, Delivery, Dispatcher};
use agent_daemon::error::AgentdError;
use agent_daemon::registry::{AgentConfig, Registry};

const AGENT: &str = "t.agent";

// -- fakes -------------------------------------------------------------------

#[derive(Clone, Default)]
struct FakeAcker {
    acks: Arc<AtomicUsize>,
    terms: Arc<AtomicUsize>,
}

impl FakeAcker {
    fn acks(&self) -> usize {
        self.acks.load(Ordering::SeqCst)
    }

    fn terms(&self) -> usize {
        self.terms.load(Ordering::SeqCst)
    }
}

impl Acker for FakeAcker {
    async fn ack(&self) -> Result<(), String> {
        self.acks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn term(&self) -> Result<(), String> {
        self.terms.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Dedup fake with injectable read/mark failures (fail-open tests).
struct FakeDedup {
    store: DedupStore,
    fail_read: bool,
    fail_mark: bool,
    marks: Arc<AtomicUsize>,
}

impl FakeDedup {
    fn new() -> Self {
        Self {
            store: DedupStore::open_in_memory().unwrap(),
            fail_read: false,
            fail_mark: false,
            marks: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn marks(&self) -> usize {
        self.marks.load(Ordering::SeqCst)
    }
}

impl DedupCheck for FakeDedup {
    fn is_completed(&self, agent_id: &AgentId, event_id: &str) -> Result<bool, AgentdError> {
        if self.fail_read {
            return Err(AgentdError::DedupStore("injected read failure".into()));
        }
        self.store.is_completed(agent_id, event_id)
    }

    fn mark_completed(&self, agent_id: &AgentId, event_id: &str) -> Result<(), AgentdError> {
        if self.fail_mark {
            return Err(AgentdError::DedupStore("injected mark failure".into()));
        }
        self.marks.fetch_add(1, Ordering::SeqCst);
        self.store.mark_completed(agent_id, event_id)
    }
}

// -- helpers -------------------------------------------------------------------

fn temp_dir(tag: &str) -> PathBuf {
    let mut d = std::env::temp_dir();
    d.push(format!(
        "agentd-disp-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn handler(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

struct Fixture {
    dir: PathBuf,
    registry: Arc<Registry>,
    dedup: Arc<FakeDedup>,
    dispatcher: Arc<Dispatcher>,
    acker: FakeAcker,
}

impl Fixture {
    /// Serial agent (`max_concurrency = concurrency`) running `handler_body`
    /// with the workdir set to `<dir>/workdir` (so handler file output lands
    /// in a known place).
    fn new(tag: &str, handler_body: &str, concurrency: u32) -> Self {
        Self::with_dedup(tag, handler_body, concurrency, FakeDedup::new())
    }

    fn with_dedup(tag: &str, handler_body: &str, concurrency: u32, dedup: FakeDedup) -> Self {
        let dir = temp_dir(tag);
        let workdir = dir.join("workdir");
        std::fs::create_dir_all(&workdir).unwrap();
        let h = handler(&dir, "on-event", handler_body);
        let registry = Arc::new(Registry::load(&dir.join("agents.d")).unwrap());
        registry
            .register(&AgentConfig {
                agent_id: AgentId::parse(AGENT).unwrap(),
                handler: h,
                max_concurrency: concurrency,
                working_directory: Some(workdir),
                enabled: true,
            })
            .unwrap();
        let dedup = Arc::new(dedup);
        let dispatcher = Arc::new(Dispatcher::new(
            registry.clone(),
            dedup.clone(),
            Duration::from_secs(3600),
            256 * 1024,
        ));
        Self {
            dir,
            registry,
            dedup,
            dispatcher,
            acker: FakeAcker::default(),
        }
    }

    fn delivery(&self, event_id: &str) -> Delivery<FakeAcker> {
        delivery_for(&self.acker, event_id)
    }

    fn workdir(&self) -> PathBuf {
        self.dir.join("workdir")
    }

    fn cleanup(&self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn delivery_for(acker: &FakeAcker, event_id: &str) -> Delivery<FakeAcker> {
    Delivery {
        agent: AgentId::parse(AGENT).unwrap(),
        raw: envelope(event_id),
        stream_sequence: 7,
        consumer_sequence: 3,
        delivery_count: 1,
        acker: acker.clone(),
    }
}

fn envelope(event_id: &str) -> Vec<u8> {
    format!(
        r#"{{"version":1,"event_id":"{event_id}","agent_id":"{AGENT}","type":"im.message","created_at":"2026-08-20T00:00:00Z","payload":{{"n":1}}}}"#
    )
    .into_bytes()
}

// -- tests ---------------------------------------------------------------------

#[tokio::test]
async fn stdin_receives_exact_original_bytes() {
    let f = Fixture::new("stdin", r#"cat > out.bin"#, 1);
    let raw = envelope("e-stdin");
    let mut d = f.delivery("e-stdin");
    d.raw = raw.clone();
    f.dispatcher.dispatch(d).await;

    assert_eq!(f.acker.acks(), 1);
    assert_eq!(f.acker.terms(), 0);
    assert_eq!(
        std::fs::read(f.workdir().join("out.bin")).unwrap(),
        raw,
        "handler stdin must be the original bytes (§8.2)"
    );
    f.cleanup();
}

#[tokio::test]
async fn env_vars_exposed_to_handler() {
    let f = Fixture::new("env", r#"env | grep '^AGENTD_' | sort > env.txt"#, 1);
    f.dispatcher.dispatch(f.delivery("e-env")).await;

    let env = std::fs::read_to_string(f.workdir().join("env.txt")).unwrap();
    for expected in [
        "AGENTD_AGENT_ID=t.agent",
        "AGENTD_EVENT_ID=e-env",
        "AGENTD_EVENT_TYPE=im.message",
        "AGENTD_STREAM_SEQUENCE=7",
        "AGENTD_CONSUMER_SEQUENCE=3",
        "AGENTD_DELIVERY_COUNT=1",
    ] {
        assert!(env.contains(expected), "missing {expected} in:\n{env}");
    }
    f.cleanup();
}

#[tokio::test]
async fn nonzero_exit_acks_once_no_retry() {
    let f = Fixture::new(
        "exit1",
        r#"cat > /dev/null; echo ran >> count.txt; exit 1"#,
        1,
    );
    f.dispatcher.dispatch(f.delivery("e-exit1")).await;

    assert_eq!(f.acker.acks(), 1, "exit 1 still acks (§8.5)");
    assert_eq!(f.acker.terms(), 0);
    assert_eq!(
        std::fs::read_to_string(f.workdir().join("count.txt")).unwrap(),
        "ran\n",
        "handler must run exactly once"
    );
    f.cleanup();
}

#[tokio::test]
async fn spawn_failure_is_terminal() {
    let dir = temp_dir("spawnfail");
    let registry = Arc::new(Registry::load(&dir.join("agents.d")).unwrap());
    registry
        .register(&AgentConfig {
            agent_id: AgentId::parse(AGENT).unwrap(),
            handler: dir.join("does-not-exist"),
            max_concurrency: 1,
            working_directory: None,
            enabled: true,
        })
        .unwrap();
    let d = Dispatcher::new(
        registry,
        Arc::new(FakeDedup::new()),
        Duration::from_secs(3600),
        256 * 1024,
    );
    let acker = FakeAcker::default();
    let del = Delivery {
        agent: AgentId::parse(AGENT).unwrap(),
        raw: envelope("e-spawn"),
        stream_sequence: 1,
        consumer_sequence: 1,
        delivery_count: 1,
        acker: acker.clone(),
    };
    d.dispatch(del).await;

    assert_eq!(acker.terms(), 1, "spawn failure → term (§8.6)");
    assert_eq!(acker.acks(), 0);
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn invalid_envelope_is_terminal() {
    let f = Fixture::new("invalid", r#"echo ran >> count.txt"#, 1);
    // Unparseable JSON.
    let mut bad = f.delivery("x");
    bad.raw = b"{not json".to_vec();
    f.dispatcher.dispatch(bad).await;
    // Unsupported version.
    let mut v2 = f.delivery("x");
    v2.raw = String::from_utf8(envelope("e-v2").clone())
        .unwrap()
        .replace("\"version\":1", "\"version\":2")
        .into_bytes();
    f.dispatcher.dispatch(v2).await;
    // Envelope agent_id mismatching the consumer's agent.
    let mut wrong = f.delivery("x");
    wrong.raw = String::from_utf8(envelope("e-x").clone())
        .unwrap()
        .replace("\"agent_id\":\"t.agent\"", "\"agent_id\":\"other.agent\"")
        .into_bytes();
    f.dispatcher.dispatch(wrong).await;

    assert_eq!(f.acker.terms(), 3, "all invalid forms → term");
    assert_eq!(f.acker.acks(), 0);
    assert!(
        !f.workdir().join("count.txt").exists(),
        "handler must never run"
    );
    f.cleanup();
}

#[tokio::test]
async fn oversize_event_is_terminal() {
    let dir = temp_dir("oversize");
    let registry = Arc::new(Registry::load(&dir.join("agents.d")).unwrap());
    registry
        .register(&AgentConfig {
            agent_id: AgentId::parse(AGENT).unwrap(),
            handler: handler(&dir, "h", "true"),
            max_concurrency: 1,
            working_directory: None,
            enabled: true,
        })
        .unwrap();
    let d = Dispatcher::new(
        registry,
        Arc::new(FakeDedup::new()),
        Duration::from_secs(3600),
        64, // tiny cap for the test
    );
    let acker = FakeAcker::default();
    d.dispatch(Delivery {
        agent: AgentId::parse(AGENT).unwrap(),
        raw: envelope("e-big"),
        stream_sequence: 1,
        consumer_sequence: 1,
        delivery_count: 1,
        acker: acker.clone(),
    })
    .await;

    assert_eq!(acker.terms(), 1, "oversize → term (§15.2)");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn dedup_hit_skips_handler_and_acks() {
    let dedup = FakeDedup::new();
    let agent = AgentId::parse(AGENT).unwrap();
    DedupCheck::mark_completed(&dedup, &agent, "e-dup").unwrap();
    let baseline = dedup.marks(); // 1: the pre-mark itself
    let f = Fixture::with_dedup(
        "deduphit",
        r#"cat > /dev/null; echo ran >> count.txt"#,
        1,
        dedup,
    );

    f.dispatcher.dispatch(f.delivery("e-dup")).await;

    assert_eq!(
        f.acker.acks(),
        1,
        "redelivery of completed event acks directly"
    );
    assert_eq!(f.acker.terms(), 0);
    assert!(
        !f.workdir().join("count.txt").exists(),
        "handler must be skipped on dedup hit"
    );
    assert_eq!(f.dedup.marks(), baseline, "no re-mark on dedup hit");
    f.cleanup();
}

#[tokio::test]
async fn in_flight_duplicate_dropped_without_ack() {
    let f = Fixture::new(
        "inflight",
        r#"cat > /dev/null; sleep 0.3; echo ran >> count.txt"#,
        4, // concurrency > 1 so the duplicate could otherwise spawn
    );

    // Two copies of the same event dispatched concurrently; the second
    // arrives while the first's handler is still running (ADR-0001 path).
    let a = f.dispatcher.dispatch(f.delivery("e-dup-inflight"));
    let b = f.dispatcher.dispatch(f.delivery("e-dup-inflight"));
    let (a, b) = tokio::join!(a, b);
    let _ = (a, b);

    assert_eq!(f.acker.acks(), 1, "exactly one copy completes and acks");
    assert_eq!(f.acker.terms(), 0);
    assert_eq!(
        std::fs::read_to_string(f.workdir().join("count.txt")).unwrap(),
        "ran\n",
        "handler runs exactly once"
    );
    f.cleanup();
}

#[tokio::test]
async fn serial_gate_never_overlaps() {
    let f = Fixture::new(
        "serial",
        r#"echo start >> marks.txt; sleep 0.15; echo stop >> marks.txt"#,
        1,
    );
    let started = Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..3 {
        let disp = f.dispatcher.clone();
        let del = f.delivery(&format!("e-ser-{i}"));
        tasks.spawn(async move {
            disp.dispatch(del).await;
        });
    }
    while tasks.join_next().await.is_some() {}
    let elapsed = started.elapsed();

    assert!(
        elapsed >= Duration::from_millis(420),
        "three serial 0.15s handlers need >= 0.45s, took {elapsed:?}"
    );
    let marks = std::fs::read_to_string(f.workdir().join("marks.txt")).unwrap();
    let seq: Vec<&str> = marks.lines().collect();
    assert_eq!(
        seq,
        &["start", "stop", "start", "stop", "start", "stop"],
        "handlers must not interleave: {seq:?}"
    );
    assert_eq!(f.acker.acks(), 3);
    f.cleanup();
}

#[tokio::test]
async fn parallel_gate_runs_concurrently() {
    let f = Fixture::new("parallel", r#"cat > /dev/null; sleep 0.3"#, 3);
    let started = Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..3 {
        let disp = f.dispatcher.clone();
        let del = f.delivery(&format!("e-par-{i}"));
        tasks.spawn(async move {
            disp.dispatch(del).await;
        });
    }
    while tasks.join_next().await.is_some() {}
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_millis(850),
        "three concurrent 0.3s handlers should finish in ~0.3s, took {elapsed:?}"
    );
    assert_eq!(f.acker.acks(), 3);
    f.cleanup();
}

#[tokio::test]
async fn epipe_from_early_exit_is_normal() {
    // Handler exits without reading a stdin payload well over the 64 KiB
    // pipe buffer; the writer gets EPIPE and dispatch still completes
    // (§8.1 step 9).
    let f = Fixture::new("epipe", r#"exit 0"#, 1);
    let padding = "x".repeat(128 * 1024);
    let raw = format!(
        r#"{{"version":1,"event_id":"e-epipe","agent_id":"{AGENT}","type":"im.message","created_at":"t","payload":{{"blob":"{padding}"}}}}"#
    )
    .into_bytes();
    let mut d = f.delivery("e-epipe");
    d.raw = raw;
    f.dispatcher.dispatch(d).await;

    assert_eq!(f.acker.acks(), 1, "EPIPE must not fail dispatch");
    f.cleanup();
}

#[tokio::test]
async fn fail_open_dispatches_when_dedup_read_fails() {
    let mut dedup = FakeDedup::new();
    dedup.fail_read = true;
    let f = Fixture::with_dedup(
        "failread",
        r#"cat > /dev/null; echo ran >> count.txt"#,
        1,
        dedup,
    );

    f.dispatcher.dispatch(f.delivery("e-fo-r")).await;

    assert_eq!(f.acker.acks(), 1, "fail-open: dispatch anyway, still ack");
    assert!(
        f.workdir().join("count.txt").exists(),
        "handler must run despite the store error"
    );
    f.cleanup();
}

#[tokio::test]
async fn fail_open_acks_when_dedup_mark_fails() {
    let mut dedup = FakeDedup::new();
    dedup.fail_mark = true;
    let f = Fixture::with_dedup("failmark", r#"cat > /dev/null"#, 1, dedup);

    f.dispatcher.dispatch(f.delivery("e-fo-m")).await;

    assert_eq!(
        f.acker.acks(),
        1,
        "handler ran; ack anyway (no redelivery loop)"
    );
    assert_eq!(f.dedup.marks(), 0);
    f.cleanup();
}

#[tokio::test]
async fn unregistered_and_disabled_agents_term() {
    let f = Fixture::new("unreg", r#"true"#, 1);
    // Unregistered agent id.
    let acker = FakeAcker::default();
    f.dispatcher
        .dispatch(Delivery {
            agent: AgentId::parse("ghost.agent").unwrap(),
            raw: envelope("e-ghost"),
            stream_sequence: 1,
            consumer_sequence: 1,
            delivery_count: 1,
            acker: acker.clone(),
        })
        .await;
    assert_eq!(acker.terms(), 1, "unregistered agent → term");

    // Disabled agent.
    let id = AgentId::parse(AGENT).unwrap();
    f.registry.set_enabled(&id, false).unwrap();
    f.dispatcher.dispatch(f.delivery("e-disabled")).await;
    assert_eq!(f.acker.terms(), 1, "disabled agent → term");
    f.cleanup();
}

#[tokio::test]
async fn available_reports_free_slots() {
    let f = Fixture::new("avail", r#"true"#, 2);
    let id = AgentId::parse(AGENT).unwrap();
    assert_eq!(f.dispatcher.available(&id), 2);
    assert_eq!(
        f.dispatcher
            .available(&AgentId::parse("ghost.agent").unwrap()),
        0
    );
    f.registry.set_enabled(&id, false).unwrap();
    f.dispatcher.apply_changes(&[]);
    assert_eq!(f.dispatcher.available(&id), 0, "disabled → 0 slots");
    f.cleanup();
}

#[tokio::test]
async fn mark_completed_written_before_ack() {
    // After dispatch, the (agent, event) pair must be recorded — the §10.5
    // ordering (mark before ack) is what makes redelivery-after-ack-loss
    // safe; here we verify the mark exists once dispatch resolves.
    let f = Fixture::new("mark", r#"cat > /dev/null"#, 1);
    f.dispatcher.dispatch(f.delivery("e-marked")).await;
    assert_eq!(f.dedup.marks(), 1);
    // And the second delivery of the same event dedups.
    f.dispatcher.dispatch(f.delivery("e-marked")).await;
    assert_eq!(f.dedup.marks(), 1, "second delivery hits dedup, no re-mark");
    assert_eq!(f.acker.acks(), 2);
    f.cleanup();
}
