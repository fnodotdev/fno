//! The opencode pane arm's external-directory grant. The thread lane
//! pre-grants `external_directory` for the worktree and each state root
//! when it creates the serve session (`opencode_serve::session_rules`);
//! the pane TUI has no session-create call, so the same grant rides a
//! config file fno writes and `OPENCODE_CONFIG` names. Without it the TUI
//! asks `Access external directory <worktree>` and the worker idles until
//! a person answers (seen on the film HOME with
//! `spawn -H opencode --as pane`, node opencode-pane-worker-stops).
//!
//! `OPENCODE_CONFIG` is precedence slot 3 in opencode's merge chain
//! (global config, then it, then project configs): the grant is a floor a
//! user's own project rules can still tighten. Best-effort by
//! construction: a version that cannot be read or a write that fails
//! leaves no env pair, which is exactly the pre-fix behavior (the ask a
//! person can answer at the pane).

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::opencode_install::{classify_contract, OpencodeContract};
use crate::opencode_serve::external_directory_patterns;

/// The env pairs the opencode pane arm adds: `OPENCODE_CONFIG` naming the
/// written grant file. Empty when the grant cannot be earned (see the
/// module doc), so `make_plan`'s overlay composes unchanged either way.
///
/// Detection reads the binary the pane LAUNCHES: the arm's argv is the
/// contract's literal `opencode` (`interactive_create` tokens), and the
/// env wrapper forwards PATH, so the pane shell and this process resolve
/// the same binary. `opencode_install::opencode_bin` stays the INSTALL
/// lane's resolver; its `FNO_OPENCODE_BIN` override never shapes a pane
/// grant.
pub(crate) fn pane_grant_env(cwd: &Path) -> Vec<(String, String)> {
    pane_grant_env_with(cwd, "opencode")
}

/// The testable core: the same grant keyed to an explicit binary.
fn pane_grant_env_with(cwd: &Path, bin: &str) -> Vec<(String, String)> {
    let (contract, reported) = classify_contract(bin);
    if reported.is_none() {
        // A missing or silent binary proves no config shape; writing a
        // guessed shape would be a guess, not a grant.
        return Vec::new();
    }
    // One canonical cwd spelling for the file key AND the patterns: a
    // process's getcwd() resolves symlinks, so a raw-spelled pattern with a
    // symlink component would never match the paths opencode's tools
    // present, the same reason grant roots ride resolve_grant_root.
    let resolved = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let patterns =
        external_directory_patterns(&resolved, &crate::claude_ask::state_dirs_from_env());
    let doc = grant_document(contract, &patterns);
    let path = grant_path(&resolved);
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return Vec::new();
        }
    }
    if crate::state::write_json_atomic(&path, &doc).is_err() {
        return Vec::new();
    }
    vec![("OPENCODE_CONFIG".to_string(), path.display().to_string())]
}

/// The config document the TUI merges. 1.x reads the `permission` map,
/// measured live on 1.18.33: `OPENCODE_CONFIG=<file> opencode debug config`
/// resolves `permission.external_directory` with exactly these rules. 2.x
/// reads the `permissions` rule list, the document shape
/// `opencode_permission::user_rule_asks` flattens (`{action, resource,
/// effect}`).
fn grant_document(contract: OpencodeContract, patterns: &[String]) -> serde_json::Value {
    match contract {
        OpencodeContract::V1 => {
            let map: serde_json::Map<String, serde_json::Value> = patterns
                .iter()
                .map(|p| (p.clone(), json!("allow")))
                .collect();
            json!({
                "$schema": "https://opencode.ai/config.json",
                "permission": {"external_directory": map},
            })
        }
        OpencodeContract::V2 => json!({
            "$schema": "https://opencode.ai/config.json",
            "permissions": patterns
                .iter()
                .map(|p| json!({
                    "action": "external_directory",
                    "resource": p,
                    "effect": "allow",
                }))
                .collect::<Vec<_>>(),
        }),
    }
}

/// One grant file per canonical cwd, blake3-keyed the way the install
/// manifest keys a config dir (`opencode_install::manifest_path`), so
/// re-spawns of one worktree overwrite instead of accumulate. Lives under
/// `agents/` beside the turn specs: durable across pane restarts and
/// workspace restore, unlike a tmp file the first reboot would strand.
fn grant_path(cwd: &Path) -> PathBuf {
    let key = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let hash = blake3::hash(key.display().to_string().as_bytes()).to_hex();
    crate::paths::AgentsHome::from_env()
        .root()
        .join("agents")
        .join("opencode-pane-grants")
        .join(format!("opencode-pane-grant-{}.json", &hash[..12]))
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)] // asserts index fixed-size documents and the one-pair env vector
mod tests {
    use super::*;

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }
    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> EnvGuard {
            let old = std::env::var(key).ok();
            std::env::set_var(key, value);
            EnvGuard { key, old }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.old {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn fake_bin(dir: &Path, name: &str, version: &str) -> PathBuf {
        crate::write_exec_stub(dir, name, &format!("#!/bin/sh\necho {version}\n"))
    }

    #[test]
    fn the_v1_document_carries_the_map_the_live_binary_resolves() {
        let doc = grant_document(
            OpencodeContract::V1,
            &["/w/t".to_string(), "/w/t/*".to_string()],
        );
        let ext = doc["permission"]["external_directory"].clone();
        assert_eq!(ext["/w/t"], json!("allow"));
        assert_eq!(ext["/w/t/*"], json!("allow"));
        assert_eq!(doc["$schema"], json!("https://opencode.ai/config.json"));
    }

    #[test]
    fn the_v2_document_carries_the_rule_list_the_docs_flatten() {
        let doc = grant_document(
            OpencodeContract::V2,
            &["/w/t".to_string(), "/w/t/*".to_string()],
        );
        let list = doc["permissions"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        for rule in list {
            assert_eq!(rule["action"], json!("external_directory"));
            assert_eq!(rule["effect"], json!("allow"));
        }
        assert_eq!(list[0]["resource"], json!("/w/t"));
        assert_eq!(list[1]["resource"], json!("/w/t/*"));
    }

    #[test]
    fn a_readable_version_writes_the_grant_and_names_it_in_the_env() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("wt");
        std::fs::create_dir_all(&cwd).unwrap();
        let bin = fake_bin(tmp.path(), "oc-v1", "1.18.33");
        let dirs = tmp.path().join("state");
        std::fs::create_dir_all(&dirs).unwrap();
        let _g = EnvGuard::set("FNO_WORKER_ADD_DIRS", &dirs.display().to_string());
        // The grant file lands in the declared test home, never the
        // operator's ambient state root.
        let _h = EnvGuard::set(crate::paths::HOME_ENV, &tmp.path().display().to_string());
        let pairs = pane_grant_env_with(&cwd, &bin.display().to_string());
        assert_eq!(pairs.len(), 1, "{pairs:?}");
        assert_eq!(pairs[0].0, "OPENCODE_CONFIG");
        let written = std::fs::read_to_string(&pairs[0].1).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&written).unwrap();
        let ext = &doc["permission"]["external_directory"];
        let cwd_trim = std::fs::canonicalize(&cwd)
            .unwrap_or_else(|_| cwd.clone())
            .display()
            .to_string();
        let star = format!("{cwd_trim}/*");
        assert_eq!(ext[cwd_trim.as_str()], json!("allow"), "{doc}");
        assert_eq!(ext[star.as_str()], json!("allow"), "{doc}");
    }

    #[test]
    fn an_unreadable_binary_earns_no_grant() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("wt");
        std::fs::create_dir_all(&cwd).unwrap();
        let pairs = pane_grant_env_with(&cwd, "/nonexistent/opencode-missing");
        assert!(pairs.is_empty(), "{pairs:?}");
    }
}
