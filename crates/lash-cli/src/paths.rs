//! Filesystem layout conventions used by the lash CLI.
//!
//! These functions live in lash-cli rather than lash core: where a
//! user's config/cache lives is a CLI-application decision, not part
//! of the library surface. Core and provider crates accept paths explicitly
//! from the caller (via `FileModelCatalogStore::new`, host-prepared
//! `InputItem` references, etc.); lash-cli is the concrete host that wires
//! these values from the `~/.lash/` conventions below.

use std::path::PathBuf;

/// Root data directory for lash. `LASH_HOME` env var overrides;
/// otherwise `~/.lash/`.
pub fn lash_home() -> PathBuf {
    if let Ok(dir) = std::env::var("LASH_HOME") {
        PathBuf::from(dir)
    } else {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".lash")
    }
}

/// Cache directory. `$LASH_HOME/cache` when set, else `~/.cache/lash/`.
pub fn lash_cache_dir() -> PathBuf {
    if std::env::var("LASH_HOME").is_ok() {
        lash_home().join("cache")
    } else {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from(".cache"))
            .join("lash")
    }
}

/// Preferred repo-local directory for lash artifacts.
pub fn repo_local_lash_dir() -> PathBuf {
    PathBuf::from(".agents").join("lash")
}

/// Skill search directories, lowest to highest priority.
pub fn default_skill_dirs() -> Vec<PathBuf> {
    vec![
        lash_home().join("skills"),
        repo_local_lash_dir().join("skills"),
    ]
}

/// Path to the CLI's provider config JSON file.
pub fn config_file() -> PathBuf {
    lash_home().join("config.json")
}

/// Root of Lash-owned durable stores for this CLI installation.
pub fn store_dir() -> PathBuf {
    lash_home().join("store")
}

pub fn durable_core_db() -> PathBuf {
    store_dir().join("durable-core.db")
}

pub fn processes_db() -> PathBuf {
    store_dir().join("processes.db")
}

pub fn triggers_db() -> PathBuf {
    store_dir().join("triggers.db")
}

pub fn effects_db() -> PathBuf {
    store_dir().join("effects.db")
}

pub fn artifacts_db() -> PathBuf {
    store_dir().join("artifacts.db")
}

pub fn process_env_db() -> PathBuf {
    store_dir().join("process-env.db")
}

pub fn attachments_dir() -> PathBuf {
    store_dir().join("attachments")
}

/// Host-owned roster and UI sidecars keyed by Lash session id.
pub fn sessions_dir() -> PathBuf {
    lash_home().join("sessions")
}

/// Stable id for this CLI installation on this host, stored under `$LASH_HOME`.
pub fn host_id() -> std::io::Result<String> {
    let home = lash_home();
    std::fs::create_dir_all(&home)?;
    let path = home.join("host-id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim();
        if !existing.is_empty() {
            return Ok(existing.to_string());
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    std::fs::write(&path, format!("{id}\n"))?;
    Ok(id)
}

/// Path to the CLI's model catalog cache.
pub fn model_catalog_cache_file() -> PathBuf {
    lash_cache_dir().join("models.json")
}
