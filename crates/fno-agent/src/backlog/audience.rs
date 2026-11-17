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
    // One persona once lived flat at the profile root; its file migrates
    // into the project's customers directory with the rest.
    if let Some(profile) = dir.parent() {
        migrate_flat_persona(profile, &dir, persona);
    }
    migrate_flat_persona(&dir, &dir, persona);
    let file = resolve_persona_file(&dir, persona);
    normalize_persona_frontmatter(&file);
    file
}

/// Move one flat persona file into the D3440 layout under the project's
/// directory. Best effort: a refused move leaves the flat file in place
/// and the fallback reads it. A symlink never moves: it is a compat link
/// a reader installed, not live state. A persona that already has the
/// laid-out file never moves: its flat file is the older copy or the
/// compat link.
fn migrate_flat_persona(flat_dir: &std::path::Path, project_dir: &std::path::Path, persona: &str) {
    let flat = flat_dir.join(format!("{persona}.md"));
    let laid = project_dir.join(persona).join("PERSONA.md");
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

/// Rewrite a persona's frontmatter to the Agent Skills shape (lead ruling
/// 2026-11-17): top-level `name` and `description`, the fno fields under
/// `metadata`. Old flat keys are accepted for one release and normalized
/// here: the prose status and confidence collapse to their one-word
/// contract values, the confidence prose becomes the evidence line, and a
/// drafting node id in the old status prose becomes `metadata.source`.
/// The description is the first sentence of Who they are. Idempotent:
/// frontmatter that already carries `metadata:` passes through untouched.
fn normalize_persona_frontmatter(laid: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(laid) else {
        return;
    };
    let Some(idx) = text[4..].find("\n---\n") else {
        return;
    };
    let content_end = 4 + idx;
    let rest = &text[content_end + 5..];
    let content = &text[4..content_end];
    if content.contains("metadata:") {
        return;
    }
    let mut keys: Vec<(String, String)> = Vec::new();
    for line in content.lines() {
        if let Some((k, v)) = line.split_once(':') {
            keys.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let get = |name: &str| keys.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let status_prose = get("status").unwrap_or_default();
    if status_prose.is_empty() {
        return;
    }
    let persona = get("persona").unwrap_or_else(|| {
        laid.parent()
            .expect("persona file has a parent")
            .file_name()
            .expect("persona dir has a name")
            .to_string_lossy()
            .to_string()
    });
    let status = if status_prose.contains("approved") && !status_prose.contains("awaiting") {
        "approved"
    } else if status_prose.contains("retired") {
        "retired"
    } else {
        "draft"
    };
    let confidence_prose = get("confidence").unwrap_or_default();
    let confidence = if confidence_prose.contains("interviewed") {
        "interviewed"
    } else if confidence_prose.contains("measured") {
        "measured"
    } else {
        "inferred"
    };
    let source = keys.iter().find_map(|(_, v)| {
        let i = v.find("x-")?;
        let token: String = v[i..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        Some(token)
    });
    let description =
        first_who_sentence(rest).unwrap_or_else(|| format!("The {persona} customer persona."));
    let mut out = String::from("---\n");
    out.push_str(&format!("name: {persona}\n"));
    out.push_str(&format!("description: {description}\n"));
    out.push_str("metadata:\n");
    if let Some(project) = get("project") {
        out.push_str(&format!("  project: \"{project}\"\n"));
    }
    out.push_str(&format!("  status: \"{status}\"\n"));
    if status == "approved" {
        if let Some(date) = get("approved") {
            out.push_str(&format!("  approved: \"{date}\"\n"));
        }
    }
    out.push_str(&format!("  confidence: \"{confidence}\"\n"));
    if !confidence_prose.is_empty() {
        out.push_str(&format!("  evidence: \"{confidence_prose}\"\n"));
    }
    if let Some(source) = source {
        out.push_str(&format!("  source: \"{source}\"\n"));
    }
    out.push_str("---\n");
    out.push_str(rest);
    let _ = std::fs::write(laid, out);
}

/// The frontmatter `name` of a laid-out persona, when it differs from the
/// folder the file lives in. The agentskills spec pins the name to the
/// folder, and the reader rejects a drift.
fn persona_name_mismatch(file: &std::path::Path, persona: &str) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    if !text.starts_with("---\n") {
        return None;
    }
    let content = &text[4..text[4..].find("\n---\n")? + 4];
    let line = content.lines().find(|l| l.starts_with("name:"))?;
    let named = line.split_once(':')?.1.trim();
    (named != persona).then(|| named.to_string())
}

/// The first sentence of the Who-they-are section: the one line the
/// skills-shape `description` needs, read from the body the record
/// already carries.
fn first_who_sentence(body: &str) -> Option<String> {
    let section = body.split("## Who they are").nth(1)?;
    let line = section
        .lines()
        .find(|l| !l.trim().is_empty() && !l.trim().starts_with('#'))?;
    let sentence = line.split(". ").next().unwrap_or(line);
    Some(sentence.trim().to_string())
}
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
                if let Some(named) = persona_name_mismatch(&file, persona) {
                    Err(SpecError(format!(
                        "--scope {raw}: {} names '{named}' but lives in the folder for '{persona}', and the reader rejects a name that differs from its folder",
                        file.display()
                    )))
                } else if file.is_file() {
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
    // The agentskills spec caps a name at 64 chars.
    !slug.is_empty()
        && slug.len() <= 64
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
        migrate_flat_persona(&dir, &dir, "alpha");
        assert!(dir.join("alpha").join("PERSONA.md").is_file());
        assert!(!dir.join("alpha.md").exists());
        // A compat symlink stays exactly where its reader put it.
        let laid = dir.join("beta").join("PERSONA.md");
        std::fs::create_dir_all(laid.parent().unwrap()).unwrap();
        std::fs::write(&laid, "laid beta").unwrap();
        std::os::unix::fs::symlink(&laid, dir.join("beta.md")).unwrap();
        migrate_flat_persona(&dir, &dir, "beta");
        assert!(dir
            .join("beta.md")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        // A persona already laid out never moves its regular flat sibling.
        let laid = dir.join("delta").join("PERSONA.md");
        std::fs::create_dir_all(laid.parent().unwrap()).unwrap();
        std::fs::write(&laid, "laid delta").unwrap();
        std::fs::write(dir.join("delta.md"), "flat delta").unwrap();
        migrate_flat_persona(&dir, &dir, "delta");
        assert!(dir.join("delta.md").is_file());
        // The operator persona's flat file lives at the profile root and
        // migrates into the project's customers directory with the rest.
        let root = dir.join("profile");
        let proj = root.join("customers").join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(root.join("fleet-operator.md"), "operator").unwrap();
        migrate_flat_persona(&root, &proj, "fleet-operator");
        assert!(proj.join("fleet-operator").join("PERSONA.md").is_file());
        assert!(!root.join("fleet-operator.md").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn flat_frontmatter_normalizes_to_the_skills_shape_once() {
        let dir = std::env::temp_dir().join(format!("fno-audience-fm-{}", std::process::id()));
        let laid = dir.join("solo-founder").join("PERSONA.md");
        std::fs::create_dir_all(laid.parent().unwrap()).unwrap();
        std::fs::write(
            &laid,
            "---\npersona: solo-founder\nproject: fno\nstatus: hypothesis, draft awaiting user approval (x-cbcf)\nconfidence: no outside user interviewed yet; inferred from the launch plan\n---\n\n# Solo founder\n\n## Who they are\nOne person building a product, with no engineering team behind them. Technical enough to run a terminal.\n\n## Jobs\n\n### turn-an-idea\n\nTurn an idea into a shipped feature.\n",
        )
        .unwrap();
        normalize_persona_frontmatter(&laid);
        let out = std::fs::read_to_string(&laid).unwrap();
        assert!(out.contains("name: solo-founder"), "{out}");
        assert!(
            out.contains(
                "description: One person building a product, with no engineering team behind them."
            ),
            "{out}"
        );
        assert!(out.contains("  project: \"fno\""), "{out}");
        assert!(out.contains("  status: \"draft\""), "{out}");
        assert!(!out.contains("approved:"), "{out}");
        assert!(out.contains("  confidence: \"inferred\""), "{out}");
        assert!(
            out.contains(
                "  evidence: \"no outside user interviewed yet; inferred from the launch plan\""
            ),
            "{out}"
        );
        assert!(out.contains("  source: \"x-cbcf\""), "{out}");
        assert!(out.contains("## Jobs"), "{out}");
        let before = out.clone();
        normalize_persona_frontmatter(&laid);
        assert_eq!(std::fs::read_to_string(&laid).unwrap(), before);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
