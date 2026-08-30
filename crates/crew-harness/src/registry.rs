//! Harness auto-detection registry (contracts-m5.md C4b, plan D2/D3).
//! Detection is pure Rust `PATH` scanning — no subprocess/`which` call —
//! so it stays deterministic and testable via [`HarnessRegistry::detect_with_path`].

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::claude::ClaudeCodeHarness;
use crate::opencode::OpencodeHarness;
use crate::pi::PiHarness;
use crate::Harness;

/// Every harness id the registry knows about, in contract order.
const KNOWN: &[&str] = &["claude-code", "codex", "gemini", "grok", "opencode", "ollama", "pi"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessInfo {
    pub id: String,
    pub installed: bool,
    pub path: Option<String>,
    pub adapter: AdapterStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterStatus {
    Real,
    Stub,
    None,
}

/// The single id -> [`AdapterStatus`] mapping shared by `detect` (which
/// reports it) and `make` (which acts on it) so the two can never disagree
/// (plan D3).
fn adapter_status_for(id: &str) -> AdapterStatus {
    match id {
        "claude-code" | "pi" => AdapterStatus::Real,
        "opencode" => AdapterStatus::Stub,
        _ => AdapterStatus::None,
    }
}

/// The `PATH` binary name for a harness id — only `claude-code` differs
/// from its id (the CLI binary is named `claude`).
fn binary_name(id: &str) -> &str {
    if id == "claude-code" {
        "claude"
    } else {
        id
    }
}

fn is_executable(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

pub struct HarnessRegistry;

impl HarnessRegistry {
    pub fn known() -> &'static [&'static str] {
        KNOWN
    }

    /// Scans the process's real `$PATH`. See [`Self::detect_with_path`] for
    /// the injectable, test-friendly variant.
    pub fn detect() -> Vec<HarnessInfo> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        Self::detect_with_path(&path)
    }

    /// Scans `path` (a `PATH`-style `:`-joined string) for each known
    /// harness's binary. Takes an injected value rather than reading the
    /// process environment so tests never mutate global state.
    pub fn detect_with_path(path: &OsStr) -> Vec<HarnessInfo> {
        let dirs: Vec<PathBuf> = std::env::split_paths(path).collect();
        KNOWN
            .iter()
            .map(|&id| {
                let bin = binary_name(id);
                let found = dirs.iter().map(|dir| dir.join(bin)).find(|p| is_executable(p));
                HarnessInfo {
                    id: id.to_string(),
                    installed: found.is_some(),
                    path: found.map(|p| p.to_string_lossy().into_owned()),
                    adapter: adapter_status_for(id),
                }
            })
            .collect()
    }

    /// Constructs the live adapter for `id`, or `None` if `id` has no real
    /// or stub implementation. `id == "pi"` is special-cased within the
    /// `Real` arm since two ids (`claude-code`, `pi`) now share that status
    /// but construct different adapters (plan D6).
    pub fn make(id: &str) -> Option<Arc<dyn Harness>> {
        match adapter_status_for(id) {
            AdapterStatus::Real if id == "pi" => Some(Arc::new(PiHarness::new()) as Arc<dyn Harness>),
            AdapterStatus::Real => Some(Arc::new(ClaudeCodeHarness::new()) as Arc<dyn Harness>),
            AdapterStatus::Stub => Some(Arc::new(OpencodeHarness::new()) as Arc<dyn Harness>),
            AdapterStatus::None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A worktree-local scratch directory (under `target/`, gitignored) for
    /// fake executables — never `/tmp` (plan D8, brief constraint).
    struct FakeBinDir {
        dir: PathBuf,
    }

    impl FakeBinDir {
        fn new() -> Self {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("test-bin-registry")
                .join(format!("{}-{}", std::process::id(), unique));
            fs::create_dir_all(&dir).expect("create fake bin dir");
            Self { dir }
        }

        /// Writes an empty file named `name`, `+x` if `executable`.
        fn write(&self, name: &str, executable: bool) -> PathBuf {
            let path = self.dir.join(name);
            fs::write(&path, b"#!/bin/sh\n").expect("write fake binary");
            let mode = if executable { 0o755 } else { 0o644 };
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))
                .expect("set fake binary permissions");
            path
        }

        fn path_os(&self) -> std::ffi::OsString {
            self.dir.as_os_str().to_owned()
        }
    }

    impl Drop for FakeBinDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn known_lists_exactly_the_seven_contract_ids() {
        // Was six (M5); M8 §F3 adds "pi" as a real adapter — not a
        // weakening, an accurate reflection of the new registered id.
        assert_eq!(
            HarnessRegistry::known(),
            &["claude-code", "codex", "gemini", "grok", "opencode", "ollama", "pi"]
        );
    }

    #[test]
    fn detect_reports_installed_true_when_binary_is_on_path() {
        let bin_dir = FakeBinDir::new();
        let claude_path = bin_dir.write("claude", true);

        let infos = HarnessRegistry::detect_with_path(&bin_dir.path_os());

        assert_eq!(infos.len(), 7);
        let claude_info = infos.iter().find(|i| i.id == "claude-code").unwrap();
        assert!(claude_info.installed);
        assert_eq!(claude_info.path.as_deref(), Some(claude_path.to_str().unwrap()));
        assert_eq!(claude_info.adapter, AdapterStatus::Real);
    }

    #[test]
    fn detect_reports_installed_false_for_every_id_on_empty_path() {
        let infos = HarnessRegistry::detect_with_path(OsStr::new(""));

        assert_eq!(infos.len(), 7);
        assert!(infos.iter().all(|i| !i.installed));
        assert!(infos.iter().all(|i| i.path.is_none()));
        // Boundary: known() ids are still all reported even with nothing found.
        let ids: Vec<&str> = infos.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, HarnessRegistry::known());
    }

    #[test]
    fn detect_treats_a_non_executable_file_as_not_installed() {
        let bin_dir = FakeBinDir::new();
        bin_dir.write("claude", false);

        let infos = HarnessRegistry::detect_with_path(&bin_dir.path_os());

        let claude_info = infos.iter().find(|i| i.id == "claude-code").unwrap();
        assert!(!claude_info.installed);
        assert!(claude_info.path.is_none());
    }

    #[test]
    fn make_maps_claude_code_and_pi_to_real_opencode_to_stub_and_others_to_none() {
        assert!(HarnessRegistry::make("claude-code").is_some());
        assert!(HarnessRegistry::make("pi").is_some());
        assert!(HarnessRegistry::make("opencode").is_some());
        for id in ["codex", "gemini", "grok", "ollama", "unknown-id"] {
            assert!(HarnessRegistry::make(id).is_none(), "id {id} should map to None");
        }
    }

    #[test]
    fn make_pi_constructs_a_distinct_adapter_from_claude_code_despite_both_being_real() {
        // Boundary: two ids share AdapterStatus::Real (D6) — make() must
        // still dispatch to the correct concrete adapter, not always the
        // first Real one (claude-code).
        let pi = HarnessRegistry::make("pi").expect("pi should be Real");
        assert_eq!(pi.id().0, "pi");
        let claude = HarnessRegistry::make("claude-code").expect("claude-code should be Real");
        assert_eq!(claude.id().0, "claude-code");
    }

    #[test]
    fn adapter_status_serializes_as_snake_case() {
        assert_eq!(serde_json::to_string(&AdapterStatus::Real).unwrap(), "\"real\"");
        assert_eq!(serde_json::to_string(&AdapterStatus::Stub).unwrap(), "\"stub\"");
        assert_eq!(serde_json::to_string(&AdapterStatus::None).unwrap(), "\"none\"");
    }
}
