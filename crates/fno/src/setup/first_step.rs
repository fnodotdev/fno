//! The line that tells a new user what to run next, read from the folder.
//! Every setup exit calls [`for_repo`]; the shape of the repo picks the line,
//! so the user is never sent to a command that refuses in their folder.

use crate::checks::repo_lint::git_capture;
use serde_json::Value;
use std::path::Path;

const TARGET: &str = "/fno:target \"<your first feature>\"";

/// The next step for the repo at `root`. Shapes are tested in order; the
/// first match wins. `ci_declared_none` is the `ci.declared_none` setting.
pub(crate) fn first_step(root: &Path, ci_declared_none: bool) -> String {
    if !root.join(".git").exists() {
        return format!(
            "git init, then gh repo create --source . --private --push, then {TARGET}. \
             With only an idea and no code yet, start with /fno:talk \"<your idea>\"."
        );
    }
    let origin = git_capture(root, &["remote", "get-url", "origin"]).unwrap_or_default();
    if !origin.contains("github.com") {
        return format!(
            "gh repo create --source . --private --push (target opens a PR, so it needs a \
             GitHub remote), then {TARGET}"
        );
    }
    if !ci_declared_none && !has_workflow(root) {
        return format!(
            "add a GitHub Actions workflow under .github/workflows, or run \
             fno config set ci.declared_none true if this repo has no CI; then {TARGET}"
        );
    }
    TARGET.to_string()
}

/// The first-step line for the current repo and settings.
pub(crate) fn for_repo() -> String {
    let root = super::setup_run::repo_root();
    let ci_declared_none =
        crate::config_doctor::Settings::load().get("ci.declared_none") == Value::Bool(true);
    first_step(&root, ci_declared_none)
}

fn has_workflow(root: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(root.join(".github/workflows")) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        name.ends_with(".yml") || name.ends_with(".yaml")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        assert!(Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success());
    }

    #[test]
    fn first_step_reads_the_folder_shape() {
        let empty = tempfile::tempdir().unwrap();
        assert!(first_step(empty.path(), false).starts_with("git init"));
        assert!(first_step(empty.path(), false).contains("/fno:talk"));

        let no_remote = tempfile::tempdir().unwrap();
        git(no_remote.path(), &["init", "-q"]);
        assert!(first_step(no_remote.path(), false)
            .starts_with("gh repo create --source . --private --push"));

        let gh = tempfile::tempdir().unwrap();
        git(gh.path(), &["init", "-q"]);
        git(
            gh.path(),
            &["remote", "add", "origin", "https://github.com/o/r.git"],
        );
        assert!(first_step(gh.path(), false).contains("fno config set ci.declared_none true"));
        assert_eq!(first_step(gh.path(), true), TARGET);

        std::fs::create_dir_all(gh.path().join(".github/workflows")).unwrap();
        std::fs::write(gh.path().join(".github/workflows/ci.yml"), "name: ci\n").unwrap();
        assert_eq!(first_step(gh.path(), false), TARGET);

        // A `.git` that git cannot read: the probe returns None, which is the no-remote line.
        let broken = tempfile::tempdir().unwrap();
        std::fs::write(broken.path().join(".git"), "not a repo\n").unwrap();
        assert!(first_step(broken.path(), false).starts_with("gh repo create"));
    }
}
