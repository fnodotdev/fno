//! The one-release `fno-agents` names. After an update lands the renamed
//! `fno-agent` triad, each old name in a bin dir becomes a link to its new
//! sibling, so a plist, hook or shell that still runs `fno-agents` reaches
//! the fresh build instead of a stale binary left beside it.

use std::path::{Path, PathBuf};

/// (old name, new name) for each triad member.
pub(crate) const LEGACY_NAMES: [(&str, &str); 3] = [
    ("fno-agents", "fno-agent"),
    ("fno-agents-daemon", "fno-agent-daemon"),
    ("fno-agents-worker", "fno-agent-worker"),
];

/// Point each old name in `dir` at its new sibling. Where the dir holds only
/// an old-name link into another dir that now carries the new name, the new
/// name is linked there first. Returns the old names it rewrote.
pub(crate) fn retarget(dir: &Path) -> Vec<String> {
    let mut done = Vec::new();
    for (old, new) in LEGACY_NAMES {
        let old_path = dir.join(old);
        let new_path = dir.join(new);
        if !new_path.exists() {
            let sibling = std::fs::read_link(&old_path)
                .ok()
                .map(|t| dir.join(t).with_file_name(new))
                .filter(|s| s.is_file());
            match sibling {
                Some(target) if swap_link(&target, &new_path).is_ok() => {}
                _ => continue,
            }
        }
        if old_path.symlink_metadata().is_err() || same_file(&old_path, &new_path) {
            continue;
        }
        if swap_link(Path::new(new), &old_path).is_ok() {
            done.push(old.to_string());
        }
    }
    done
}

/// Retarget the old names in each of `dirs` and in every PATH dir, and print
/// one line per dir that changed.
pub(crate) fn retarget_all(dirs: &[PathBuf]) {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path_dirs = std::env::split_paths(&path);
    let mut seen: Vec<PathBuf> = Vec::new();
    for dir in dirs.iter().cloned().chain(path_dirs) {
        let key = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let done = retarget(&dir);
        if !done.is_empty() {
            println!(
                "fno update: {} now link to the fno-agent bins in {}",
                done.join(", "),
                dir.display()
            );
        }
    }
}

/// Rewrite footnote's own auto-mode rules in the user's Claude settings when
/// an older wording is installed there: the rules now name `fno agent`. A
/// settings file that never took the rules is left alone.
pub(crate) fn refresh_claude_rules() {
    let path = crate::cli_hooks::user_claude_settings_path();
    let installed =
        std::fs::read_to_string(&path).is_ok_and(|text| text.contains("Footnote State Writes:"));
    if !installed || crate::cli_hooks::claude_auto_mode_allow_status(&path) != Ok(false) {
        return;
    }
    let result = crate::cli_hooks::install_claude_auto_mode_allow(&path);
    if result.changed {
        println!(
            "fno update: rewrote footnote's auto-mode rules in {}",
            path.display()
        );
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// Land `link -> target` as a temp symlink renamed over whatever is there.
fn swap_link(target: &Path, link: &Path) -> std::io::Result<()> {
    let name = link.file_name().unwrap_or_default().to_string_lossy();
    let tmp = link.with_file_name(format!(".{name}.fno-legacy-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp).and_then(|_| std::fs::rename(&tmp, link))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_names_follow_the_new_triad_and_a_link_dir_gains_the_new_name() {
        let tmp = tempfile::tempdir().unwrap();
        let cargo = tmp.path().join("cargo");
        let local = tmp.path().join("local");
        std::fs::create_dir_all(&cargo).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        for (old, new) in LEGACY_NAMES {
            std::fs::write(cargo.join(old), b"stale").unwrap();
            std::fs::write(cargo.join(new), b"fresh").unwrap();
            std::os::unix::fs::symlink(cargo.join(old), local.join(old)).unwrap();
        }
        assert_eq!(retarget(&cargo).len(), 3);
        assert_eq!(retarget(&local).len(), 0, "already reaches the new bytes");
        for (old, new) in LEGACY_NAMES {
            assert_eq!(std::fs::read(cargo.join(old)).unwrap(), b"fresh");
            assert_eq!(std::fs::read(local.join(old)).unwrap(), b"fresh");
            assert_eq!(std::fs::read(local.join(new)).unwrap(), b"fresh");
        }
        // A dir with neither name is left alone.
        assert!(retarget(tmp.path()).is_empty());
    }
}
