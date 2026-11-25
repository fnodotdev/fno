//! The config project list the app composer picks from, and its one writer.
//!
//! A project is `{id, name, path}`. It lives in the config's
//! `work.workspaces.<group>.projects[]` rows, which carry `name` and `path`.
//! The id is the slug of the name, derived on read, so no row needs a new key.

use std::collections::BTreeMap;
use std::path::Path;

/// The group a composer add lands in. `fno config setup run` writes the same one.
const ADD_KEY: &str = "work.workspaces.main.projects";

/// One project row: the slug id, the display name, the absolute folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigProject {
    pub id: String,
    pub name: String,
    pub path: String,
}

/// The id a project name derives: lowercase ASCII letters and digits, any
/// other run collapsed to one `-`, no leading or trailing `-`.
pub fn project_id(name: &str) -> String {
    let mut id = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            id.push(c.to_ascii_lowercase());
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
    }
    id.trim_end_matches('-').to_string()
}

/// Every project in every group of the global config, in key order. A
/// missing or unreadable config reads as no projects: the composer then
/// shows its workspace folders alone.
pub fn config_projects() -> Vec<ConfigProject> {
    let global = crate::config_defaults::global_config_path();
    match crate::config_defaults::read_flat(&global) {
        Ok(Some(map)) => projects_from(&map),
        _ => Vec::new(),
    }
}

fn projects_from(map: &BTreeMap<String, toml::Value>) -> Vec<ConfigProject> {
    let mut out: Vec<ConfigProject> = Vec::new();
    for (key, value) in map {
        let is_projects = key
            .strip_prefix("work.workspaces.")
            .is_some_and(|rest| rest.ends_with(".projects"));
        let (true, toml::Value::Array(rows)) = (is_projects, value) else {
            continue;
        };
        for row in rows {
            let Some(path) = row.get("path").and_then(toml::Value::as_str) else {
                continue;
            };
            if path.is_empty() || out.iter().any(|p| p.path == path) {
                continue;
            }
            let name = row
                .get("name")
                .and_then(toml::Value::as_str)
                .filter(|n| !n.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| base_name(path));
            out.push(ConfigProject {
                id: project_id(&name),
                name,
                path: path.to_string(),
            });
        }
    }
    out
}

fn base_name(path: &str) -> String {
    Path::new(path.trim_end_matches('/'))
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Register `path` as a project. A path already listed returns its row and
/// writes nothing. The name defaults to the basename of the git toplevel
/// over `path`, else of `path` itself. The write goes through `fno config
/// set`, the validated writer, with every existing row carried verbatim.
pub fn add_project(path: &str) -> Result<ConfigProject, String> {
    let path = path.trim();
    if !Path::new(path).is_absolute() || !Path::new(path).is_dir() {
        return Err(format!("{path}: not an absolute folder"));
    }
    let global = crate::config_defaults::global_config_path();
    let flat = crate::config_defaults::read_flat(&global)?.unwrap_or_default();
    if let Some(known) = projects_from(&flat).into_iter().find(|p| p.path == path) {
        return Ok(known);
    }
    let mut rows: Vec<serde_json::Value> = match flat.get(ADD_KEY) {
        None => Vec::new(),
        Some(toml::Value::Array(items)) => items
            .iter()
            .filter_map(|v| serde_json::to_value(v).ok())
            .collect(),
        Some(_) => return Err(format!("{ADD_KEY} is hand-set to a non-array value")),
    };
    let name = base_name(&toplevel(path).unwrap_or_else(|| path.to_string()));
    rows.push(serde_json::json!({ "name": name, "path": path }));
    let value = serde_json::to_string(&rows).map_err(|e| e.to_string())?;
    let bin = std::env::var_os("FNO_BIN").unwrap_or_else(|| "fno".into());
    let out = std::process::Command::new(bin)
        .args(["config", "set"])
        .arg(format!("{ADD_KEY}={value}"))
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("fno config set: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "fno config set refused: {}",
            why.lines().next().unwrap_or("no reason given")
        ));
    }
    Ok(ConfigProject {
        id: project_id(&name),
        name,
        path: path.to_string(),
    })
}

fn toplevel(path: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C", path, "rev-parse", "--show-toplevel"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !top.is_empty()).then_some(top)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(text: &str) -> BTreeMap<String, toml::Value> {
        let table: toml::Table = toml::from_str(text).expect("fixture parses");
        let mut out = BTreeMap::new();
        for (group, entry) in table["work"]["workspaces"].as_table().expect("groups") {
            out.insert(
                format!("work.workspaces.{group}.projects"),
                entry["projects"].clone(),
            );
        }
        out
    }

    #[test]
    fn projects_read_across_groups_with_derived_ids() {
        let map = flat(
            "[work.workspaces.main]\nprojects = [{ name = \"Foot Note\", path = \"/a\" }, { name = \"x\" }]\n\
             [work.workspaces.me]\nprojects = [{ path = \"/b/.claude\" }, { name = \"dup\", path = \"/a\" }]\n",
        );
        let got = projects_from(&map);
        let seen: Vec<(&str, &str, &str)> = got
            .iter()
            .map(|p| (p.id.as_str(), p.name.as_str(), p.path.as_str()))
            .collect();
        assert_eq!(
            seen,
            [
                ("foot-note", "Foot Note", "/a"),
                ("claude", ".claude", "/b/.claude")
            ]
        );
    }

    #[test]
    fn add_refuses_a_relative_path() {
        let err = add_project("relative/dir").expect_err("relative path refuses");
        assert!(err.contains("not an absolute folder"), "{err}");
    }
}
