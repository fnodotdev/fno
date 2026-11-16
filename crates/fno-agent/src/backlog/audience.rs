//! Audience: who a node, a question or a decision serves.
//!
//! `--scope internal` is this project's own operation and never ships.
//! `--scope customers` serves every customer of the project.
//! `--scope customer:<persona>` serves one persona, named by a file under
//! `~/.fno/profile/customers/<project.id>/<persona>.md`. No door defaults it,
//! so the author names the audience each time. Where a decision binds our
//! sessions is a different question, called reach (decision_index.rs).

use crate::decision_index::SpecError;
use std::path::PathBuf;

const USAGE: &str = "--scope takes internal (this project's own operation), customers (every customer) or customer:<persona> (one persona)";

/// The directory that holds one project's persona files. Resolved through
/// paths.rs so the `FNO_CUSTOMERS_DIR` override and the state ladder have
/// one home.
pub fn persona_dir(project_id: &str) -> PathBuf {
    crate::paths::persona_customers_dir(project_id)
}

/// Check a typed `--scope` value and return its canonical spelling. A
/// comma-separated list names several audiences (`customer:a,customer:b`);
/// every persona named must have its file. `customers` still means all
/// personas, and mixing it with a persona list refuses.
pub fn parse_audience(raw: Option<&str>, project_id: &str) -> Result<String, SpecError> {
    let Some(raw) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Err(SpecError(format!(
            "--scope is required and has no default, so each record names who it serves. {USAGE}"
        )));
    };
    let values: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .collect();
    if values.len() > 1 && values.iter().any(|v| *v == "customers" || *v == "internal") {
        return Err(SpecError(format!(
            "--scope {raw} mixes a list with customers or internal: a list names specific personas only. {USAGE}"
        )));
    }
    let mut canonical: Vec<String> = Vec::new();
    for value in values {
        match value {
            "internal" | "customers" => canonical.push(value.to_string()),
            "global" => {
                return Err(SpecError(format!(
                    "--scope global is refused: global names every user. {USAGE}. To bind every one of our sessions, pass --reach global"
                )))
            }
            _ => match value.strip_prefix("customer:") {
                Some(persona) if persona_slug_ok(persona) => {
                    let file = persona_dir(project_id).join(format!("{persona}.md"));
                    if file.is_file() {
                        canonical.push(value.to_string());
                    } else {
                        return Err(SpecError(format!(
                            "--scope {value} needs the persona file {}, and it does not exist. Write the persona there, or use --scope customers",
                            file.display()
                        )));
                    }
                }
                Some(_) => {
                    return Err(SpecError(format!(
                        "--scope {value}: a persona is a lowercase slug (letters, digits, hyphen). {USAGE}"
                    )))
                }
                None => {
                    return Err(SpecError(format!(
                        "--scope {value:?} is not an audience. {USAGE}"
                    )))
                }
            },
        }
    }
    Ok(canonical.join(","))
}

/// Every persona slug a stored scope names, in stored order. A plain
/// `internal` or `customers` scope names none.
pub fn personas_in_scope(scope: &str) -> Vec<&str> {
    scope
        .split(',')
        .filter_map(|v| v.trim().strip_prefix("customer:"))
        .filter(|p| persona_slug_ok(p))
        .collect()
}

/// The single scope term the board score uses: the highest-impact audience a
/// stored scope names. A list takes its strongest member, never a sum.
pub fn scope_score_term(scope: &str) -> u32 {
    scope
        .split(',')
        .map(str::trim)
        .map(|v| match v {
            v if v.starts_with("customer:") => 60,
            "customers" => 50,
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

fn persona_slug_ok(slug: &str) -> bool {
    !slug.is_empty()
        && !slug.starts_with('-')
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Check a typed `--job <persona>/<job-slug>` against the record's scope.
/// Required with `customer:<persona>` (the file must carry a
/// `### <job-slug>` heading under a `## Jobs` section), optional with
/// `customers`, refused with `internal`.
pub fn validate_job(scope: &str, jobs: &[String], project_id: &str) -> Result<(), SpecError> {
    let trimmed: Vec<&str> = jobs
        .iter()
        .map(|j| j.trim())
        .filter(|j| !j.is_empty())
        .collect();
    let named_personas = personas_in_scope(scope);
    if named_personas.is_empty() {
        if !trimmed.is_empty() {
            if scope == "internal" {
                return Err(SpecError(
                    "--job is refused with --scope internal: an internal record keeps its free-text reason in the why".to_string(),
                ));
            }
            // `customers` may name jobs for any personas it cares to name.
            return Ok(());
        }
        return Ok(());
    }
    // One job per named persona: each persona file must carry the heading.
    for persona in &named_personas {
        let job = trimmed.iter().find(|j| {
            j.split_once('/')
                .map(|(p, _)| p == *persona)
                .unwrap_or(false)
        });
        let Some(job) = job else {
            return Err(SpecError(format!(
                "--job <persona>/<job-slug> is required for {persona}: a product record for one persona names the job it serves"
            )));
        };
        let Some((job_persona, job_slug)) = job.split_once('/') else {
            return Err(SpecError(format!(
                "--job {job} is not a persona/job pair. The shape is <persona>/<job-slug>, e.g. {persona}/<job-slug>"
            )));
        };
        let file = persona_dir(project_id).join(format!("{job_persona}.md"));
        let text = std::fs::read_to_string(&file).map_err(|_| {
            SpecError(format!(
                "--job {job} needs the persona file {}, and it does not exist",
                file.display()
            ))
        })?;
        let in_jobs = text.lines().any(|l| l.trim() == "## Jobs");
        let heading = text
            .lines()
            .any(|l| l.contains("###") && l.trim().trim_start_matches('#').trim() == job_slug);
        if !(in_jobs && heading) {
            return Err(SpecError(format!(
                "the persona file {} carries no ### {job_slug} heading under a ## Jobs section, so --job {job} names no job it lists",
                file.display()
            )));
        }
    }
    Ok(())
}

/// The one-paragraph summary a session surface prints for a persona: the
/// first non-heading, non-frontmatter line of the persona file.
pub fn persona_summary(persona: &str, project_id: &str) -> Option<String> {
    let file = persona_dir(project_id).join(format!("{persona}.md"));
    let text = std::fs::read_to_string(&file).ok()?;
    let mut in_frontmatter = text.starts_with("---");
    for line in text.lines().skip(1) {
        let trimmed = line.trim();
        if in_frontmatter {
            if trimmed == "---" {
                in_frontmatter = false;
            }
            continue;
        }
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        return Some(trimmed.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The persona dir resolves through the process-global
    /// FNO_CUSTOMERS_DIR, so the tests that set it serialize on this lock;
    /// the default parallel harness would otherwise race the env var.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take the env lock and clear any var a panicked earlier test left
    /// behind; the caller points FNO_CUSTOMERS_DIR at its own temp dir and
    /// removes the var again before returning.
    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("FNO_CUSTOMERS_DIR");
        guard
    }

    /// Point FNO_CUSTOMERS_DIR at a fresh temp dir; the caller holds the
    /// lock the helper returned for the whole test body.
    fn scoped_customers_dir(tag: &str) {
        let dir = std::env::temp_dir().join(tag);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("FNO_CUSTOMERS_DIR", &dir);
    }

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
    fn job_rules_track_the_scope() {
        let _env = lock_env();
        scoped_customers_dir("fno-audience-job-test");
        let project = std::env::temp_dir()
            .join("fno-audience-job-test")
            .join("fno-test-job");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("reader.md"),
            "---\npersona: reader\n---\n\n# Reader\n\n## Jobs\n\n### read-the-docs\n\nGet an answer fast.\n",
        )
        .unwrap();
        // Required with customer:<persona>, the job must exist as a heading.
        let jobs = |vals: &[&str]| -> Vec<String> { vals.iter().map(|s| s.to_string()).collect() };
        assert!(validate_job(
            "customer:reader",
            &jobs(&["reader/read-the-docs"]),
            "fno-test-job"
        )
        .is_ok());
        assert!(validate_job("customer:reader", &[], "fno-test-job")
            .unwrap_err()
            .0
            .contains("is required for"));
        assert!(validate_job(
            "customer:reader",
            &jobs(&["reader/no-such-job"]),
            "fno-test-job"
        )
        .unwrap_err()
        .0
        .contains("carries no ### no-such-job"));
        assert!(validate_job(
            "customer:reader",
            &jobs(&["other/read-the-docs"]),
            "fno-test-job"
        )
        .unwrap_err()
        .0
        .contains("names persona other"));
        assert!(
            validate_job("customer:reader", &jobs(&["bare-slug"]), "fno-test-job")
                .unwrap_err()
                .0
                .contains("persona/job pair")
        );
        // Optional with customers, refused with internal.
        assert!(validate_job("customers", &[], "fno-test-job").is_ok());
        assert!(
            validate_job("internal", &jobs(&["reader/read-the-docs"]), "fno-test-job")
                .unwrap_err()
                .0
                .contains("refused with --scope internal")
        );
        std::env::remove_var("FNO_CUSTOMERS_DIR");
    }

    #[test]
    fn persona_summary_skips_frontmatter_and_headings() {
        let _env = lock_env();
        scoped_customers_dir("fno-audience-summary-test");
        let project = std::env::temp_dir()
            .join("fno-audience-summary-test")
            .join("fno-test-sum");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("reader.md"),
            "---\npersona: reader\n---\n\n# Reader\n\nFirst prose line wins.\n",
        )
        .unwrap();
        assert_eq!(
            persona_summary("reader", "fno-test-sum").as_deref(),
            Some("First prose line wins.")
        );
        assert!(persona_summary("ghost", "fno-test-sum").is_none());
        std::env::remove_var("FNO_CUSTOMERS_DIR");
    }

    #[test]
    fn a_two_persona_scope_parses_and_takes_its_strongest_term() {
        let _env = lock_env();
        scoped_customers_dir("fno-audience-multi-test");
        let project = std::env::temp_dir()
            .join("fno-audience-multi-test")
            .join("fno-test-multi");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("reader.md"), "# Reader\n\nReads.\n").unwrap();
        std::fs::write(project.join("writer.md"), "# Writer\n\nWrites.\n").unwrap();
        assert_eq!(
            parse_audience(Some("customer:reader,customer:writer"), "fno-test-multi").unwrap(),
            "customer:reader,customer:writer"
        );
        assert!(
            parse_audience(Some("customer:reader,customer:ghost"), "fno-test-multi")
                .unwrap_err()
                .0
                .contains("ghost.md")
        );
        assert!(parse_audience(Some("internal,customers"), "fno-test-multi")
            .unwrap_err()
            .0
            .contains("mixes a list"));
        assert_eq!(scope_score_term("customer:reader,customer:writer"), 60);
        assert_eq!(scope_score_term("customers"), 50);
        assert_eq!(scope_score_term("internal"), 0);
        assert_eq!(
            personas_in_scope("customer:reader,customer:writer"),
            vec!["reader", "writer"]
        );
        std::env::remove_var("FNO_CUSTOMERS_DIR");
    }

    #[test]
    fn parse_audience_stores_the_canonical_spelling() {
        let _env = lock_env();
        scoped_customers_dir("fno-audience-canonical-test");
        let project = std::env::temp_dir()
            .join("fno-audience-canonical-test")
            .join("fno-test-canonical");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("r.md"), "# R\n\nReads.\n").unwrap();
        assert_eq!(
            parse_audience(Some("customer:r"), "fno-test-canonical").unwrap(),
            "customer:r"
        );
        assert!(parse_audience(Some("customer:r "), "fno-test-canonical").is_ok());
        std::env::remove_var("FNO_CUSTOMERS_DIR");
    }
}
