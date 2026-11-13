//! The shell-runner script doors. `bash script.sh` and `bash <<'EOF'` hand a
//! script body to a shell without it ever appearing at command position, so
//! the segment gates never see a push inside. expand() reads what the runner
//! will execute and parses it as more command segments for the same gates.
//!
//! The hook runs before the command, so it walks the segments in order the
//! way the shell will: a `cd` moves the base of a relative script path, a
//! `NAME=value` names a script path, and a writer (`cat > s.sh <<EOF`,
//! `cp a.sh s.sh`) supplies the text a later `bash s.sh` runs. A script that
//! does not exist and that nothing writes runs nothing, so it allows unless
//! the command's own text holds a push.
//!
//! The guard-diet ruling (refuse nothing harmless): the door refuses only
//! what it can read showing a protected push or --no-verify. A script whose
//! text the guard cannot get (a download, a variable it cannot resolve, a
//! binary, a piped stdin, a chain nested past the cap) WARNS and proceeds.

use super::git_gate::is_a_push;
use super::segments::{self, base_lower, Segment};
use super::Ctx;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A script beyond this size is not a realistic agent action; refusing it
/// is fail-closed and caps the hook's read cost.
const MAX_SCRIPT_BYTES: u64 = 1 << 20;
/// A chain deeper than this is adversarial: refuse, never follow.
const MAX_DEPTH: usize = 3;

/// The guard diet's warn-and-proceed yield: the segments the doors parsed,
/// plus what they could not read. A warning surfaces only when no gate
/// refuses.
#[derive(Default)]
pub(super) struct Expanded {
    pub segments: Vec<Segment>,
    pub warnings: Vec<String>,
}

/// What one runner segment contributed: nothing, a readable script, an
/// unreadable script (label + why: a warning at the top level, a silent
/// skip nested, since the enclosing readable text is gated), or a refusal
/// (readable text shows a push).
enum Runner {
    None,
    Text(String, String),
    Warn(String, String),
    Refuse(Refusal),
}

/// The refusal text a script door blocks with, newtyped so this module adds
/// no string-error surface (R6: that pattern is shrink-only per crate).
pub(super) struct Refusal(pub String);

impl From<Refusal> for String {
    fn from(r: Refusal) -> String {
        r.0
    }
}

/// Every segment the runner segments hide, in execution order, plus the
/// unreadable-script warnings. `Err` is a refusal: some readable text shows
/// a push, the only refusing fact this door reports.
pub(super) fn expand(command: &str, segs: &[Segment], ctx: &Ctx) -> Result<Expanded, Refusal> {
    let mut out = Expanded::default();
    visit(command, segs, ctx, &ctx.cwd, 0, &mut out)?;
    Ok(out)
}

/// What a writer in the same command puts at a path.
#[derive(Clone)]
enum Source {
    /// Text the guard can read: a heredoc body or a copied file.
    Text(String),
    /// A writer whose output the guard cannot read, by its command text.
    Opaque(String),
}

/// The shell state one command builds up as its segments run in order.
struct Walk {
    /// None after a `cd` the guard cannot resolve.
    cwd: Option<PathBuf>,
    /// None marks a name assigned a value the guard cannot resolve.
    vars: HashMap<String, Option<String>>,
    written: Vec<(PathBuf, Source)>,
    heredocs: Vec<(String, String)>,
    /// Segments outside `$( ... )`: only these change cwd and variables.
    top: Vec<Segment>,
}

impl Walk {
    fn new(command: &str, cwd: &Path) -> Self {
        Walk {
            cwd: Some(cwd.to_path_buf()),
            vars: HashMap::new(),
            written: Vec::new(),
            heredocs: segments::heredoc_writes(command),
            top: segments::top_level_segments(command),
        }
    }

    /// `$NAME` and `${NAME}` from this command's assignments, `$PWD`, then
    /// the hook's environment. None when any expansion stays unresolved.
    fn expand_vars(&self, raw: &str) -> Option<String> {
        static VAR: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let re = VAR.get_or_init(|| {
            regex::Regex::new(r"\$(?:\{([A-Za-z_][A-Za-z0-9_]*)\}|([A-Za-z_][A-Za-z0-9_]*))")
                .expect("static regex")
        });
        if raw.contains("$(") || raw.contains('`') {
            return None;
        }
        let mut unresolved = false;
        let text = re.replace_all(raw, |caps: &regex::Captures| {
            let name = caps
                .get(1)
                .or_else(|| caps.get(2))
                .map_or("", |m| m.as_str());
            let value = match self.vars.get(name) {
                Some(v) => v.clone(),
                None if name == "PWD" => self.cwd.as_ref().map(|p| p.display().to_string()),
                None => std::env::var(name).ok(),
            };
            value.unwrap_or_else(|| {
                unresolved = true;
                String::new()
            })
        });
        (!unresolved && !text.contains('$')).then(|| text.into_owned())
    }

    fn resolve(&self, raw: &str) -> Option<PathBuf> {
        let text = self.expand_vars(raw)?;
        let path = match text.strip_prefix("~/") {
            Some(rest) => PathBuf::from(std::env::var("HOME").ok()?).join(rest),
            None => PathBuf::from(text),
        };
        if path.is_absolute() {
            Some(path)
        } else {
            self.cwd.as_ref().map(|cwd| cwd.join(path))
        }
    }

    fn written_to(&self, path: &Path) -> Option<&Source> {
        self.written
            .iter()
            .rev()
            .find(|(p, _)| p == path)
            .map(|(_, s)| s)
    }

    /// Fold one segment's effect on the shell state the later segments see.
    fn observe(&mut self, seg: &Segment) {
        let (argv, assignments) = segments::effective_argv_and_assignments(seg);
        let base0 = argv.first().map(|t| base_lower(t)).unwrap_or_default();
        let top = self.top.contains(seg);
        if top && argv.is_empty() {
            self.assign(&assignments);
        } else if top && base0 == "export" {
            self.assign(argv.get(1..).unwrap_or_default());
        } else if top && base0 == "cd" {
            let target = argv
                .iter()
                .skip(1)
                .find(|a| !a.starts_with('-') || *a == "-");
            self.cwd = match target.map(String::as_str) {
                None => std::env::var("HOME").ok().map(PathBuf::from),
                Some("-") => None,
                Some(t) => self.resolve(t),
            };
        }
        if let Some(target) = segments::write_target(seg) {
            let source = match self.heredocs.iter().position(|(t, _)| *t == target) {
                Some(at) => Source::Text(self.heredocs.remove(at).1),
                None => Source::Opaque(seg.join(" ")),
            };
            self.record(&target, source);
        }
        if ["cp", "mv", "install"].contains(&base0.as_str()) {
            let files: Vec<&String> = argv
                .iter()
                .skip(1)
                .filter(|a| !a.starts_with('-'))
                .collect();
            if let [src, dst] = files.as_slice() {
                let src_path = self.resolve(src);
                let source = match src_path.as_deref().and_then(|p| self.written_to(p)) {
                    Some(s) => s.clone(),
                    None => match src_path.as_deref().map(read_script) {
                        Some(Ok(text)) => Source::Text(text),
                        _ => Source::Opaque(seg.join(" ")),
                    },
                };
                let dst_path = self.resolve(dst).map(|d| {
                    match (d.is_dir(), Path::new(src.as_str()).file_name()) {
                        (true, Some(name)) => d.join(name),
                        _ => d,
                    }
                });
                if let Some(d) = dst_path {
                    self.written.push((d, source));
                }
            }
        }
        if base0 == "curl" || base0 == "wget" {
            let out_flag = argv
                .iter()
                .position(|a| ["-o", "-O", "--output", "--output-document"].contains(&a.as_str()));
            if let Some(target) = out_flag.and_then(|at| argv.get(at + 1)) {
                self.record(target, Source::Opaque(seg.join(" ")));
            }
        }
    }

    fn assign(&mut self, assignments: &[String]) {
        for a in assignments {
            if let Some((name, value)) = a.split_once('=') {
                let value = self.expand_vars(value);
                self.vars.insert(name.to_string(), value);
            }
        }
    }

    fn record(&mut self, target: &str, source: Source) {
        if let Some(path) = self.resolve(target) {
            self.written.push((path, source));
        }
    }
}

/// Follow the runner segments of `segs`, reading each script they would
/// execute and parsing its lines as command segments. `command` is the text
/// heredoc bodies are read from at this level.
fn visit(
    command: &str,
    segs: &[Segment],
    ctx: &Ctx,
    cwd: &Path,
    depth: usize,
    out: &mut Expanded,
) -> Result<(), Refusal> {
    if depth > MAX_DEPTH {
        let why = format!("script chain nests more than {MAX_DEPTH} shells deep");
        if could_push(command) {
            return Err(refusal(command, "<chain>", &why));
        }
        out.warnings.push(warn_line("<chain>", &why));
        return Ok(());
    }
    let mut walk = Walk::new(command, cwd);
    for seg in segs {
        let found = runner_script(command, seg, &walk);
        walk.observe(seg);
        match found {
            Runner::None => {}
            Runner::Warn(label, why) => {
                // A nested helper the door cannot read (a variable the
                // script assigns at run time, a binary) carries no readable
                // text, and the enclosing script's own lines are gated, so
                // it skips silently; the outermost door warns.
                if depth == 0 {
                    out.warnings.push(warn_line(&label, &why));
                }
            }
            Runner::Refuse(r) => return Err(r),
            Runner::Text(text, label) => {
                let inner = parse_script_text(command, &label, &text)?;
                out.segments.extend(inner.iter().cloned());
                if !inner.is_empty() {
                    let child_cwd = walk.cwd.clone().unwrap_or_else(|| ctx.cwd.clone());
                    visit(&text, &inner, ctx, &child_cwd, depth + 1, out)?;
                }
            }
        }
    }
    Ok(())
}

/// The script text a runner segment will execute with the label the refusal
/// names it by (its path, or a stdin form), or None. `bash -c` strings are
/// excluded: effective_argv already re-tokenizes them, so argv[0] is the
/// inner command, never the runner.
fn runner_script(command: &str, seg: &Segment, walk: &Walk) -> Runner {
    let argv = segments::effective_argv(seg);
    let Some(base0) = argv.first().map(|t| base_lower(t)) else {
        return Runner::None;
    };
    if !segments::SHELL_RUNNERS.contains(&base0.as_str()) {
        return Runner::None;
    }
    // First non-flag token after the runner is the script; `<` names a
    // stdin script instead. `-c` strings are not files.
    let list: Vec<&String> = argv.iter().skip(1).collect();
    let mut file: Option<&String> = None;
    let mut herestring: Option<&String> = None;
    let mut i = 0;
    while i < list.len() {
        let Some(tok) = list.get(i).copied() else {
            break;
        };
        if segments::is_dash_c(tok) {
            i += 2;
            continue;
        }
        if tok.starts_with('-') {
            i += 1;
            continue;
        }
        if tok == "<" {
            file = list.get(i + 1).copied();
            break;
        }
        if tok == "<<" {
            // Heredoc stdin: the body is the script; stop scanning so the
            // delimiter word never reads as a file.
            break;
        }
        if tok == "<<<" {
            herestring = list.get(i + 1).copied();
            break;
        }
        file = Some(tok);
        break;
    }
    if let Some(body) = herestring {
        return Runner::Text(body.clone(), "<herestring>".to_string());
    }
    let Some(path) = file else {
        // Bare runner: a heredoc stdin is its script. `-c` never lands here.
        let bodies = segments::heredoc_bodies(command);
        if !bodies.is_empty() {
            return Runner::Text(bodies.join("\n"), "<heredoc>".to_string());
        }
        // No heredoc: a piped stdin is the only remaining script source,
        // and its text never appears in the command. The readable command
        // text showing a push refuses; anything else warns and proceeds.
        if segments::pipe_fed_texts(command)
            .iter()
            .any(|t| *t == seg.join(" "))
        {
            // The readable command text showing a push refuses (a pipe
            // would carry it past the gate); anything else warns.
            if could_push(command) {
                return Runner::Refuse(piped_refusal(command));
            }
            return Runner::Warn(
                "<stdin>".to_string(),
                "is fed to the runner by a pipe, so its text never appears in the command"
                    .to_string(),
            );
        }
        return Runner::None;
    };
    let label = path.to_string();
    let Some(full) = walk.resolve(path) else {
        let why = if walk.expand_vars(path).is_none() {
            "names its path through a variable or substitution the guard cannot resolve. \
             Assign the path in this same command, or spell it out"
        } else {
            "is relative to a `cd` target the guard cannot resolve. Spell the directory out, \
             or give the script an absolute path"
        };
        return decide(command, path, why);
    };
    match walk.written_to(&full) {
        Some(Source::Text(text)) => return Runner::Text(text.clone(), label),
        Some(Source::Opaque(writer)) => {
            return decide(
                command,
                path,
                &format!(
                    "is written in this same command by `{writer}`, whose output the guard \
                     cannot read. Write it in one call and run it in the next"
                ),
            )
        }
        None => {}
    }
    if !full.exists() {
        // A missing script runs nothing: bash fails on it. Only a push in
        // this command's own text, which an unseen writer could put into
        // the script, keeps the refusal.
        if could_push(command) {
            return Runner::Refuse(refusal(
                command,
                path,
                "does not exist yet, and this command's own text holds a push that a writer \
                 the guard cannot follow may put into it. Write the script in one call and \
                 run it in the next",
            ));
        }
        return Runner::None;
    }
    match read_script(&full) {
        Ok(text) => Runner::Text(text, label),
        Err(why) => decide(command, path, &why),
    }
}

/// The guard-diet split for one unreadable script: the readable command
/// text showing a push refuses; anything else warns and proceeds.
fn decide(command: &str, script: &str, why: &str) -> Runner {
    if could_push(command) {
        Runner::Refuse(refusal(command, script, why))
    } else {
        Runner::Warn(script.to_string(), why.to_string())
    }
}

/// Read the script file. `Err` carries the why; the caller decides between
/// a refusal (the command text holds a push) and warn-and-proceed.
fn read_script(full: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(full).map_err(|e| read_why(&e))?;
    if !meta.is_file() {
        return Err("is not a regular file".to_string());
    }
    if meta.len() > MAX_SCRIPT_BYTES {
        return Err("is larger than the guard's 1 MiB read cap".to_string());
    }
    let bytes = std::fs::read(full).map_err(|e| read_why(&e))?;
    if bytes.contains(&0u8) {
        return Err("contains NUL bytes (binary)".to_string());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_why(e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        "does not exist".to_string()
    } else {
        format!("unreadable ({e})")
    }
}

/// Parse script text as command segments. Text that parses as unbalanced is
/// judged by the fallback: a `git push` word pair refuses, otherwise allow -
/// the guard exists for the protected-branch push, and a script whose only
/// git lines are read-only (`git rev-parse`) must pass. The refusal names
/// the real command and the script location, never the raw body.
fn parse_script_text(command: &str, label: &str, text: &str) -> Result<Vec<Segment>, Refusal> {
    match segments::command_segments(text) {
        Ok(inner) => Ok(inner),
        Err(_) => {
            if could_push(text) {
                Err(refusal(command, label, "cannot be parsed as shell text"))
            } else {
                Ok(Vec::new())
            }
        }
    }
}

/// The fallback's push read at text level. Quote characters drop before the
/// match, so a spliced spelling (`git 'pu'sh`) in a script the parser cannot
/// read still refuses: bash joins `'pu'` and `sh` into one word the raw-text
/// regex never sees. A quoted prose mention refuses too, which is the cheap
/// direction for an unreadable script.
fn could_push(text: &str) -> bool {
    let stripped: String = text.chars().filter(|c| *c != '\'' && *c != '"').collect();
    is_a_push(&stripped)
}

fn refusal(command: &str, script: &str, why: &str) -> Refusal {
    Refusal(format!(
        "🚫 BLOCKED: the script this command would run cannot be gated\n\nCommand: {command}\n\n\
         Script: {script}\nReason: {why}\n\n\
         A shell runner on a script file must let the guard read the script, so the \
         protected-branch push gate can see every line it executes. No marker opens this \
         door: write the script where the guard can read it, or run the steps as typed commands."
    ))
}

/// The warn-and-proceed line an unreadable script produces. It surfaces
/// only when no gate refuses.
fn warn_line(label: &str, why: &str) -> String {
    format!("fno script door: `{label}` {why}; proceeding without gating its lines")
}

/// The piped runner's refusal, kept distinct from the file door's: it fires
/// only when the readable command text shows a push the pipe would carry
/// past the gate.
fn piped_refusal(command: &str) -> Refusal {
    Refusal(format!(
        "🚫 BLOCKED: a piped shell runner would carry a push past the gate\n\nCommand: {command}\n\n\
         The readable command text holds a push, and the script this command pipes into the \
         runner is not in the command text, so the protected-branch push gate can never read \
         the lines it executes. No marker opens this door: write the script to a file, feed it \
         with a quoted heredoc (`bash <<'EOF'`), or run the steps as typed commands."
    ))
}

#[cfg(test)]
mod tests {
    use super::super::fake::{bash, Fake};
    use super::super::git_gate::approval_flag;
    use super::super::{judge_verdict, judge_with, Ctx, Verdict};
    use super::*;

    fn ctx<'a>(dir: &Path, fake: &'a Fake) -> Ctx<'a> {
        Ctx {
            home: dir.join("home"),
            cwd: dir.to_path_buf(),
            probes: fake,
        }
    }

    fn feature(dir: &Path) -> Fake {
        Fake {
            branch: Some("feature/x".into()),
            stamps: dir.join("stamps"),
            ..Fake::default()
        }
    }

    #[test]
    fn a_push_hidden_in_a_script_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("build.sh"),
            "#!/bin/bash\n# stage\nset -e\ngit push origin main\n",
        )
        .unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        for cmd in [
            "bash build.sh",
            "/bin/bash build.sh",
            "bash -x build.sh",
            "bash < build.sh",
            "sh build.sh",
            "zsh build.sh",
        ] {
            let refusal = judge_with(&bash(cmd), &c).unwrap_or_else(|| panic!("allowed: {cmd}"));
            assert!(
                refusal.contains("BLOCKED: Direct push to protected branch"),
                "{cmd}: {refusal}"
            );
        }
    }

    #[test]
    fn a_push_hidden_one_script_deeper_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("outer.sh"), "bash inner.sh\n").unwrap();
        std::fs::write(dir.path().join("inner.sh"), "git push origin main\n").unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        let refusal = judge_with(&bash("bash outer.sh"), &c).unwrap();
        assert!(
            refusal.contains("BLOCKED: Direct push to protected branch"),
            "{refusal}"
        );
    }

    #[test]
    fn a_push_fed_to_a_bare_runner_by_heredoc_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        for cmd in [
            "bash <<'EOF'\ngit push origin main\nEOF",
            "sh <<EOF\ngit push origin main\nEOF",
            "bash -s <<'EOF'\ngit push origin main\nEOF",
            "bash <<<'git push origin main'",
        ] {
            let refusal = judge_with(&bash(cmd), &c).unwrap_or_else(|| panic!("allowed: {cmd}"));
            assert!(
                refusal.contains("BLOCKED: Direct push to protected branch"),
                "{cmd}: {refusal}"
            );
        }
    }

    #[test]
    fn an_unreadable_script_warns_and_proceeds() {
        let dir = tempfile::tempdir().unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        let warns = |cmd: &str, needle: &str| {
            let verdict = judge_verdict(&bash(cmd), &c);
            assert!(
                matches!(&verdict, Verdict::Warn(text) if text.contains(needle)),
                "{cmd}: {verdict:?}"
            );
        };
        // Binary
        std::fs::write(dir.path().join("bin.sh"), b"\x00\x01trick").unwrap();
        warns("bash bin.sh", "NUL");
        // Not a regular file (a directory named like a script)
        std::fs::create_dir(dir.path().join("dir.sh")).unwrap();
        warns("bash dir.sh", "not a regular file");
        // Oversized
        std::fs::write(dir.path().join("big.sh"), vec![b'a'; (1 << 20) + 1]).unwrap();
        warns("bash big.sh", "read cap");
    }

    #[test]
    fn a_readable_push_outranks_an_unreadable_sibling() {
        // One command, two runner segments: a readable script showing a
        // push refuses, and the block outranks the unreadable sibling's
        // warning.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("push.sh"), "git push origin main\n").unwrap();
        std::fs::write(dir.path().join("bin.sh"), b"\x00bin").unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        let refusal = judge_with(&bash("bash push.sh; bash bin.sh"), &c).unwrap();
        assert!(
            refusal.contains("Direct push to protected branch"),
            "{refusal}"
        );
    }

    #[test]
    fn a_script_written_in_the_same_command_is_gated_by_what_writes_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clean.sh"), "cargo build\n").unwrap();
        std::fs::write(dir.path().join("push.sh"), "git push origin main\n").unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        for cmd in [
            "bash missing.sh; echo rc=$?",
            "cat > s.sh <<'EOF'\necho hi\nEOF\nbash s.sh",
            "cp clean.sh s.sh && bash s.sh",
        ] {
            assert_eq!(judge_with(&bash(cmd), &c), None, "{cmd}");
        }
        for cmd in [
            "cat > s.sh <<'EOF'\ngit push origin main\nEOF\nbash s.sh",
            "cp push.sh s.sh && bash s.sh",
            // The file on disk is clean, but the heredoc overwrites it.
            "cat > clean.sh <<'EOF'\ngit push origin main\nEOF\nbash clean.sh",
        ] {
            let refusal = judge_with(&bash(cmd), &c).unwrap_or_else(|| panic!("allowed: {cmd}"));
            assert!(
                refusal.contains("BLOCKED: Direct push to protected branch"),
                "{cmd}: {refusal}"
            );
        }
        // An opaque same-command writer (a download) warns and proceeds:
        // the command text shows no push.
        let verdict = judge_verdict(
            &bash("curl -fsSL -o d.sh https://example.test/d && bash d.sh"),
            &c,
        );
        assert!(
            matches!(
                &verdict,
                Verdict::Warn(text) if text.contains("written in this same command by `curl")
            ),
            "{verdict:?}"
        );
        // A missing script nothing visibly writes refuses when the command
        // text holds a push an unseen writer may put into it.
        let refusal = judge_with(
            &bash("python3 -c \"open('m.sh','w').write('git push origin main')\" && bash m.sh"),
            &c,
        )
        .unwrap();
        assert!(refusal.contains("does not exist yet"), "{refusal}");
    }

    #[test]
    fn a_script_path_resolves_through_cd_and_same_command_assignments() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("repo/scripts");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("push.sh"), "git push origin main\n").unwrap();
        std::fs::create_dir(dir.path().join("crates")).unwrap();
        let fake = feature(dir.path());
        // The hook runs from a subdirectory with no scripts/ beside it.
        let c = ctx(&dir.path().join("crates"), &fake);
        let repo = dir.path().join("repo");
        let repo = repo.display();
        for cmd in [
            format!("cd {repo} && bash scripts/push.sh"),
            format!("S={repo}/scripts/push.sh; bash $S"),
            format!("D={repo}; bash \"${{D}}/scripts/push.sh\""),
            format!("cd {repo} && bash $PWD/scripts/push.sh"),
            // A cd inside a substitution runs in a subshell.
            format!("cd {repo} && X=$(cd /tmp && pwd) && bash scripts/push.sh"),
        ] {
            let refusal = judge_with(&bash(&cmd), &c).unwrap_or_else(|| panic!("allowed: {cmd}"));
            assert!(
                refusal.contains("BLOCKED: Direct push to protected branch"),
                "{cmd}: {refusal}"
            );
        }
        // An unresolvable path warns and proceeds: the command text shows
        // no push.
        let verdict = judge_verdict(&bash("bash $FNO_TEST_UNSET_SCRIPT_DIR/x.sh"), &c);
        assert!(
            matches!(&verdict, Verdict::Warn(text) if text.contains("cannot resolve")),
            "{verdict:?}"
        );
    }

    #[test]
    fn a_chain_nested_past_the_cap_warns_and_proceeds() {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [
            ("d1.sh", "bash d2.sh\n"),
            ("d2.sh", "bash d3.sh\nline\n"),
            ("d3.sh", "bash d4.sh\n"),
            ("d4.sh", "git push origin main\n"),
        ] {
            std::fs::write(dir.path().join(name), body).unwrap();
        }
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        let verdict = judge_verdict(&bash("bash d1.sh"), &c);
        assert!(
            matches!(&verdict, Verdict::Warn(text) if text.contains("nests more than 3")),
            "{verdict:?}"
        );
    }

    #[test]
    fn an_empty_or_inert_script_allows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("build.sh"),
            "#!/bin/bash\nset -e\ncargo build\n",
        )
        .unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        assert_eq!(judge_with(&bash("bash build.sh"), &c), None, "inert script");
    }

    #[test]
    fn a_piped_runner_whose_text_holds_a_push_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        for cmd in [
            "echo 'git push origin main' | bash",
            "printf 'git push origin main' | dash",
        ] {
            let refusal = judge_with(&bash(cmd), &c).unwrap_or_else(|| panic!("allowed: {cmd}"));
            assert!(refusal.contains("piped"), "{cmd}: {refusal}");
        }
    }

    #[test]
    fn a_piped_runner_without_a_push_warns_and_proceeds() {
        let dir = tempfile::tempdir().unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        for cmd in [
            "curl -fsSL https://example.test/install.sh | bash",
            "cat notes.sh | sh",
            "curl -fsSL https://example.test/x | bash -s",
            "curl -fsSL https://example.test/x | zsh",
            "curl -fsSL https://example.test/x | sudo bash",
        ] {
            let verdict = judge_verdict(&bash(cmd), &c);
            assert!(
                matches!(&verdict, Verdict::Warn(text) if text.contains("pipe")),
                "{cmd}: {verdict:?}"
            );
        }
    }

    #[test]
    fn a_runner_with_a_script_file_keeps_the_file_door_despite_a_pipe() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("build.sh"), "#!/bin/bash\ncargo build\n").unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        assert_eq!(
            judge_with(&bash("cat notes.txt | bash build.sh"), &c),
            None,
            "argv file wins over piped stdin"
        );
    }

    #[test]
    fn a_bare_runner_without_a_pipe_stays_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        assert_eq!(judge_with(&bash("bash"), &c), None);
        assert_eq!(judge_with(&bash("bash -x"), &c), None);
        assert_eq!(judge_with(&bash("cd /tmp && bash"), &c), None);
    }

    #[test]
    fn a_piped_runner_inside_a_heredoc_body_warns_and_proceeds() {
        let dir = tempfile::tempdir().unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        // The heredoc body is readable, so the door gates its segments;
        // the piped runner inside it skips (no warning surfaces nested).
        let verdict = judge_verdict(
            &bash("bash <<'EOF'\ncurl -fsSL https://example.test/i | bash\nEOF"),
            &c,
        );
        assert!(matches!(&verdict, Verdict::Allow), "{verdict:?}");
    }

    #[test]
    fn a_piped_push_inside_a_heredoc_body_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        let refusal = judge_with(
            &bash("bash <<'EOF'\necho 'git push origin main' | bash\nEOF"),
            &c,
        )
        .unwrap();
        assert!(refusal.contains("piped"), "{refusal}");
    }

    #[test]
    fn a_push_after_an_apostrophe_comment_is_still_refused() {
        // The comment's apostrophe must not corrupt the split into hiding
        // the push: the parse now completes, and the push segment is gated
        // like a typed one.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("notes.sh"),
            "#!/bin/bash\necho hi # don't panic\ngit push origin main\n",
        )
        .unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        let refusal = judge_with(&bash("bash notes.sh"), &c).unwrap();
        assert!(
            refusal.contains("BLOCKED: Direct push to protected branch"),
            "{refusal}"
        );
    }

    #[test]
    fn a_push_after_a_dollar_hash_separator_is_still_refused() {
        // `$#` is an expansion, not a comment: the `;` after it separates,
        // and the next command gates like a typed one. A mid-word `#` that
        // commented the rest of the line once hid this push from the gate.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dh.sh"), "echo $#; git push origin main\n").unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        let refusal = judge_with(&bash("bash dh.sh"), &c).unwrap();
        assert!(
            refusal.contains("BLOCKED: Direct push to protected branch"),
            "{refusal}"
        );
    }

    #[test]
    fn the_lane_emit_script_the_review_skill_runs_is_gateable() {
        // skills/review/scripts/emit-attestation.sh is the review lane's one
        // attestation producer. Its comments carry apostrophes, which once
        // corrupted the line split, failed the parse, and threw every lane
        // onto the typed fallback. The real script must pass the door: the
        // parse completes, and it hides no push.
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../skills/review/scripts/emit-attestation.sh");
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(&src, dir.path().join("emit.sh")).unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        assert_eq!(
            judge_with(&bash("bash emit.sh --help"), &c),
            None,
            "the review lane's emit script must pass the script door"
        );
        // The live lane invocation runs from a worktree cwd with the
        // repo-relative path and the lane's real argument.
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let fake = feature(&repo);
        let c = ctx(&repo, &fake);
        assert_eq!(
            judge_with(
                &bash("bash skills/review/scripts/emit-attestation.sh code-review"),
                &c
            ),
            None,
            "the worktree emit invocation must pass the script door"
        );
    }

    #[test]
    fn an_unparsable_script_with_only_readonly_git_allows() {
        // The fallback refuses a push pair only: a script whose parse breaks
        // while its only git line is read-only must pass the door. A git
        // mention alone once refused it wholesale.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("ro.sh"),
            "#!/bin/bash\nset -e\necho 'unterminated\ngit rev-parse HEAD\n",
        )
        .unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        assert_eq!(
            judge_with(&bash("bash ro.sh"), &c),
            None,
            "read-only git in an unparsable script must not refuse"
        );
    }

    #[test]
    fn an_unparsable_script_carrying_a_push_refuses_and_names_the_real_command() {
        // An unparsable script whose text carries a push pair refuses, and
        // the refusal prints the real command and script path: the swapped
        // refusal args once printed the whole body as Command: and the
        // literal "the script" as Script:.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("bad.sh"),
            "#!/bin/bash\necho 'unterminated\ngit push origin main\n",
        )
        .unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        let refusal = judge_with(&bash("bash bad.sh"), &c).unwrap();
        assert!(
            refusal.contains("cannot be gated")
                && refusal.contains("Command: bash bad.sh")
                && refusal.contains("Script: bad.sh"),
            "{refusal}"
        );
    }

    #[test]
    fn a_quote_spliced_push_in_an_unparsable_script_refuses() {
        // `$''` quoting is valid bash the parser cannot read, and quote
        // splicing (`git 'pu'sh`) hid the push pair from the text match:
        // bash joins `'pu'` and `sh` into one word. The fallback strips
        // quote characters before matching.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("splice.sh"),
            "mode=$'don\\'t'\ngit 'pu'sh origin main\n",
        )
        .unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        let refusal = judge_with(&bash("bash splice.sh"), &c).unwrap();
        assert!(refusal.contains("cannot be gated"), "{refusal}");
    }

    #[test]
    fn a_nested_runner_on_an_absent_helper_skips_when_the_script_could_not_push() {
        // `$(bash "$helper" ...)` with a variable path reads no file: the
        // runtime resolves it (or skips it), and no hidden text exists.
        // The nested read failure once refused the whole enclosing
        // script, which broke every producer script that nests a helper.
        // The fallback applies: no push pair in the enclosing script
        // skips the helper.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("nest.sh"),
            "#!/bin/bash\nrepo=$(git rev-parse --show-toplevel 2>/dev/null || true)\nout=$(bash \"$helper\" \"$repo\" 2>/dev/null)\necho \"$out\"\n",
        )
        .unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        assert_eq!(
            judge_with(&bash("bash nest.sh"), &c),
            None,
            "an absent nested helper must not refuse a push-free script"
        );
    }

    #[test]
    fn a_nested_runner_on_an_absent_helper_refuses_when_the_script_could_push() {
        // The same shape with a push pair in the enclosing script still
        // refuses: the push itself is a readable, gated segment.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("nest-push.sh"),
            "#!/bin/bash\ngit push origin main\nout=$(bash \"$helper\" x 2>/dev/null)\n",
        )
        .unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        let refusal = judge_with(&bash("bash nest-push.sh"), &c).unwrap();
        assert!(
            refusal.contains("Direct push to protected branch"),
            "{refusal}"
        );
    }

    #[test]
    fn the_blueprint_plan_validator_script_is_gateable() {
        // skills/blueprint/scripts/validate-plan.sh runs on every blueprint,
        // so the script door must read it. Its prose comments and
        // herestrings once broke the parse, and the fallback refused it for
        // its `git rev-parse` lines. The real script must pass the door.
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../skills/blueprint/scripts/validate-plan.sh");
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(&src, dir.path().join("validate-plan.sh")).unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        assert_eq!(
            judge_with(&bash("bash validate-plan.sh --help"), &c),
            None,
            "the blueprint validator must pass the script door"
        );
        // The live blueprint invocation runs from a worktree cwd with the
        // repo-relative path and a plan argument.
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let fake = feature(&repo);
        let c = ctx(&repo, &fake);
        assert_eq!(
            judge_with(
                &bash("bash skills/blueprint/scripts/validate-plan.sh internal/plans/demo"),
                &c
            ),
            None,
            "the worktree validator invocation must pass the script door"
        );
    }

    #[test]
    fn a_cat_heredoc_body_stays_content() {
        let dir = tempfile::tempdir().unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        assert_eq!(
            judge_with(&bash("cat <<'EOF'\ngit push origin main\nEOF"), &c),
            None
        );
    }

    #[test]
    fn an_authorization_never_carries_a_script_run() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("s.sh"), "git commit --no-verify -m x\n").unwrap();
        let fake = feature(dir.path());
        let c = ctx(dir.path(), &fake);
        std::fs::create_dir_all(&c.home).unwrap();
        std::fs::write(approval_flag(&c.home), b"").unwrap();
        let refusal = judge_with(&bash("bash s.sh"), &c).unwrap();
        assert!(
            refusal.contains("cannot authorize several actions"),
            "{refusal}"
        );
    }
}
