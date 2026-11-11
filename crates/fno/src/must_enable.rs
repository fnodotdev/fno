//! fno's must-enable list: the narrow allow rules for its own record
//! commands and the common reads, rendered in each harness's rule syntax.
//! fno owns no permission layer. Setup writes these rules only after the
//! person approves the `permission-rules` step, and doctor reports what is
//! missing or edited without ever reverting an edit.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Map, Value};

const TABLE: &str = include_str!("must_enable.toml");

/// A rule whose first words are one of these allows arbitrary code, so the
/// loader refuses it (filter A). `rg` is here because `rg --pre` runs any
/// program on every file it searches.
#[rustfmt::skip]
const FILTER_A: &[&str] = &[
    "python", "python3", "node", "bun", "deno", "ruby", "perl", "php", "lua", "bash", "sh", "zsh",
    "fish", "eval", "exec", "ssh", "npx", "bunx", "uvx", "uv run", "xargs", "env", "awk", "sudo",
    "gh api", "git fetch", "git remote", "docker run", "docker exec", "kubectl exec", "npm run",
    "make", "just", "cargo run", "go run", "rg",
];

/// Commands that launch, merge or rotate. A rule may neither name one nor
/// sit above one: `fno agents *` would cover `fno agents spawn`.
const NEVER: &[&str] = &[
    "fno agents spawn",
    "fno do pr merge",
    "fno do pr create",
    "fno do pr push",
    "fno do target",
    "fno config accounts use",
    "fno agents session stop",
    "fno agents session rm",
    "fno agents session resume",
    "fno agents session recover",
    "fno agents session move",
];

const CODEX_HEADER: &str = "# Written by fno (`fno config setup run --only permission-rules`).\n# fno reads an edit here as drift and never reverts it. Delete the file and\n# run the step again to take the current list.\n";

#[derive(Deserialize)]
#[serde(untagged)]
enum Word {
    One(String),
    Any(Vec<String>),
}

#[derive(Deserialize)]
struct Row {
    prefix: Vec<Word>,
    why: String,
}

#[derive(Deserialize)]
struct Table {
    rule: Vec<Row>,
    corpus: Vec<String>,
}

pub(crate) struct List {
    rows: Vec<Row>,
    corpus: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Harness {
    Claude,
    Codex,
    Opencode,
    Pi,
    Agy,
}

impl Harness {
    pub(crate) const ALL: [Harness; 5] = [
        Harness::Claude,
        Harness::Codex,
        Harness::Opencode,
        Harness::Pi,
        Harness::Agy,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Opencode => "opencode",
            Harness::Pi => "pi",
            Harness::Agy => "agy",
        }
    }
}

/// Every concrete word sequence a row allows.
fn expand(prefix: &[Word]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = vec![Vec::new()];
    for word in prefix {
        let alts: Vec<&String> = match word {
            Word::One(w) => vec![w],
            Word::Any(ws) => ws.iter().collect(),
        };
        out = out
            .iter()
            .flat_map(|head| {
                alts.iter().map(move |w| {
                    let mut next = head.clone();
                    next.push((*w).clone());
                    next
                })
            })
            .collect();
    }
    out
}

/// True when one word sequence is a prefix of the other, so a rule for
/// either one covers a command of the shorter one's family.
fn overlaps(a: &[String], b: &str) -> bool {
    let b: Vec<&str> = b.split(' ').collect();
    let n = a.len().min(b.len());
    a[..n].iter().zip(&b[..n]).all(|(x, y)| x == y)
}

/// The plain-word segments of a compound command, split where all three
/// harnesses split one: `&&`, `||`, `;` and `|`.
fn segments(command: &str) -> Vec<Vec<&str>> {
    command
        .split("&&")
        .flat_map(|s| s.split("||"))
        .flat_map(|s| s.split(';'))
        .flat_map(|s| s.split('|'))
        .map(|s| s.split_whitespace().collect::<Vec<_>>())
        .filter(|w| !w.is_empty())
        .collect()
}

fn starts_with(words: &[&str], prefix: &[String]) -> bool {
    words.len() >= prefix.len() && prefix.iter().zip(words).all(|(p, w)| p == w)
}

pub(crate) fn load(text: &str) -> Result<List, String> {
    let table: Table = toml::from_str(text).map_err(|e| format!("must_enable.toml: {e}"))?;
    let corpus = table.corpus;
    for row in &table.rule {
        for words in expand(&row.prefix) {
            let rule = words.join(" ");
            if let Some(hit) = FILTER_A.iter().find(|f| overlaps(&words, f)) {
                return Err(format!(
                    "filter A refuses rule `{rule}`: it reaches `{hit}`, which runs any code"
                ));
            }
            if let Some(hit) = NEVER.iter().find(|f| overlaps(&words, f)) {
                return Err(format!(
                    "the never-list refuses rule `{rule}`: it reaches `{hit}`"
                ));
            }
            let witnessed = corpus
                .iter()
                .any(|c| segments(c).iter().any(|seg| starts_with(seg, &words)));
            if !witnessed {
                return Err(format!(
                    "rule `{rule}` has no corpus command it allows; add one"
                ));
            }
        }
    }
    Ok(List {
        rows: table.rule,
        corpus,
    })
}

pub(crate) fn shipped() -> List {
    load(TABLE).expect("the shipped must_enable.toml loads")
}

impl List {
    /// One `Bash(<prefix> *)` per alternative, for `permissions.allow`.
    pub(crate) fn claude(&self) -> Vec<String> {
        self.rows
            .iter()
            .flat_map(|r| expand(&r.prefix))
            .map(|w| format!("Bash({} *)", w.join(" ")))
            .collect()
    }

    /// The whole `fno.rules` file: one `prefix_rule` per row, each with
    /// the first corpus command it allows as its `match` example.
    pub(crate) fn codex(&self) -> String {
        let mut out = CODEX_HEADER.to_string();
        for row in &self.rows {
            let pattern: Vec<String> = row
                .prefix
                .iter()
                .map(|w| match w {
                    Word::One(s) => format!("{s:?}"),
                    Word::Any(v) => format!("{v:?}"),
                })
                .collect();
            let example = self
                .corpus
                .iter()
                .flat_map(|c| segments(c))
                .find(|seg| expand(&row.prefix).iter().any(|w| starts_with(seg, w)))
                .map(|seg| seg.join(" "))
                .unwrap_or_default();
            out.push_str(&format!(
                "\nprefix_rule(\n    pattern = [{}],\n    decision = \"allow\",\n    justification = {:?},\n    match = [{example:?}],\n)\n",
                pattern.join(", "),
                row.why
            ));
        }
        out
    }

    /// The v1 `permission.bash` keys, each mapped to `allow`.
    fn opencode_v1(&self) -> Vec<String> {
        self.rows
            .iter()
            .flat_map(|r| expand(&r.prefix))
            .map(|w| format!("{} *", w.join(" ")))
            .collect()
    }

    /// The v2 `permissions` rows.
    fn opencode_v2(&self) -> Vec<Value> {
        self.opencode_v1()
            .into_iter()
            .map(|r| json!({"action": "shell", "resource": r, "effect": "allow"}))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Where each harness keeps its rules
// ---------------------------------------------------------------------------

fn codex_home() -> PathBuf {
    crate::cli_hooks::env_path("CODEX_HOME")
        .unwrap_or_else(|| crate::cli_hooks::home().join(".codex"))
}

fn opencode_home() -> PathBuf {
    crate::cli_hooks::env_path("OPENCODE_CONFIG_DIR")
        .unwrap_or_else(|| crate::cli_hooks::home().join(".config/opencode"))
}

/// The rule file for a harness whose config home exists, else None.
pub(crate) fn target(h: Harness) -> Option<PathBuf> {
    match h {
        Harness::Claude => {
            let path = crate::cli_hooks::user_claude_settings_path();
            path.parent().is_some_and(Path::is_dir).then_some(path)
        }
        Harness::Codex => {
            let home = codex_home();
            home.is_dir().then(|| home.join("rules").join("fno.rules"))
        }
        Harness::Opencode => {
            let home = opencode_home();
            home.is_dir().then(|| opencode_file(&home))
        }
        Harness::Pi | Harness::Agy => None,
    }
}

fn opencode_file(home: &Path) -> PathBuf {
    let jsonc = home.join("opencode.jsonc");
    if jsonc.exists() {
        jsonc
    } else {
        home.join("opencode.json")
    }
}

// ---------------------------------------------------------------------------
// Writers
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Wrote,
    AlreadyPresent,
    /// The person edited fno's file; the edit wins.
    KeptEdit,
    Refused(String),
}

pub(crate) fn install(h: Harness, list: &List, path: &Path) -> Outcome {
    match h {
        Harness::Claude => install_claude(list, path),
        Harness::Codex => install_codex(list, path),
        Harness::Opencode => install_opencode(list, path, opencode_major),
        Harness::Pi | Harness::Agy => Outcome::Refused("no rule syntax to translate to".into()),
    }
}

fn write(path: &Path, data: Map<String, Value>) -> Outcome {
    match crate::cli_hooks::write_json(path, &Value::Object(data)) {
        Ok(_) => Outcome::Wrote,
        Err(e) => Outcome::Refused(format!("write failed ({e}); left unchanged")),
    }
}

/// Append the missing rules to `permissions.allow`, after the person's own.
fn install_claude(list: &List, path: &Path) -> Outcome {
    let mut data = match crate::cli_hooks::load_settings(path) {
        Ok(d) => d,
        Err(e) => return Outcome::Refused(e),
    };
    let Some(perms) = data
        .entry("permissions")
        .or_insert_with(|| json!({}))
        .as_object_mut()
    else {
        return Outcome::Refused("`permissions` is not an object; left unchanged".into());
    };
    let Some(allow) = perms
        .entry("allow")
        .or_insert_with(|| json!([]))
        .as_array_mut()
    else {
        return Outcome::Refused("`permissions.allow` is not an array; left unchanged".into());
    };
    let missing: Vec<String> = list
        .claude()
        .into_iter()
        .filter(|r| !allow.iter().any(|v| v.as_str() == Some(r.as_str())))
        .collect();
    if missing.is_empty() {
        return Outcome::AlreadyPresent;
    }
    allow.extend(missing.into_iter().map(Value::String));
    write(path, data)
}

/// fno owns `fno.rules` whole. An existing file that differs is the
/// person's edit and stays.
fn install_codex(list: &List, path: &Path) -> Outcome {
    let want = list.codex();
    match std::fs::read_to_string(path) {
        Ok(have) if have == want => return Outcome::AlreadyPresent,
        Ok(_) => return Outcome::KeptEdit,
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Outcome::Refused(format!("{} could not be read ({e})", path.display()))
        }
        Err(_) => {}
    }
    match crate::cli_hooks::atomic_write(path, &want) {
        Ok(()) => Outcome::Wrote,
        Err(e) => Outcome::Refused(format!("write failed ({e}); left unchanged")),
    }
}

/// The installed opencode's major version, when it answers within 5s.
fn opencode_major() -> Option<u32> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("opencode")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while child.try_wait().ok()?.is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim().split('.').next()?.parse().ok()
}

/// opencode applies the LAST matching rule, so fno's rules go first and a
/// rule of the person's later in the list wins. v2 reads a `permissions`
/// array; v1 reads a `permission.bash` map.
fn install_opencode(list: &List, path: &Path, major: fn() -> Option<u32>) -> Outcome {
    let mut data = match read_opencode(path) {
        Ok(d) => d,
        Err(e) => return Outcome::Refused(e),
    };
    let v2 = data.contains_key("permissions")
        || (!data.contains_key("permission") && major().is_some_and(|m| m >= 2));
    if v2 {
        let Some(rows) = data
            .entry("permissions")
            .or_insert_with(|| json!([]))
            .as_array_mut()
        else {
            return Outcome::Refused("`permissions` is not an array; left unchanged".into());
        };
        let missing: Vec<Value> = list
            .opencode_v2()
            .into_iter()
            .filter(|r| !rows.contains(r))
            .collect();
        if missing.is_empty() {
            return Outcome::AlreadyPresent;
        }
        rows.splice(0..0, missing);
        return write(path, data);
    }
    let Some(perm) = data
        .entry("permission")
        .or_insert_with(|| json!({}))
        .as_object_mut()
    else {
        return Outcome::Refused("`permission` is not an object; left unchanged".into());
    };
    let Some(bash) = perm
        .entry("bash")
        .or_insert_with(|| json!({}))
        .as_object_mut()
    else {
        return Outcome::Refused("`permission.bash` is not an object; left unchanged".into());
    };
    let missing: Vec<String> = list
        .opencode_v1()
        .into_iter()
        .filter(|k| !bash.contains_key(k))
        .collect();
    if missing.is_empty() {
        return Outcome::AlreadyPresent;
    }
    let mut merged: Map<String, Value> = missing.into_iter().map(|k| (k, json!("allow"))).collect();
    merged.extend(std::mem::take(bash));
    *bash = merged;
    write(path, data)
}

/// An absent file reads as empty. A file with comments or trailing commas
/// is refused rather than rewritten without them.
fn read_opencode(path: &Path) -> Result<Map<String, Value>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(format!("{} could not be read ({e})", path.display())),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(format!("{} is not a JSON object; left unchanged", path.display())),
        Err(_) => Err(format!(
            "{} holds comments or is not plain JSON; fno will not rewrite it. Add the rules by hand",
            path.display()
        )),
    }
}

// ---------------------------------------------------------------------------
// Doctor status
// ---------------------------------------------------------------------------

/// `{harness, status, path, missing}` with status one of present, missing,
/// drift, untranslatable, absent or unknown. A read, never a repair.
pub(crate) fn status(h: Harness, list: &List, path: Option<&Path>) -> Value {
    let name = h.name();
    let Some(path) = path else {
        let status = if matches!(h, Harness::Pi | Harness::Agy) {
            "untranslatable"
        } else {
            "absent"
        };
        return json!({"harness": name, "status": status});
    };
    let shown = path.display().to_string();
    let present: Result<(Vec<String>, Vec<String>), String> = match h {
        Harness::Claude => crate::cli_hooks::load_settings(path).map(|d| {
            let allow = d
                .get("permissions")
                .and_then(|p| p.get("allow"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            split(list.claude(), |r| {
                allow.iter().any(|v| v.as_str() == Some(r))
            })
        }),
        Harness::Codex => match std::fs::read_to_string(path) {
            Ok(have) if have == list.codex() => Ok((vec!["fno.rules".into()], vec![])),
            Ok(_) => {
                return json!({"harness": name, "status": "drift", "path": shown, "missing": []})
            }
            Err(_) => Ok((vec![], vec!["fno.rules".into()])),
        },
        Harness::Opencode => read_opencode(path).map(|d| {
            let v1 = d.get("permission").and_then(|p| p.get("bash"));
            let v2 = d.get("permissions").and_then(Value::as_array);
            split(list.opencode_v1(), |k| {
                v1.and_then(|b| b.get(k)).and_then(Value::as_str) == Some("allow")
                    || v2.is_some_and(|rows| {
                        rows.iter()
                            .any(|r| r["resource"].as_str() == Some(k) && r["effect"] == "allow")
                    })
            })
        }),
        Harness::Pi | Harness::Agy => unreachable!("no target path"),
    };
    match present {
        Err(detail) => {
            json!({"harness": name, "status": "unknown", "path": shown, "detail": detail})
        }
        Ok((have, missing)) => {
            let status = match (have.is_empty(), missing.is_empty()) {
                (_, true) => "present",
                (true, false) => "missing",
                (false, false) => "drift",
            };
            json!({"harness": name, "status": status, "path": shown, "missing": missing})
        }
    }
}

/// The doctor's `must_enable_rules` key: one status row per harness.
pub(crate) fn doctor_report() -> Value {
    let list = shipped();
    let rows = Harness::ALL.iter().map(|&h| {
        let path = target(h);
        let mut row = status(h, &list, path.as_deref());
        if h == Harness::Claude && path.as_deref().is_some_and(claude_rewrites_through_rtk) {
            row["rewriter"] = json!("rtk");
        }
        row
    });
    Value::Array(rows.collect())
}

/// One human line per harness that is missing rules, edited or unreadable.
pub(crate) fn doctor_lines(report: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for row in report.as_array().into_iter().flatten() {
        let (h, path) = (
            row["harness"].as_str().unwrap_or("?"),
            row["path"].as_str().unwrap_or(""),
        );
        match row["status"].as_str() {
            Some("missing") => out.push(format!("fno doctor: {h} has none of fno's must-enable rules at {path}. Add them: `fno config setup run --only permission-rules`.")),
            Some("drift") => out.push(format!("fno doctor: {h} must-enable rules at {path} differ from the shipped list; your edit stays and doctor never reverts it.")),
            Some("unknown") => out.push(format!("fno doctor: {h} rules at {path} could not be read: {}", row["detail"].as_str().unwrap_or(""))),
            _ => {}
        }
        if row["rewriter"] == "rtk" {
            out.push("fno doctor: a Claude PreToolUse hook rewrites Bash through rtk, and Claude matches permissions.allow against the rewritten command, so read rules such as Bash(grep *) do not match `rtk grep`.".into());
        }
    }
    out
}

fn split(rules: Vec<String>, has: impl Fn(&str) -> bool) -> (Vec<String>, Vec<String>) {
    rules.into_iter().partition(|r| has(r))
}

/// True when a Claude PreToolUse hook pipes Bash through rtk. Claude
/// matches `permissions.allow` against the rewritten command, so
/// `Bash(grep *)` stops matching `rtk grep`.
pub(crate) fn claude_rewrites_through_rtk(path: &Path) -> bool {
    let Ok(d) = crate::cli_hooks::load_settings(path) else {
        return false;
    };
    d.get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|h| h.get("command").and_then(Value::as_str))
        .any(|c| {
            c.split(|ch: char| !ch.is_ascii_alphanumeric())
                .any(|w| w == "rtk")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_allows(rule: &str, seg: &[&str]) -> bool {
        let p = rule
            .strip_prefix("Bash(")
            .and_then(|r| r.strip_suffix(" *)"));
        let cmd = seg.join(" ");
        p.is_some_and(|p| cmd == p || cmd.starts_with(&format!("{p} ")))
    }

    /// opencode's whole-value glob, where a trailing ` *` also matches the
    /// bare command.
    fn glob(pat: &str, text: &str) -> bool {
        if let Some(bare) = pat.strip_suffix(" *") {
            if text == bare {
                return true;
            }
        }
        fn go(p: &[u8], t: &[u8]) -> bool {
            match p.first() {
                None => t.is_empty(),
                Some(b'*') => (0..=t.len()).any(|i| go(&p[1..], &t[i..])),
                Some(b'?') => !t.is_empty() && go(&p[1..], &t[1..]),
                Some(c) => t.first() == Some(c) && go(&p[1..], &t[1..]),
            }
        }
        go(pat.as_bytes(), text.as_bytes())
    }

    #[test]
    fn every_corpus_command_is_allowed_by_each_generated_rule_set() {
        let list = shipped();
        let claude = list.claude();
        let opencode = list.opencode_v1();
        assert!(claude.contains(&"Bash(fno backlog *)".to_string()));
        let codex = list.codex();
        assert_eq!(codex.matches("prefix_rule(").count(), list.rows.len());
        assert!(
            !codex.contains("match = [\"\"]"),
            "a codex rule lacks a match example"
        );
        for command in &list.corpus {
            for seg in segments(command) {
                let line = seg.join(" ");
                assert!(
                    claude.iter().any(|r| claude_allows(r, &seg)),
                    "claude: `{line}` in `{command}`"
                );
                assert!(
                    opencode.iter().any(|r| glob(r, &line)),
                    "opencode: `{line}` in `{command}`"
                );
                let codex_hit = list
                    .rows
                    .iter()
                    .any(|r| expand(&r.prefix).iter().any(|w| starts_with(&seg, w)));
                assert!(codex_hit, "codex: `{line}` in `{command}`");
            }
        }
        for h in [Harness::Pi, Harness::Agy] {
            assert_eq!(status(h, &list, None)["status"], "untranslatable");
        }
    }

    #[test]
    fn filter_a_and_the_never_list_refuse_a_row() {
        let corpus = "corpus = [\"python3 x\", \"uv run x\", \"fno agents spawn x\"]\n";
        for (prefix, word) in [
            ("[\"python3\"]", "filter A"),
            ("[\"uv\"]", "filter A"),
            ("[\"fno\", \"agents\"]", "never-list"),
            ("[\"fno\", \"agents\", [\"mail\", \"spawn\"]]", "never-list"),
        ] {
            let text = format!("{corpus}[[rule]]\nprefix = {prefix}\nwhy = \"x\"\n");
            let err = load(&text)
                .err()
                .unwrap_or_else(|| panic!("{prefix} loaded"));
            assert!(err.contains(word), "{prefix}: {err}");
        }
    }

    #[test]
    fn writers_add_once_put_opencode_rules_first_and_report_present() {
        let list = shipped();
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("settings.json");
        std::fs::write(
            &claude,
            r#"{"permissions":{"allow":["Bash(ls *)"]},"autoMode":{"allow":["x"]}}"#,
        )
        .unwrap();
        let codex = dir.path().join("rules/fno.rules");
        let oc = dir.path().join("opencode.json");
        let user = json!({"action": "shell", "resource": "fno backlog *", "effect": "ask"});
        std::fs::write(&oc, json!({"permissions": [user.clone()]}).to_string()).unwrap();
        let none = || None;
        assert_eq!(install_claude(&list, &claude), Outcome::Wrote);
        assert_eq!(install_codex(&list, &codex), Outcome::Wrote);
        assert_eq!(install_opencode(&list, &oc, none), Outcome::Wrote);
        let bytes: Vec<Vec<u8>> = [&claude, &codex, &oc]
            .iter()
            .map(|p| std::fs::read(p).unwrap())
            .collect();
        assert_eq!(install_claude(&list, &claude), Outcome::AlreadyPresent);
        assert_eq!(install_codex(&list, &codex), Outcome::AlreadyPresent);
        assert_eq!(install_opencode(&list, &oc, none), Outcome::AlreadyPresent);
        let again: Vec<Vec<u8>> = [&claude, &codex, &oc]
            .iter()
            .map(|p| std::fs::read(p).unwrap())
            .collect();
        assert_eq!(bytes, again, "a second run changed bytes");
        let c: Value = serde_json::from_slice(&again[0]).unwrap();
        assert_eq!(c["permissions"]["allow"][0], "Bash(ls *)");
        assert_eq!(c["autoMode"], json!({"allow": ["x"]}));
        let o: Value = serde_json::from_slice(&again[2]).unwrap();
        let rows = o["permissions"].as_array().unwrap();
        assert_eq!(
            rows.last(),
            Some(&user),
            "the person's rule must stay last so it wins"
        );
        for (h, p) in [
            (Harness::Claude, &claude),
            (Harness::Codex, &codex),
            (Harness::Opencode, &oc),
        ] {
            assert_eq!(
                status(h, &list, Some(p.as_path()))["status"],
                "present",
                "{h:?}"
            );
        }
        let v1 = dir.path().join("v1.json");
        assert_eq!(install_opencode(&list, &v1, none), Outcome::Wrote);
        let v: Value = serde_json::from_slice(&std::fs::read(&v1).unwrap()).unwrap();
        assert_eq!(v["permission"]["bash"]["fno backlog *"], "allow");
    }

    #[test]
    fn bad_shapes_refuse_and_a_removed_rule_reads_drift_byte_identical() {
        let list = shipped();
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("settings.json");
        let oc = dir.path().join("opencode.jsonc");
        let codex = dir.path().join("fno.rules");
        for (path, text) in [
            (&claude, r#"{"permissions":{"allow":"Bash(ls *)"}}"#),
            (&oc, r#"{"permissions":{"x":1}}"#),
        ] {
            std::fs::write(path, text).unwrap();
        }
        std::fs::write(&codex, "# mine\n").unwrap();
        assert!(matches!(
            install_claude(&list, &claude),
            Outcome::Refused(_)
        ));
        assert!(matches!(
            install_opencode(&list, &oc, || None),
            Outcome::Refused(_)
        ));
        assert_eq!(install_codex(&list, &codex), Outcome::KeptEdit);
        std::fs::write(&oc, "{ // mine\n}").unwrap();
        assert!(matches!(
            install_opencode(&list, &oc, || None),
            Outcome::Refused(_)
        ));
        assert_eq!(
            std::fs::read_to_string(&claude).unwrap(),
            r#"{"permissions":{"allow":"Bash(ls *)"}}"#
        );
        assert_eq!(std::fs::read_to_string(&oc).unwrap(), "{ // mine\n}");
        assert_eq!(std::fs::read_to_string(&codex).unwrap(), "# mine\n");
        assert_eq!(
            status(Harness::Codex, &list, Some(codex.as_path()))["status"],
            "drift"
        );
        let mut rules = list.claude();
        let removed = rules.remove(0);
        std::fs::write(
            &claude,
            json!({"permissions": {"allow": rules}}).to_string(),
        )
        .unwrap();
        let before = std::fs::read(&claude).unwrap();
        let s = status(Harness::Claude, &list, Some(claude.as_path()));
        assert_eq!(s["status"], "drift");
        assert_eq!(s["missing"], json!([removed]));
        assert_eq!(std::fs::read(&claude).unwrap(), before);
    }
}
