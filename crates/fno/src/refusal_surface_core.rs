//! The one owner of the refusal filing surface rule: which surface a
//! repeating (command, signature) refusal pair points at. Vendored
//! byte-identical into crates/fno-agents/src/eval/refusal_surface_owner.rs; the
//! parity test pins the equality. Edit here, then copy - never fork. No
//! `crate::` references: the file splices into either host crate.

/// The filing surface one refusal pair names, from stored row fields only,
/// so the report and the filer answer identically. `bucket` is the rows'
/// refusal_bucket; `command` the command key; `signature` the pair's
/// signature; `skills` one entry per row (empty string when the row named
/// none). Order matters: the first rule that fits wins.
pub fn refusal_surface(
    bucket: Option<&str>,
    command: Option<&str>,
    signature: &str,
    skills: &[String],
) -> String {
    let command_names_fno = command.is_some_and(|c| {
        c == "fno" || c.starts_with("fno ") || c == "fno-agents" || c.starts_with("fno-agents ")
    });
    match bucket {
        Some(b)
            if matches!(
                b,
                "usage_error" | "style_lint" | "fno_guard" | "gate_refusal"
            ) && command_names_fno =>
        {
            format!("help:{}", command.unwrap_or_default())
        }
        Some("hook") => match dominant_skill(skills) {
            Some(skill) => format!("skill:{skill}"),
            None => match signature.strip_prefix("hook:").filter(|t| !t.is_empty()) {
                Some(tag) => format!("gate:{tag}"),
                None => "unresolved".to_string(),
            },
        },
        Some("permission") => match dominant_skill(skills) {
            Some(skill) => format!("skill:{skill}"),
            None => "instructions:permissions".to_string(),
        },
        Some("tool_use_error") => format!("instructions:{}", command.unwrap_or("tool")),
        _ => "report-only".to_string(),
    }
}

/// The skill named on half or more of the rows; ties break to the name
/// first, so the answer is deterministic. `None` when none holds half.
fn dominant_skill(skills: &[String]) -> Option<&str> {
    if skills.is_empty() {
        return None;
    }
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for skill in skills {
        if skill.is_empty() {
            continue;
        }
        match counts.iter_mut().find(|(s, _)| *s == skill.as_str()) {
            Some(slot) => slot.1 += 1,
            None => counts.push((skill.as_str(), 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    counts
        .first()
        .filter(|(_, n)| n * 2 >= skills.len())
        .map(|(s, _)| *s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_rules_answer_in_order() {
        let one = |s: &str| vec![s.to_string()];
        assert_eq!(
            refusal_surface(Some("style_lint"), Some("fno agents mail send"), "", &[]),
            "help:fno agents mail send"
        );
        assert_eq!(
            refusal_surface(Some("usage_error"), Some("gh pr view"), "", &[]),
            "report-only",
            "a non-fno command is nobody's surface to fix"
        );
        assert_eq!(
            refusal_surface(
                Some("hook"),
                Some("fno mail send"),
                "hook:graphql reserve",
                &[]
            ),
            "gate:graphql reserve"
        );
        assert_eq!(
            refusal_surface(
                Some("hook"),
                Some("fno mail send"),
                "hook:graphql reserve",
                &one("fno:review")
            ),
            "skill:fno:review",
            "a dominant active skill outranks the gate tag"
        );
        assert_eq!(
            refusal_surface(
                Some("permission"),
                None,
                "",
                &["".to_string(), "".to_string()]
            ),
            "instructions:permissions"
        );
        assert_eq!(
            refusal_surface(Some("tool_use_error"), Some("Grep"), "", &[]),
            "instructions:Grep"
        );
        assert_eq!(
            refusal_surface(None, Some("fno mail send"), "", &[]),
            "report-only"
        );
        assert_eq!(
            refusal_surface(Some("hook"), None, "no tag here", &[]),
            "unresolved"
        );
        // Half: 2 of 3 rows hold the skill, so it wins; 1 of 3 does not.
        let skills = vec![
            "fno:review".to_string(),
            "fno:review".to_string(),
            String::new(),
        ];
        assert_eq!(
            refusal_surface(Some("permission"), None, "", &skills),
            "skill:fno:review"
        );
    }
}
