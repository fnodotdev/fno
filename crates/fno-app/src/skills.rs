//! The skills a composer completes before any session runs: read from the
//! picked harness's skill dirs on disk. Best-effort; a missing root reads
//! as none and never blocks a submit.

use std::path::{Path, PathBuf};

/// Where a skill came from; the popover groups rows by this, in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Source {
    Project,
    User,
    Plugin,
}

impl Source {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Source::Project => "project",
            Source::User => "user",
            Source::Plugin => "plugins",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Skill {
    pub(crate) source: Source,
    /// Insert text without the sigil: `name` or `plugin:name`.
    pub(crate) name: String,
    pub(crate) description: String,
}

/// The sigil a harness types before a skill name.
pub(crate) fn sigil(harness: &str) -> Option<char> {
    match harness {
        "claude" | "opencode" => Some('/'),
        "codex" => Some('$'),
        _ => None,
    }
}

/// Every skill the picked harness can run from `project`. Harnesses with
/// no known on-disk layout list none.
pub(crate) fn list(harness: &str, project: &str) -> Vec<Skill> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    list_in(harness, project, home.as_deref())
}

pub(crate) fn list_in(harness: &str, project: &str, home: Option<&Path>) -> Vec<Skill> {
    let mut out = Vec::new();
    match (harness, home) {
        ("claude", Some(home)) => {
            if !project.is_empty() {
                read_root(
                    &Path::new(project).join(".claude/skills"),
                    Source::Project,
                    "",
                    &mut out,
                );
            }
            read_root(&home.join(".claude/skills"), Source::User, "", &mut out);
            read_plugins(
                &home.join(".claude/plugins/installed_plugins.json"),
                &mut out,
            );
        }
        ("codex", Some(home)) => {
            read_root(&home.join(".codex/skills"), Source::User, "", &mut out);
        }
        _ => {}
    }
    out.sort_by(|a, b| (a.source, &a.name).cmp(&(b.source, &b.name)));
    out.dedup_by(|a, b| a.source == b.source && a.name == b.name);
    out
}

fn read_plugins(manifest: &Path, out: &mut Vec<Skill>) {
    let Ok(text) = std::fs::read_to_string(manifest) else {
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    let Some(plugins) = json.get("plugins").and_then(|p| p.as_object()) else {
        return;
    };
    for (key, installs) in plugins {
        let plugin = key.split('@').next().unwrap_or(key);
        let Some(path) = installs
            .as_array()
            .and_then(|a| a.last())
            .and_then(|i| i.get("installPath"))
            .and_then(|p| p.as_str())
        else {
            continue;
        };
        read_root(&Path::new(path).join("skills"), Source::Plugin, plugin, out);
    }
}

fn read_root(root: &Path, source: Source, plugin: &str, out: &mut Vec<Skill>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for e in entries.flatten() {
        let md = e.path().join("SKILL.md");
        let Ok(text) = std::fs::read_to_string(&md) else {
            continue;
        };
        let Some(dir) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let name = if plugin.is_empty() {
            dir
        } else {
            format!("{plugin}:{dir}")
        };
        out.push(Skill {
            source,
            name,
            description: description(&text),
        });
    }
}

/// The frontmatter `description:` as one line; empty when absent.
fn description(text: &str) -> String {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return String::new();
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some(v) = line.strip_prefix("description:") {
            return v.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        }
    }
    String::new()
}

/// Skills whose name contains the query (case-insensitive), prefix hits
/// first, then the grouped order `list` returns. At most `max` rows.
pub(crate) fn matching<'a>(skills: &'a [Skill], query: &str, max: usize) -> Vec<&'a Skill> {
    let q = query.to_lowercase();
    let mut hits: Vec<&Skill> = skills
        .iter()
        .filter(|s| s.name.to_lowercase().contains(&q))
        .collect();
    hits.sort_by_key(|s| (s.source, !s.name.to_lowercase().starts_with(&q)));
    hits.truncate(max);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);

    impl Tmp {
        fn new() -> Self {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let d = std::env::temp_dir().join(format!("fno-skills-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&d).unwrap();
            Tmp(d)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn skill(dir: &Path, name: &str, desc: &str) {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {desc}\n---\nbody\n"),
        )
        .unwrap();
    }

    #[test]
    fn claude_lists_project_user_and_plugin_skills_grouped() {
        let t = Tmp::new();
        let home = t.path().join("home");
        let proj = t.path().join("proj");
        skill(&home.join(".claude/skills"), "caveman", "talk short");
        skill(&proj.join(".claude/skills"), "deploy", "ship it");
        let plug = home.join("cache/fno");
        skill(&plug.join("skills"), "target", "run a node");
        std::fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        std::fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            format!(
                r#"{{"plugins":{{"fno@m":[{{"installPath":"{}"}}]}}}}"#,
                plug.display()
            ),
        )
        .unwrap();
        let got = list_in("claude", proj.to_str().unwrap(), Some(&home));
        let names: Vec<_> = got.iter().map(|s| (s.source, s.name.as_str())).collect();
        assert_eq!(
            names,
            vec![
                (Source::Project, "deploy"),
                (Source::User, "caveman"),
                (Source::Plugin, "fno:target"),
            ]
        );
        assert_eq!(got[2].description, "run a node");
    }

    #[test]
    fn codex_reads_its_own_root_and_unknown_harnesses_none() {
        let t = Tmp::new();
        skill(&t.path().join(".codex/skills"), "alpha", "a");
        skill(&t.path().join(".claude/skills"), "beta", "b");
        let got = list_in("codex", "", Some(t.path()));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "alpha");
        assert!(list_in("agy", "", Some(t.path())).is_empty());
        assert_eq!(sigil("codex"), Some('$'));
    }

    #[test]
    fn matching_puts_prefix_hits_first_within_a_group() {
        let mk = |n: &str| Skill {
            source: Source::User,
            name: n.into(),
            description: String::new(),
        };
        let all = vec![mk("my-fno"), mk("fno-doc")];
        let got: Vec<_> = matching(&all, "fno", 4)
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(got, vec!["fno-doc", "my-fno"]);
    }
}
