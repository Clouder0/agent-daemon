//! Agent registry: the map the dispatcher resolves `agent_id` against, and
//! the `agents.d/*.toml` persistence store (whitepaper §7, ADR-0004).
//!
//! One file per agent, named `{agent_id}.toml` (dot-separated ids make this
//! lossless and collision-free — ADR-0004). Writes are temp-file → fsync →
//! atomic rename, and in-memory state changes only after the file write
//! succeeds (persist-then-mutate).
//!
//! Validation is two-tier: *structural* issues (absolute handler path,
//! concurrency, id) are hard errors that reject the registration; *liveness*
//! issues (handler missing / not executable) are warnings only — a deleted
//! handler is a dispatch-time terminal error (whitepaper §8.6), not a reason
//! to stop the daemon.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::agent_id::AgentId;
use crate::error::AgentdError;

/// Default `max_concurrency`: one handler at a time (whitepaper §9.1).
pub const DEFAULT_CONCURRENCY: u32 = 1;

/// A registered agent (whitepaper §7.1). Mirrors the per-agent TOML file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub agent_id: AgentId,
    /// Absolute path to the handler executable (execve'd directly, never via
    /// a shell — whitepaper §7.2).
    pub handler: PathBuf,
    #[serde(default = "default_concurrency")]
    pub max_concurrency: u32,
    #[serde(default)]
    pub working_directory: Option<PathBuf>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

const fn default_concurrency() -> u32 {
    DEFAULT_CONCURRENCY
}

const fn default_true() -> bool {
    true
}

impl AgentConfig {
    /// Structural validation: absolute handler path, positive concurrency,
    /// absolute working directory. On error the config is rejected.
    pub fn validate(&self) -> Result<(), AgentdError> {
        if !self.handler.is_absolute() {
            return Err(AgentdError::config(format!(
                "agent {}: handler path must be absolute, got {:?}",
                self.agent_id, self.handler
            )));
        }
        if self.max_concurrency == 0 {
            return Err(AgentdError::config(format!(
                "agent {}: max_concurrency must be >= 1",
                self.agent_id
            )));
        }
        if let Some(cwd) = &self.working_directory
            && !cwd.is_absolute()
        {
            return Err(AgentdError::config(format!(
                "agent {}: working_directory must be absolute: {:?}",
                self.agent_id, cwd
            )));
        }
        Ok(())
    }

    /// Liveness issues are warnings, not errors: a missing or non-executable
    /// handler fails at dispatch (§8.6), not at load.
    pub fn liveness_warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.handler.exists() {
            out.push(format!("handler {:?} does not exist", self.handler));
        } else if !is_executable(&self.handler) {
            out.push(format!("handler {:?} is not executable", self.handler));
        }
        out
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

/// A registry reload, expressed as a diff the relay (#2) and dispatcher (#3)
/// subscribe to (whitepaper §7.4).
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Added(AgentConfig),
    Updated(AgentConfig),
    Enabled(AgentConfig),
    Disabled(AgentConfig),
    Removed(AgentId),
}

impl Change {
    /// The affected agent id (for the shared log span / field).
    pub fn agent_id(&self) -> AgentId {
        match self {
            Change::Added(c) | Change::Updated(c) | Change::Enabled(c) | Change::Disabled(c) => {
                c.agent_id.clone()
            }
            Change::Removed(id) => id.clone(),
        }
    }
}

/// The registry. Cheap reads (snapshot/get) under a read lock; mutations
/// persist to disk then update memory under a write lock.
pub struct Registry {
    dir: PathBuf,
    inner: RwLock<RegistryInner>,
}

struct RegistryInner {
    agents: HashMap<AgentId, AgentConfig>,
}

impl Registry {
    /// Load all `agents.d/*.toml` files. A missing directory is an empty
    /// registry; a malformed file, a content-id duplicate, or a filename that
    /// does not equal the content's agent_id is an error.
    pub fn load(dir: &Path) -> Result<Self, AgentdError> {
        let mut agents = HashMap::new();
        if !dir.exists() {
            return Ok(Self {
                dir: dir.to_owned(),
                inner: RwLock::new(RegistryInner { agents }),
            });
        }
        for entry in std::fs::read_dir(dir)
            .map_err(|e| AgentdError::config(format!("cannot read {}: {e}", dir.display())))?
        {
            let entry = entry.map_err(|e| AgentdError::config(format!("read_dir error: {e}")))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let config = read_agent_file(&path)?;
            let id = config.agent_id.clone();
            // Filename must equal the content's agent_id (ADR-0004).
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if stem != id.as_str() {
                return Err(AgentdError::config(format!(
                    "file {} declares agent_id {id} but is named {stem}.toml; \
                     filenames must match the agent_id (ADR-0004)",
                    path.display()
                )));
            }
            if agents.contains_key(&id) {
                return Err(AgentdError::config(format!(
                    "duplicate agent_id {id} in {}",
                    path.display()
                )));
            }
            config.validate()?;
            for w in config.liveness_warnings() {
                tracing::warn!(agent_id = %id, "{}", w);
            }
            agents.insert(id, config);
        }
        Ok(Self {
            dir: dir.to_owned(),
            inner: RwLock::new(RegistryInner { agents }),
        })
    }

    /// Read one agent's config by id.
    pub fn get(&self, id: &AgentId) -> Option<AgentConfig> {
        self.inner
            .read()
            .expect("registry lock poisoned")
            .agents
            .get(id)
            .cloned()
    }

    /// A sorted snapshot of all registered agents.
    pub fn snapshot(&self) -> Vec<AgentConfig> {
        let inner = self.inner.read().expect("registry lock poisoned");
        let mut v: Vec<_> = inner.agents.values().cloned().collect();
        v.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        v
    }

    /// Register a new agent: validate, persist (atomic write), then mutate
    /// in-memory state. Errors if the agent already exists.
    pub fn register(&self, config: &AgentConfig) -> Result<(), AgentdError> {
        config.validate()?;
        let mut inner = self.inner.write().expect("registry lock poisoned");
        if inner.agents.contains_key(&config.agent_id) {
            return Err(AgentdError::config(format!(
                "agent {} already registered",
                config.agent_id
            )));
        }
        self.persist(config)?; // on failure, in-memory state is unchanged
        inner.agents.insert(config.agent_id.clone(), config.clone());
        Ok(())
    }

    /// Replace an existing agent's config. Errors if absent.
    pub fn update(&self, config: &AgentConfig) -> Result<(), AgentdError> {
        config.validate()?;
        let mut inner = self.inner.write().expect("registry lock poisoned");
        if !inner.agents.contains_key(&config.agent_id) {
            return Err(AgentdError::config(format!(
                "agent {} is not registered",
                config.agent_id
            )));
        }
        self.persist(config)?;
        inner.agents.insert(config.agent_id.clone(), config.clone());
        Ok(())
    }

    /// Toggle `enabled`, persisting the change. A disabled agent stops
    /// pulling new events (§7.4); the file is kept.
    pub fn set_enabled(&self, id: &AgentId, enabled: bool) -> Result<(), AgentdError> {
        let mut inner = self.inner.write().expect("registry lock poisoned");
        let Some(existing) = inner.agents.get(id) else {
            return Err(AgentdError::config(format!("agent {id} is not registered")));
        };
        if existing.enabled == enabled {
            return Ok(());
        }
        let mut updated = existing.clone();
        updated.enabled = enabled;
        self.persist(&updated)?;
        inner.agents.insert(id.clone(), updated);
        Ok(())
    }

    /// Unregister an agent: delete its file, then remove in-memory. The file
    /// is the source of truth, so this is atomic-enough (missing file =
    /// unregistered after restart). On failure in-memory is unchanged.
    pub fn unregister(&self, id: &AgentId) -> Result<(), AgentdError> {
        let mut inner = self.inner.write().expect("registry lock poisoned");
        if !inner.agents.contains_key(id) {
            return Err(AgentdError::config(format!("agent {id} is not registered")));
        }
        let path = self.file_path(id);
        std::fs::remove_file(&path)
            .map_err(|e| AgentdError::config(format!("cannot remove {}: {e}", path.display())))?;
        inner.agents.remove(id);
        Ok(())
    }

    /// Re-read the directory and return the diff against current state
    /// (whitepaper §7.4). Consumers (#2/#3) subscribe to the changes.
    pub fn reload(&self) -> Result<Vec<Change>, AgentdError> {
        let fresh = Self::load(&self.dir)?;
        let mut old = self.inner.write().expect("registry lock poisoned");
        let new_map = fresh.inner.read().expect("poisoned").agents.clone();

        let mut changes = Vec::new();
        let ids: HashSet<&AgentId> = new_map.keys().chain(old.agents.keys()).collect();
        for id in ids {
            let was = old.agents.get(id);
            let is = new_map.get(id);
            match (was, is) {
                (None, Some(c)) => changes.push(Change::Added(c.clone())),
                (Some(_), None) => changes.push(Change::Removed(id.clone())),
                (Some(a), Some(b)) => {
                    if a.enabled != b.enabled {
                        changes.push(if b.enabled {
                            Change::Enabled(b.clone())
                        } else {
                            Change::Disabled(b.clone())
                        });
                    } else if a != b {
                        changes.push(Change::Updated(b.clone()));
                    }
                }
                (None, None) => unreachable!(),
            }
        }
        changes.sort_by_key(|c| c.agent_id());
        *old = RegistryInner { agents: new_map };
        Ok(changes)
    }

    /// Path for an agent's config file: `{dir}/{agent_id}.toml` (ADR-0004).
    fn file_path(&self, id: &AgentId) -> PathBuf {
        self.dir.join(format!("{}.toml", id))
    }

    /// Atomic write: temp file in the same dir → fsync → rename → best-effort
    /// dir fsync. On any failure in-memory state is unchanged (caller decides).
    fn persist(&self, config: &AgentConfig) -> Result<(), AgentdError> {
        if !self.dir.exists() {
            std::fs::create_dir_all(&self.dir).map_err(|e| {
                AgentdError::config(format!("cannot create {}: {e}", self.dir.display()))
            })?;
        }
        let content = toml::to_string_pretty(config).map_err(|e| {
            AgentdError::config(format!("cannot serialize {}: {e}", config.agent_id))
        })?;
        let final_path = self.file_path(&config.agent_id);
        let tmp_path = self
            .dir
            .join(format!(".{}.{}.tmp", config.agent_id, std::process::id()));
        std::fs::write(&tmp_path, content.as_bytes()).map_err(|e| {
            AgentdError::config(format!("cannot write {}: {e}", tmp_path.display()))
        })?;
        // fsync the file so the rename is durable.
        sync_file(&tmp_path)?;
        std::fs::rename(&tmp_path, &final_path).map_err(|e| {
            AgentdError::config(format!(
                "cannot rename {} -> {}: {e}",
                tmp_path.display(),
                final_path.display()
            ))
        })?;
        // Best-effort dir fsync (the rename may not be durable on crash, but
        // the config is small and reload-tolerant).
        if let Ok(dir) = std::fs::File::open(&self.dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

/// Open the file read-write (needed for `sync_all` on Linux) and fsync it.
fn sync_file(path: &Path) -> Result<(), AgentdError> {
    let file = std::fs::File::options()
        .read(true)
        .open(path)
        .map_err(|e| {
            AgentdError::config(format!("cannot open {} for fsync: {e}", path.display()))
        })?;
    file.sync_all()
        .map_err(|e| AgentdError::config(format!("fsync failed for {}: {e}", path.display())))
}

/// Parse and structurally validate a single `agents.d` file.
fn read_agent_file(path: &Path) -> Result<AgentConfig, AgentdError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentdError::config(format!("cannot read {}: {e}", path.display())))?;
    let config: AgentConfig = toml::from_str(&raw).map_err(|e| {
        AgentdError::config(format!("invalid agent config {}: {e}", path.display()))
    })?;
    config.validate()?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!(
            "agentd-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn cfg(id: &str, handler: &str, concurrency: u32) -> AgentConfig {
        AgentConfig {
            agent_id: AgentId::parse(id).unwrap(),
            handler: PathBuf::from(handler),
            max_concurrency: concurrency,
            working_directory: None,
            enabled: true,
        }
    }

    #[test]
    fn structural_validation_rejects_bad_configs() {
        assert!(cfg("a.b", "/abs/handler", 1).validate().is_ok());
        let relative_handler = cfg("a.b", "relative", 1);
        assert!(relative_handler.validate().is_err());
        let zero = AgentConfig {
            max_concurrency: 0,
            ..cfg("a.b", "/abs", 1)
        };
        assert!(zero.validate().is_err());
        let bad_cwd = AgentConfig {
            working_directory: Some(PathBuf::from("relative")),
            ..cfg("a.b", "/abs", 1)
        };
        assert!(bad_cwd.validate().is_err());
    }

    #[test]
    fn register_persists_and_reload_roundtrips() {
        let dir = temp_dir("roundtrip");
        let r = Registry::load(&dir).unwrap();
        r.register(&cfg("coding.main", "/bin/true", 1)).unwrap();
        let file = dir.join("coding.main.toml");
        assert!(file.exists());
        // A fresh load sees the same config.
        let r2 = Registry::load(&dir).unwrap();
        assert_eq!(r2.snapshot().len(), 1);
        assert_eq!(r2.snapshot()[0].agent_id.as_str(), "coding.main");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn duplicate_register_is_an_error() {
        let dir = temp_dir("dup");
        let r = Registry::load(&dir).unwrap();
        r.register(&cfg("a.b", "/bin/true", 1)).unwrap();
        assert!(r.register(&cfg("a.b", "/bin/false", 2)).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn duplicate_content_id_across_files_errors() {
        let dir = temp_dir("dupcontent");
        std::fs::create_dir_all(&dir).unwrap();
        // two files whose content claims the same id → load error
        std::fs::write(
            dir.join("x.toml"),
            toml_string(&cfg("dup.id", "/bin/true", 1)),
        )
        .unwrap();
        std::fs::write(
            dir.join("y.toml"),
            toml_string(&cfg("dup.id", "/bin/false", 1)),
        )
        .unwrap();
        assert!(Registry::load(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    fn toml_string(c: &AgentConfig) -> String {
        toml::to_string_pretty(c).unwrap()
    }

    #[test]
    fn filename_must_match_agent_id() {
        let dir = temp_dir("fname");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("wrong.toml"),
            toml_string(&cfg("a.b", "/bin/true", 1)),
        )
        .unwrap();
        assert!(Registry::load(&dir).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reload_diffs_transitions() {
        let dir = temp_dir("reload");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.toml"), toml_string(&cfg("a", "/bin/a", 1))).unwrap();
        std::fs::write(dir.join("b.toml"), toml_string(&cfg("b", "/bin/b", 1))).unwrap();

        let r = Registry::load(&dir).unwrap();
        assert_eq!(r.snapshot().len(), 2);

        // add c, remove a, update b's concurrency, disable c
        std::fs::write(dir.join("c.toml"), toml_string(&cfg("c", "/bin/c", 1))).unwrap();
        std::fs::remove_file(dir.join("a.toml")).unwrap();
        let b_disabled = AgentConfig {
            enabled: false,
            ..cfg("b", "/bin/b2", 1)
        };
        std::fs::write(dir.join("b.toml"), toml_string(&b_disabled)).unwrap();

        let changes = r.reload().unwrap();
        // sorting by id: a removed, b disabled+updated, c added
        let kinds: Vec<&str> = changes
            .iter()
            .map(|c| match c {
                Change::Added(_) => "added",
                Change::Updated(_) => "updated",
                Change::Enabled(_) => "enabled",
                Change::Disabled(_) => "disabled",
                Change::Removed(_) => "removed",
            })
            .collect();
        assert_eq!(kinds.join(","), "removed,disabled,added", "{changes:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn update_and_unregister_flow() {
        let dir = temp_dir("flow");
        let r = Registry::load(&dir).unwrap();
        r.register(&cfg("a.b", "/bin/a", 1)).unwrap();
        r.update(&cfg("a.b", "/bin/a2", 2)).unwrap();
        assert_eq!(r.snapshot()[0].handler, PathBuf::from("/bin/a2"));
        r.set_enabled(&AgentId::parse("a.b").unwrap(), false)
            .unwrap();
        assert!(!r.snapshot()[0].enabled);
        r.unregister(&AgentId::parse("a.b").unwrap()).unwrap();
        assert!(r.snapshot().is_empty());
        assert!(!dir.join("a.toml").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
