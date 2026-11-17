//! Audience: who a node, a question or a decision serves.
//!
//! `--scope internal` is this project's own operation and never ships.
//! `--scope customers` serves every customer of the project.
//! `--scope customer:<persona>` serves one persona, named by a PERSONA.md
//! under `~/.fno/profile/customers/<project.id>/<persona>/`. A flat
//! `<persona>.md` beside the directory still reads (the pre-layout
//! fallback). No door defaults it, so the author names the audience each
//! time. Where a decision binds our sessions is a different question,
//! called reach (decision_index.rs).

use crate::decision_index::SpecError;
use std::path::PathBuf;

const USAGE: &str = "--scope takes internal (this project's own operation), customers (every customer) or customer:<persona> (one persona)";

/// The directory that holds one project's persona directories.
pub fn persona_dir(project_id: &str) -> PathBuf {
    let root = std::env::var_os("FNO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".fno")))
        .unwrap_or_else(|| PathBuf::from(".fno"));
    root.join("profile").join("customers").join(project_id)
}

/// The file that carries one persona: the D3440 layout reads
/// `<persona>/PERSONA.md` first and falls back to the flat `<persona>.md`
/// the pre-layout files still use. The first lookup after upgrade migrates
/// a flat regular file in place, so live state never waits on a manual
/// move (a manual move outran the installed reader once and broke persona
/// lookups on this machine).
fn persona_file(project_id: &str, persona: &str) -> PathBuf {
    let dir = persona_dir(project_id);
    migrate_flat_persona(&dir, persona);
    resolve_persona_file(&dir, persona)
}

/// Move one flat persona file into the D3440 layout. Best effort: a
/// refused move leaves the flat file in place and the fallback reads it.
/// A symlink never moves: it is a compat link a reader installed, not
/// live state. A persona that already has the laid-out file never moves:
/// its flat file is the older copy or the compat link.
fn migrate_flat_persona(dir: &std::path::Path, persona: &str) {
    let flat = dir.join(format!("{persona}.md"));
    let laid = dir.join(persona).join("PERSONA.md");
    let is_symlink = flat
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(true);
    if !flat.is_file() || is_symlink || laid.exists() {
        return;
    }
    if std::fs::create_dir_all(laid.parent().expect("persona dir has a parent")).is_err() {
        return;
    }
    let _ = std::fs::rename(&flat, &laid);
}

/// Pure resolution against one project's persona directory, so tests pin
/// the layout preference without touching `HOME`.
fn resolve_persona_file(dir: &std::path::Path, persona: &str) -> PathBuf {
    let laid_out = dir.join(persona).join("PERSONA.md");
    if laid_out.is_file() {
        laid_out
    } else {
        dir.join(format!("{persona}.md"))
    }
}

/// Check a typed `--scope` value and return its canonical spelling.
/// `project_id` names the persona directory for `customer:<persona>`.
pub fn parse_audience(raw: Option<&str>, project_id: &str) -> Result<String, SpecError> {
    let Some(raw) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Err(SpecError(format!(
            "--scope is required and has no default, so each record names who it serves. {USAGE}"
        )));
    };
    match raw {
        "internal" | "customers" => Ok(raw.to_string()),
        "global" => Err(SpecError(format!(
            "--scope global is refused: global names every user. {USAGE}. To bind every one of our sessions, pass --reach global"
        ))),
        _ => match raw.strip_prefix("customer:") {
            Some(persona) if persona_slug_ok(persona) => {
                let file = persona_file(project_id, persona);
                if file.is_file() {
                    Ok(raw.to_string())
                } else {
                    Err(SpecError(format!(
                        "--scope {raw} needs the persona file {}, and it does not exist. Write the persona at {} (a flat <persona>.md beside it still reads), or use --scope customers",
                        file.display(),
                        persona_dir(project_id).join(persona).join("PERSONA.md").display()
                    )))
                }
            }
            Some(_) => Err(SpecError(format!(
                "--scope {raw}: a persona is a lowercase slug (letters, digits, hyphen). {USAGE}"
            ))),
            None => Err(SpecError(format!("--scope {raw:?} is not an audience. {USAGE}"))),
        },
    }
}

fn persona_slug_ok(slug: &str) -> bool {
    !slug.is_empty()
        && !slug.starts_with('-')
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audience_has_no_default_and_refuses_global() {
        assert!(parse_audience(None, "fno")
            .unwrap_err()
            .0
            .contains("no default"));
        assert!(parse_audience(Some("global"), "fno")
            .unwrap_err()
            .0
            .contains("--reach global"));
        assert_eq!(parse_audience(Some("internal"), "fno").unwrap(), "internal");
        assert_eq!(
            parse_audience(Some("customers"), "fno").unwrap(),
            "customers"
        );
    }

    #[test]
    fn a_missing_persona_file_names_its_path() {
        let err = parse_audience(Some("customer:ghost"), "fno-test-none")
            .unwrap_err()
            .0;
        assert!(
            err.contains("profile/customers/fno-test-none/ghost.md"),
            "{err}"
        );
    }

    #[test]
    fn the_laid_out_persona_md_wins_over_the_flat_file() {
        let dir = std::env::temp_dir().join(format!("fno-audience-{}", std::process::id()));
        let laid = dir.join("solo-founder").join("PERSONA.md");
        std::fs::create_dir_all(laid.parent().unwrap()).unwrap();
        std::fs::write(&laid, "laid out").unwrap();
        std::fs::write(dir.join("solo-founder.md"), "flat").unwrap();
        assert_eq!(resolve_persona_file(&dir, "solo-founder"), laid);
        std::fs::remove_file(&laid).unwrap();
        assert_eq!(
            resolve_persona_file(&dir, "solo-founder"),
            dir.join("solo-founder.md")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_first_lookup_migrates_a_flat_regular_file_and_spares_a_symlink() {
        let dir = std::env::temp_dir().join(format!("fno-audience-mig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A flat regular file moves on migration; the laid-out file wins.
        std::fs::write(dir.join("alpha.md"), "flat alpha").unwrap();
        migrate_flat_persona(&dir, "alpha");
        assert!(dir.join("alpha").join("PERSONA.md").is_file());
        assert!(!dir.join("alpha.md").exists());
        // A compat symlink stays exactly where its reader put it.
        let laid = dir.join("beta").join("PERSONA.md");
        std::fs::create_dir_all(laid.parent().unwrap()).unwrap();
        std::fs::write(&laid, "laid beta").unwrap();
        std::os::unix::fs::symlink(&laid, dir.join("beta.md")).unwrap();
        migrate_flat_persona(&dir, "beta");
        assert!(dir
            .join("beta.md")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        // A persona already laid out never moves its flat sibling.
        std::fs::write(dir.join("beta.md2"), "").unwrap();
        std::fs::rename(dir.join("beta.md2"), dir.join("gamma.md")).unwrap();
        migrate_flat_persona(&dir, "beta");
        assert!(!dir.join("beta").join("beta.md").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
