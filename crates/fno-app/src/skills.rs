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
        let Some(text) = read_head(&md) else {
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

/// The first 4 KiB of a SKILL.md: the frontmatter is all the popover reads.
fn read_head(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(4096)
        .read_to_end(&mut buf)
        .ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// The frontmatter `description:` as one line; empty when absent. A YAML
/// block scalar (`>` or `|`) reads as its first indented line.
fn description(text: &str) -> String {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return String::new();
    }
    let mut found = false;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if found {
            let t = line.trim();
            if !t.is_empty() {
                return t.to_string();
            }
            continue;
        }
        if let Some(v) = line.strip_prefix("description:") {
            let v = v.trim();
            if matches!(v, ">" | "|" | ">-" | "|-") {
                found = true;
                continue;
            }
            return v.trim_matches(|c| c == '"' || c == '\'').to_string();
        }
    }
    String::new()
}

/// Rows the popover shows at most.
pub(crate) const SHOWN: usize = 7;

/// Skills whose name contains the query (case-insensitive), grouped by
/// source with prefix hits first inside a group.
pub(crate) fn matching<'a>(skills: &'a [Skill], query: &str) -> Vec<&'a Skill> {
    let q = query.to_lowercase();
    let mut hits: Vec<&Skill> = skills
        .iter()
        .filter(|s| s.name.to_lowercase().contains(&q))
        .collect();
    hits.sort_by_key(|s| (s.source, !s.name.to_lowercase().starts_with(&q)));
    hits
}

/// A name split around its first match of `query`: before, match, after.
pub(crate) fn split_match(name: &str, query: &str) -> (String, String, String) {
    let at = name.to_lowercase().find(query).unwrap_or(0);
    let end = (at + query.len()).min(name.len());
    match (name.get(..at), name.get(at..end), name.get(end..)) {
        (Some(a), Some(b), Some(c)) => (a.into(), b.into(), c.into()),
        _ => (String::new(), String::new(), name.into()),
    }
}

/// The popover's pick state: the selected row, and the draft text Esc
/// closed it at. A changed draft resets the row and reopens the popover.
#[derive(Clone, Debug, Default)]
pub(crate) struct Pick {
    sel: usize,
    seen: String,
    closed_at: Option<String>,
}

impl Pick {
    fn sel_at(&self, draft: &str, shown: usize) -> usize {
        if self.seen == draft {
            self.sel.min(shown.saturating_sub(1))
        } else {
            0
        }
    }

    pub(crate) fn step(&mut self, draft: &str, down: bool, shown: usize) {
        if shown == 0 {
            return;
        }
        let at = self.sel_at(draft, shown);
        self.seen = draft.to_string();
        self.sel = if down {
            (at + 1) % shown
        } else {
            (at + shown - 1) % shown
        };
    }

    pub(crate) fn close(&mut self, draft: &str) {
        self.closed_at = Some(draft.to_string());
    }
}

/// What the popover shows for a draft.
pub(crate) struct View {
    pub(crate) rows: Vec<Skill>,
    pub(crate) more: usize,
    pub(crate) sel: usize,
    /// The typed query, lowercased: the bold span in each name.
    pub(crate) query: String,
}

/// The popover for `draft` on `harness`: `None` when the draft holds no
/// leading `<sigil>query`, the query has a space, nothing matches, or Esc
/// closed it at this text.
pub(crate) fn view(skills: &[Skill], harness: &str, draft: &str, pick: &Pick) -> Option<View> {
    let sigil = sigil(harness)?;
    let q = draft
        .strip_prefix(sigil)
        .filter(|q| !q.contains(char::is_whitespace))?;
    if pick.closed_at.as_deref() == Some(draft) {
        return None;
    }
    let all = matching(skills, q);
    if all.is_empty() {
        return None;
    }
    let more = all.len().saturating_sub(SHOWN);
    let rows: Vec<Skill> = all.into_iter().take(SHOWN).cloned().collect();
    let sel = pick.sel_at(draft, rows.len());
    Some(View {
        rows,
        more,
        sel,
        query: q.to_lowercase(),
    })
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

    #[test]
    fn the_popover_steps_closes_and_reopens_on_a_new_draft() {
        let mk = |n: &str| Skill {
            source: Source::User,
            name: n.into(),
            description: String::new(),
        };
        let all = vec![mk("fno-a"), mk("fno-b"), mk("fno-c")];
        let mut pick = Pick::default();
        assert_eq!(view(&all, "claude", "/fno", &pick).unwrap().sel, 0);
        pick.step("/fno", true, 3);
        assert_eq!(view(&all, "claude", "/fno", &pick).unwrap().sel, 1);
        pick.step("/fno", false, 3);
        pick.step("/fno", false, 3);
        assert_eq!(view(&all, "claude", "/fno", &pick).unwrap().sel, 2);
        // A longer draft resets the row; a space ends the query.
        assert_eq!(view(&all, "claude", "/fno-", &pick).unwrap().sel, 0);
        assert!(view(&all, "claude", "/fno ", &pick).is_none());
        pick.close("/fno");
        assert!(view(&all, "claude", "/fno", &pick).is_none());
        assert!(view(&all, "claude", "/fno-", &pick).is_some());
    }

    #[test]
    fn the_view_counts_the_matches_it_leaves_out() {
        let all: Vec<Skill> = (0..SHOWN + 3)
            .map(|i| Skill {
                source: Source::User,
                name: format!("fno-{i}"),
                description: String::new(),
            })
            .collect();
        let v = view(&all, "claude", "/fno", &Pick::default()).unwrap();
        assert_eq!((v.rows.len(), v.more), (SHOWN, 3));
    }
}
