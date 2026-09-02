//! Onboarding backend (plan D1-D4): workspace settings persistence + tool
//! detection/install-command registry. `lib.rs`'s `#[tauri::command]`
//! wrappers are thin adapters over these; Rust tests target this module
//! only (mirrors `core.rs`'s convention).

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------
// Settings (plan D1/D2)
// ---------------------------------------------------------------------

/// Persisted app settings: `~/.linkly-crew/settings.json` (plan D1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    pub workspace_root: PathBuf,
}

impl Default for Settings {
    fn default() -> Self {
        Self { workspace_root: default_workspace_root() }
    }
}

fn default_workspace_root() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME must be set on this platform");
    Path::new(&home).join("linkly-crew").join("workspace")
}

/// App-default settings persistence path (plan D1): `~/.linkly-crew/settings.json`.
/// Never `/tmp`/`$TMPDIR`.
pub fn settings_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME must be set on this platform");
    Path::new(&home).join(".linkly-crew").join("settings.json")
}

/// Expands a leading `~` (or `~/...`) to `$HOME` (plan D2). Any other input
/// (including other tilde forms like `~user`) is returned unchanged — the
/// absolute-path check right after this is what actually rejects it.
fn expand_tilde(input: &str) -> PathBuf {
    if input == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = input.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    PathBuf::from(input)
}

/// Validates + normalizes a candidate `workspace_root` (plan D2): `~`
/// expansion happens first, the result must then be absolute, and a
/// missing directory is created (failure surfaces as an explicit `Err`,
/// never silently ignored).
fn validate_workspace_root(input: &str) -> Result<PathBuf, String> {
    let expanded = expand_tilde(input);
    if !expanded.is_absolute() {
        return Err(format!("workspace_root must be an absolute path (after ~ expansion), got \"{}\"", expanded.display()));
    }
    std::fs::create_dir_all(&expanded)
        .map_err(|e| format!("failed to create workspace_root \"{}\": {e}", expanded.display()))?;
    Ok(expanded)
}

/// Loads settings at `path` (path injected for testability, plan D1 — tests
/// never touch the real home directory). Missing or corrupted file falls
/// back to defaults (plan D1: never block onboarding on a damaged file).
fn load_settings(path: &Path) -> Settings {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
            tracing::warn!(error = %err, path = %path.display(), "settings.json corrupted; falling back to defaults");
            Settings::default()
        }),
        Err(_) => Settings::default(),
    }
}

/// Persists `settings` atomically at `path` via temp-file + rename (plan
/// D1), creating the parent directory if needed.
fn save_settings(path: &Path, settings: &Settings) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| "settings path has no parent directory".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    let tmp_path = parent.join(format!(".settings.json.tmp-{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp_path, &json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp_path, path).map_err(|e| e.to_string())
}

/// `get_settings` command body.
pub fn get_settings_core(path: &Path) -> Settings {
    load_settings(path)
}

/// `set_settings` command body: validates `workspace_root` (plan D2)
/// before persisting — an invalid path leaves the file untouched.
pub fn set_settings_core(path: &Path, workspace_root: &str) -> Result<Settings, String> {
    let validated_root = validate_workspace_root(workspace_root)?;
    let settings = Settings { workspace_root: validated_root };
    save_settings(path, &settings)?;
    Ok(settings)
}

// ---------------------------------------------------------------------
// Tool detection + install-command registry (plan D3/D4)
// ---------------------------------------------------------------------

/// `onboarding_status()` row per supported tool — field shape is the
/// `ToolStatus` contract stub in `apps/crew-app/src/lib/types.ts` verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolStatus {
    pub id: String,
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub install_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authenticated: Option<bool>,
}

struct RegistryEntry {
    id: &'static str,
    install_command: &'static str,
}

/// Static install-command registry (plan D4): each `install_command` is a
/// display/copy-only string for the tool's official distribution channel.
/// This registry intentionally does NOT execute or freshness-check these
/// commands — see plan D4 (advisory: an npm/brew package can rename or move
/// without this table noticing; that risk is accepted, not handled).
const REGISTRY: &[RegistryEntry] = &[
    // https://git-scm.com/downloads
    RegistryEntry { id: "git", install_command: "brew install git" },
    // https://cli.github.com
    RegistryEntry { id: "gh", install_command: "brew install gh" },
    // https://docs.claude.com/en/docs/claude-code
    RegistryEntry { id: "claude", install_command: "npm install -g @anthropic-ai/claude-code" },
    // https://github.com/openai/codex
    RegistryEntry { id: "codex", install_command: "npm install -g @openai/codex" },
    // https://opencode.ai
    RegistryEntry { id: "opencode", install_command: "npm install -g opencode-ai" },
    // https://github.com/google-gemini/gemini-cli
    RegistryEntry { id: "gemini", install_command: "npm install -g @google/gemini-cli" },
    // https://github.com/superagent-ai/grok-cli
    RegistryEntry { id: "grok", install_command: "npm install -g @vibe-kit/grok-cli" },
    // https://github.com/badlogic/pi-mono
    RegistryEntry { id: "pi", install_command: "npm install -g @mariozechner/pi" },
];

fn is_executable(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

fn scan_path_dirs(dirs: &[PathBuf], bin: &str) -> Option<PathBuf> {
    dirs.iter().map(|dir| dir.join(bin)).find(|p| is_executable(p))
}

/// Runs `program args...` with stdin closed, waits up to `timeout`, and
/// returns the trimmed first line of stdout on success. Any spawn failure,
/// non-zero exit, or timeout yields `None` — used for both `--version`
/// probes and the login-shell `command -v` fallback (plan D3), so one
/// missing/hanging tool never blocks the rest of the report.
async fn run_capture_first_line(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let child = cmd.spawn().ok()?;
    let output = tokio::time::timeout(timeout, child.wait_with_output()).await.ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).lines().next().map(|line| line.trim().to_string())
}

/// PATH-scans `path_env` for `id`'s binary; if not found and `shell` is
/// given, falls back to `$SHELL -lc 'command -v <id>'` (plan D3 — catches
/// tools only reachable through a login shell's rc-file PATH, e.g.
/// nvm-managed npm globals). `shell` is injected (rather than read from
/// `std::env::var_os` here) so tests can force the no-fallback path without
/// mutating process-global env state (unsound under concurrent test
/// threads — see `std::env::remove_var`'s docs).
async fn locate_binary(id: &str, path_env: &OsStr, shell: Option<&OsStr>) -> Option<PathBuf> {
    let dirs: Vec<PathBuf> = std::env::split_paths(path_env).collect();
    if let Some(found) = scan_path_dirs(&dirs, id) {
        return Some(found);
    }
    let shell = shell?.to_string_lossy();
    let line = run_capture_first_line(&shell, &["-lc", &format!("command -v {id}")], Duration::from_secs(1)).await?;
    let candidate = PathBuf::from(line);
    if candidate.is_absolute() && is_executable(&candidate) {
        Some(candidate)
    } else {
        None
    }
}

/// `gh auth status`'s exit code, `false` when `gh_path` is absent (never
/// installed => never authenticated) or the probe itself fails.
async fn gh_authenticated(gh_path: Option<&Path>) -> bool {
    let Some(bin) = gh_path else { return false };
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(["auth", "status"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    match tokio::time::timeout(Duration::from_secs(5), cmd.status()).await {
        Ok(Ok(status)) => status.success(),
        _ => false,
    }
}

/// Detects one registry entry by id (plan D3/D4), with the login-shell
/// fallback's `shell` explicitly injected for testability.
async fn detect_single_with_shell(id: &str, path_env: &OsStr, shell: Option<&OsStr>) -> ToolStatus {
    let entry = REGISTRY.iter().find(|e| e.id == id).unwrap_or_else(|| panic!("unknown registry id \"{id}\""));
    let found = locate_binary(entry.id, path_env, shell).await;
    let version = match &found {
        Some(bin) => run_capture_first_line(&bin.to_string_lossy(), &["--version"], Duration::from_secs(1)).await,
        None => None,
    };
    let authenticated = if entry.id == "gh" { Some(gh_authenticated(found.as_deref()).await) } else { None };
    ToolStatus {
        id: entry.id.to_string(),
        installed: found.is_some(),
        path: found.map(|p| p.to_string_lossy().into_owned()),
        version,
        install_command: entry.install_command.to_string(),
        authenticated,
    }
}

/// Detects one registry entry by id (plan D3/D4). `path_env` is injected
/// (test-friendly, mirrors `crew_harness::HarnessRegistry::detect_with_path`)
/// rather than reading the process environment directly; the login-shell
/// fallback reads the real `$SHELL`.
pub async fn detect_single(id: &str, path_env: &OsStr) -> ToolStatus {
    let shell = std::env::var_os("SHELL");
    detect_single_with_shell(id, path_env, shell.as_deref()).await
}

/// `onboarding_status` command body: one row per registry entry, in
/// registry order.
pub async fn onboarding_status_core(path_env: &OsStr) -> Vec<ToolStatus> {
    let mut results = Vec::with_capacity(REGISTRY.len());
    for entry in REGISTRY {
        results.push(detect_single(entry.id, path_env).await);
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A fresh directory under `target/` (never `/tmp`/`$TMPDIR` — workspace
    /// constraints), removed on drop. Mirrors `core.rs::TestDataRoot`.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("test-onboarding")
                .join(format!("{label}-{}-{unique}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // -- Settings: normal (round trip) --------------------------------

    #[test]
    fn settings_round_trip_save_then_load_preserves_workspace_root() {
        let dir = TestDir::new("roundtrip");
        let settings_file = dir.0.join("settings.json");
        let workspace = dir.0.join("my-workspace");

        let saved = set_settings_core(&settings_file, workspace.to_str().unwrap()).expect("valid absolute path must save");
        assert_eq!(saved.workspace_root, workspace);

        let loaded = get_settings_core(&settings_file);
        assert_eq!(loaded.workspace_root, workspace, "load after save must return exactly what was saved");
    }

    #[test]
    fn get_settings_on_missing_file_returns_the_default_workspace_root() {
        let dir = TestDir::new("missing");
        let settings_file = dir.0.join("does-not-exist.json");

        let loaded = get_settings_core(&settings_file);
        assert_eq!(loaded.workspace_root, default_workspace_root());
    }

    // -- Settings: error (corrupted file falls back) -------------------

    #[test]
    fn get_settings_on_corrupted_file_falls_back_to_defaults_instead_of_panicking() {
        let dir = TestDir::new("corrupt");
        let settings_file = dir.0.join("settings.json");
        std::fs::write(&settings_file, b"{ not valid json").unwrap();

        let loaded = get_settings_core(&settings_file);
        assert_eq!(loaded.workspace_root, default_workspace_root(), "corruption must fall back, never propagate as a panic");
    }

    // -- Settings: boundary (relative path rejected) --------------------

    #[test]
    fn set_settings_rejects_a_relative_workspace_root() {
        let dir = TestDir::new("relative");
        let settings_file = dir.0.join("settings.json");

        let err = set_settings_core(&settings_file, "relative/workspace").expect_err("relative path must be rejected");
        assert!(err.contains("absolute"), "error must explain the absoluteness requirement, got: {err}");
        assert!(!settings_file.exists(), "a rejected set_settings must not touch the settings file");
    }

    #[test]
    fn expand_tilde_resolves_leading_tilde_forms_to_home_without_touching_the_filesystem() {
        // Pure-function check (plan D2) — deliberately does not go through
        // `set_settings_core`, which would `create_dir_all` under the real
        // `$HOME`; this only exercises the string transform.
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~/foo/bar"), Path::new(&home).join("foo/bar"));
        assert_eq!(expand_tilde("~"), PathBuf::from(&home));
        assert_eq!(expand_tilde("/already/absolute"), PathBuf::from("/already/absolute"), "non-tilde input passes through unchanged");
    }

    // -- Registry: normal (all entries populated) ------------------------

    #[test]
    fn registry_lists_the_eight_contract_tools_with_a_nonempty_install_command_each() {
        let ids: Vec<&str> = REGISTRY.iter().map(|e| e.id).collect();
        assert_eq!(ids, vec!["git", "gh", "claude", "codex", "opencode", "gemini", "grok", "pi"]);
        for entry in REGISTRY {
            assert!(!entry.install_command.is_empty(), "{} must have a non-empty install_command", entry.id);
        }
    }

    // -- Detection: normal (real spot check) -----------------------------

    #[tokio::test]
    async fn detect_single_finds_the_real_git_on_the_actual_path() {
        // Real spot check (plan Task 02 Verify) — not #[ignore]'d: this
        // only reads PATH/spawns `git --version`, no network or mutation.
        let path_env = std::env::var_os("PATH").unwrap_or_default();
        let status = detect_single("git", &path_env).await;
        assert!(status.installed, "git must be installed on this dev machine");
        assert!(status.path.is_some());
        assert_eq!(status.authenticated, None, "authenticated is gh-only");
    }

    // -- Detection: error/boundary (mocked empty PATH => not installed) --

    #[tokio::test]
    async fn detect_single_reports_not_installed_on_an_empty_path_with_no_shell_fallback() {
        // `shell: None` (not real-env mutation, unsound under parallel test
        // threads — see `locate_binary`'s doc comment) deterministically
        // disables the login-shell fallback.
        let status = detect_single_with_shell("codex", OsStr::new(""), None).await;
        assert!(!status.installed);
        assert_eq!(status.path, None);
        assert_eq!(status.version, None);
        assert_eq!(status.install_command, "npm install -g @openai/codex");
    }

    #[tokio::test]
    async fn detect_single_reports_gh_authenticated_false_when_gh_is_not_installed() {
        let status = detect_single_with_shell("gh", OsStr::new(""), None).await;
        assert!(!status.installed);
        assert_eq!(status.authenticated, Some(false), "gh's authenticated must default to false, never panic, when gh is missing");
    }

    #[tokio::test]
    async fn onboarding_status_core_returns_all_eight_entries_in_registry_order() {
        let statuses = onboarding_status_core(OsStr::new("")).await;
        assert_eq!(statuses.len(), 8);
        let ids: Vec<&str> = statuses.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["git", "gh", "claude", "codex", "opencode", "gemini", "grok", "pi"]);
    }

    #[test]
    fn tool_status_serializes_without_authenticated_key_for_non_gh_tools() {
        let status = ToolStatus {
            id: "git".to_string(),
            installed: true,
            path: Some("/usr/bin/git".to_string()),
            version: Some("git version 2.0".to_string()),
            install_command: "brew install git".to_string(),
            authenticated: None,
        };
        let value = serde_json::to_value(&status).unwrap();
        assert!(value.get("authenticated").is_none(), "non-gh rows must omit authenticated, not serialize it as null");
    }
}
