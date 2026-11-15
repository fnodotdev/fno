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
                let file = persona_dir(project_id).join(format!("{persona}.md"));
                if file.is_file() {
                    Ok(raw.to_string())
                } else {
                    Err(SpecError(format!(
                        "--scope {raw} needs the persona file {}, and it does not exist. Write the persona there, or use --scope customers",
                        file.display()
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

/// Check a typed `--job <persona>/<job-slug>` against the record's scope.
/// Required with `customer:<persona>` (the file must carry a
/// `### <job-slug>` heading under a `## Jobs` section), optional with
/// `customers`, refused with `internal`.
pub fn validate_job(scope: &str, job: Option<&str>, project_id: &str) -> Result<(), SpecError> {
    let trimmed = job.map(str::trim).filter(|j| !j.is_empty());
    match scope.strip_prefix("customer:") {
        Some(persona) => {
            let Some(job) = trimmed else {
                return Err(SpecError(format!(
                    "--job <persona>/<job-slug> is required with --scope customer:{persona}: a product record for one persona names the job it serves"
                )));
            };
            let Some((job_persona, job_slug)) = job.split_once('/') else {
                return Err(SpecError(format!(
                    "--job {job} is not a persona/job pair. The shape is <persona>/<job-slug>, e.g. {persona}/<job-slug>"
                )));
            };
            if job_persona != persona {
                return Err(SpecError(format!(
                    "--job {job} names persona {job_persona}, but the scope is customer:{persona}"
                )));
            }
            let file = persona_dir(project_id).join(format!("{persona}.md"));
            let text = std::fs::read_to_string(&file).map_err(|_| {
                SpecError(format!(
                    "--job {job} needs the persona file {}, and it does not exist",
                    file.display()
                ))
            })?;
            let in_jobs = text.lines().any(|l| l.trim() == "## Jobs");
            let heading = text
                .lines()
                .any(|l| l.trim().trim_start_matches('#').trim() == job_slug && l.contains("###"));
            if !(in_jobs && heading) {
                return Err(SpecError(format!(
                    "the persona file {} carries no ### {job_slug} heading under a ## Jobs section, so --job {job} names no job it lists",
                    file.display()
                )));
            }
            Ok(())
        }
        _ if scope == "internal" && trimmed.is_some() => Err(SpecError(
            "--job is refused with --scope internal: an internal record keeps its free-text reason in the why".to_string(),
        )),
        _ => Ok(()),
    }
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
        let dir = std::env::temp_dir().join("fno-audience-job-test");
        let project = dir.join("fno-test-job");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("reader.md"),
            "---\npersona: reader\n---\n\n# Reader\n\n## Jobs\n\n### read-the-docs\n\nGet an answer fast.\n",
        )
        .unwrap();
        std::env::set_var("FNO_CUSTOMERS_DIR", &dir);
        // Required with customer:<persona>, the job must exist as a heading.
        assert!(validate_job(
            "customer:reader",
            Some("reader/read-the-docs"),
            "fno-test-job"
        )
        .is_ok());
        assert!(validate_job("customer:reader", None, "fno-test-job")
            .unwrap_err()
            .0
            .contains("is required with"));
        assert!(validate_job(
            "customer:reader",
            Some("reader/no-such-job"),
            "fno-test-job"
        )
        .unwrap_err()
        .0
        .contains("carries no ### no-such-job"));
        assert!(validate_job(
            "customer:reader",
            Some("other/read-the-docs"),
            "fno-test-job"
        )
        .unwrap_err()
        .0
        .contains("names persona other"));
        assert!(
            validate_job("customer:reader", Some("bare-slug"), "fno-test-job")
                .unwrap_err()
                .0
                .contains("persona/job pair")
        );
        // Optional with customers, refused with internal.
        assert!(validate_job("customers", None, "fno-test-job").is_ok());
        assert!(
            validate_job("internal", Some("reader/read-the-docs"), "fno-test-job")
                .unwrap_err()
                .0
                .contains("refused with --scope internal")
        );
        std::env::remove_var("FNO_CUSTOMERS_DIR");
    }

    #[test]
    fn persona_summary_skips_frontmatter_and_headings() {
        let dir = std::env::temp_dir().join("fno-audience-summary-test");
        let project = dir.join("fno-test-sum");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("reader.md"),
            "---\npersona: reader\n---\n\n# Reader\n\nFirst prose line wins.\n",
        )
        .unwrap();
        std::env::set_var("FNO_CUSTOMERS_DIR", &dir);
        assert_eq!(
            persona_summary("reader", "fno-test-sum").as_deref(),
            Some("First prose line wins.")
        );
        assert!(persona_summary("ghost", "fno-test-sum").is_none());
        std::env::remove_var("FNO_CUSTOMERS_DIR");
    }
}
