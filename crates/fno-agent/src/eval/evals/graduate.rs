//! The graduate command: retag a capability task to regression, in place,
//! line-level so comments survive (the deleted Python leg's
//! report.py::graduate_task_file, ported). The goldens pin the bytes.
//! `--demote` adds the `demoted` tag with the same line-level rewrite.

use regex::Regex;

pub fn run(args: &[String]) -> i32 {
    let Some(task_id) = args.first().map(String::as_str) else {
        eprintln!("usage: fno-agent evals graduate <task-id> [--demote] [--bank <dir>]");
        return 2;
    };
    let bank_dir = super::resolve_bank_dir(super::flag_lookup(args, "--bank"));
    let tasks = match super::bank::discover_bank(&bank_dir) {
        Ok(tasks) => tasks,
        Err(e) => {
            eprintln!("Error: {e}");
            return 1;
        }
    };
    let Some(task) = tasks.iter().find(|t| t.id == task_id) else {
        let bank = bank_dir.display().to_string();
        eprintln!("Error: no bank task '{task_id}' in {bank}");
        return 1;
    };
    let demote = args.iter().any(|a| a == "--demote");
    // A generated pack-skill copy retags beside its canonical twin, or the
    // bundle freshness gate reads the tree out of sync (the 2026-11-17
    // graduation PR retagged only the copy and smoke-rest went red).
    let paths = match super::bank::bank_repo_root(&bank_dir) {
        Some(root) => write_set(&root, &task.source_path),
        None => vec![task.source_path.clone()],
    };
    if let Err(text) = rewrite_set(&paths, demote) {
        eprintln!("Error: {text}");
        return 2;
    }
    let sources = paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if demote {
        println!("demoted '{task_id}' ({sources})");
    } else {
        println!("graduated '{task_id}' -> regression ({sources})");
        println!("the daemon opens this PR after its weekly capability run; with the daemon off, commit the edit yourself");
    }
    0
}

/// Rewrite tier: capability to regression in place; a line-level rewrite so
/// comments and formatting survive. The Python regex at seed semantics:
/// ^(\s*tier:\s*)capability(\s*(?:#.*)?)$ with (?m).
pub fn graduate_task_file(task_path: &std::path::Path) -> Result<(), String> {
    let text = std::fs::read_to_string(task_path).map_err(|e| e.to_string())?;
    let new_text = graduate_text(&text).map_err(|e| format!("{}: {e}", task_path.display()))?;
    std::fs::write(task_path, new_text).map_err(|e| e.to_string())?;
    Ok(())
}

/// Rewrite every file or none: every text is read and retagged before the
/// first write lands, so a set with one unretaggable file changes nothing.
fn rewrite_set(paths: &[std::path::PathBuf], demote: bool) -> Result<(), super::bank::BankError> {
    let mut rewritten = Vec::with_capacity(paths.len());
    for path in paths {
        let text = std::fs::read_to_string(path)
            .map_err(|e| super::bank::BankError(format!("{}: {e}", path.display())))?;
        let retagged = if demote {
            demote_text(&text)
        } else {
            graduate_text(&text).map_err(super::bank::BankError)
        };
        let new_text =
            retagged.map_err(|e| super::bank::BankError(format!("{}: {e}", path.display())))?;
        rewritten.push((path.clone(), new_text));
    }
    for (path, new_text) in &rewritten {
        std::fs::write(path, new_text)
            .map_err(|e| super::bank::BankError(format!("{}: {e}", path.display())))?;
    }
    Ok(())
}

/// The same retag on the file's text, for a caller that writes elsewhere.
pub fn graduate_text(text: &str) -> Result<String, String> {
    let re =
        Regex::new(r"(?m)^([ \t]*tier:[ \t]*)capability([ \t]*(?:#.*)?)$").expect("static regex");
    if re.find_iter(text).count() == 0 {
        return Err("no `tier: capability` line to graduate (already regression, or non-standard formatting)".into());
    }
    Ok(re.replace_all(text, "${1}regression${2}").into_owned())
}

/// The files a retag writes, canonical source first, the given bank file
/// last. A task discovered under a generated pack-skill copy
/// (`skills/<sid>/...` bundled from `plugins/<pack>/<source>/...` by a
/// plugin.yaml manifest, the same rows parse-bundle-manifest.py emits)
/// resolves to its canonical twin, and both files come back so the copy
/// stays byte-identical to what `scripts/generate-skill-bundles.sh` would
/// produce from the retagged source. A file with no twin (a plain bank
/// task, or a copy whose canonical twin does not exist yet) retags alone.
pub fn write_set(
    repo_root: &std::path::Path,
    task_path: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    let Ok(rel) = task_path.strip_prefix(repo_root) else {
        return vec![task_path.to_path_buf()];
    };
    let mut comps = rel.components();
    let sid = match (comps.next(), comps.next()) {
        (Some(first), Some(second)) if first.as_os_str() == std::ffi::OsStr::new("skills") => {
            second.as_os_str().to_string_lossy().into_owned()
        }
        _ => return vec![task_path.to_path_buf()],
    };
    let rest = comps.as_path();
    let Ok(packs) = std::fs::read_dir(repo_root.join("plugins")) else {
        return vec![task_path.to_path_buf()];
    };
    let mut twins: Vec<std::path::PathBuf> = packs
        .flatten()
        .filter_map(|pack| {
            let text = std::fs::read_to_string(pack.path().join("plugin.yaml")).ok()?;
            let manifest: PackManifest = serde_yaml_ng::from_str(&text).ok()?;
            // One malformed skills entry skips that row, never the pack:
            // the generator's parser skips bad entries the same way, so
            // both resolvers agree on which copies exist.
            manifest
                .skills?
                .into_iter()
                .filter_map(|entry| {
                    let skill: PackSkill = serde_yaml_ng::from_value(entry).ok()?;
                    match (skill.id, skill.source) {
                        (Some(id), Some(source)) if id == sid => {
                            let twin = pack.path().join(source).join(rest);
                            twin.is_file().then_some(twin)
                        }
                        _ => None,
                    }
                })
                .next()
        })
        .collect();
    twins.sort();
    twins.push(task_path.to_path_buf());
    twins
}

#[derive(serde::Deserialize)]
struct PackManifest {
    skills: Option<Vec<serde_yaml_ng::Value>>,
}

// Both fields optional: an entry missing one skips that row instead of
// failing the whole list, matching parse-bundle-manifest.py.
#[derive(serde::Deserialize)]
struct PackSkill {
    id: Option<String>,
    source: Option<String>,
}

/// Append `demoted` to the flow-style `tags: [...]` line in place. A task
/// already demoted refuses by name and the file stays as it was.
pub fn demote_task_file(task_path: &std::path::Path) -> Result<(), String> {
    let text = std::fs::read_to_string(task_path).map_err(|e| e.to_string())?;
    let new_text = demote_text(&text).map_err(|e| e.to_string())?;
    std::fs::write(task_path, new_text).map_err(|e| e.to_string())?;
    Ok(())
}

/// The same tag append on the file's text, for a caller that writes
/// elsewhere.
fn demote_text(text: &str) -> Result<String, super::bank::BankError> {
    let re = Regex::new(r"(?m)^([ \t]*tags:[ \t]*\[)([^\]\n]*)(\][ \t]*(?:#.*)?)$")
        .expect("static regex");
    let Some(caps) = re.captures(text) else {
        return Err(super::bank::BankError(
            "no flow-style `tags: [...]` line to demote".to_string(),
        ));
    };
    let tags: Vec<&str> = caps[2]
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    if tags.contains(&super::bank::DEMOTED_TAG) {
        return Err(super::bank::BankError("already demoted".to_string()));
    }
    let mut kept = tags.clone();
    kept.push(super::bank::DEMOTED_TAG);
    let line = format!("{}{}{}", &caps[1], kept.join(", "), &caps[3]);
    let range = caps.get(0).expect("whole match").range();
    let mut new_text = text.to_owned();
    new_text.replace_range(range, &line);
    Ok(new_text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pack_copy_retags_beside_its_canonical_twin() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let source = root.join("plugins/acme/skills/packed/evals");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            root.join("plugins/acme/plugin.yaml"),
            "id: acme\nskills:\n  - id: packed\n    source: skills/packed\n",
        )
        .unwrap();
        let copy = root.join("skills/packed/evals");
        std::fs::create_dir_all(&copy).unwrap();
        let body = "id: t\ntier: capability\n";
        std::fs::write(source.join("t.yaml"), body).unwrap();
        std::fs::write(copy.join("t.yaml"), body).unwrap();
        let paths = write_set(root, &copy.join("t.yaml"));
        assert_eq!(paths, vec![source.join("t.yaml"), copy.join("t.yaml")]);
        for p in &paths {
            graduate_task_file(p).unwrap();
            assert!(
                std::fs::read_to_string(p)
                    .unwrap()
                    .contains("tier: regression"),
                "{}",
                p.display()
            );
        }
    }

    #[test]
    fn a_set_with_one_unretaggable_file_writes_none_of_them() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.yaml");
        let b = dir.path().join("b.yaml");
        std::fs::write(&a, "id: t\ntier: capability\n").unwrap();
        std::fs::write(&b, "id: t\ntier: regression\n").unwrap();
        let err = rewrite_set(&[a.clone(), b.clone()], false).unwrap_err();
        assert!(
            err.to_string().contains("no `tier: capability` line"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(&a).unwrap(),
            "id: t\ntier: capability\n"
        );
    }
}
