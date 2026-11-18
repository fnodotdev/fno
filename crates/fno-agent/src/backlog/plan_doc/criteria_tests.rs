use super::*;

fn codes(criteria: &[Criterion]) -> Vec<&str> {
    criteria.iter().map(|c| c.code.as_str()).collect()
}

fn kinds(criteria: &[Criterion]) -> Vec<&str> {
    criteria.iter().map(|c| c.kind.as_str()).collect()
}

#[test]
fn an_empty_section_compiles_to_nothing_and_unshaped_prose_is_refused_with_guidance() {
    assert!(compile_criteria("", "Acceptance Criteria")
        .unwrap()
        .is_empty());
    assert!(compile_criteria("   \n\n  \n", "Acceptance Criteria")
        .unwrap()
        .is_empty());

    let err = compile_criteria(
        "This is just prose sentences with no criterion shape.\nMore prose here.\n",
        "Acceptance Criteria",
    )
    .unwrap_err();
    assert!(err.0.contains("Given/When/Then"), "{err}");
    assert!(
        err.0.starts_with("Acceptance Criteria has content"),
        "{err}"
    );
}

#[test]
fn legacy_bold_codes_keep_tag_suffix_metadata_drop_bare_markers_and_accept_letter_suffixes() {
    let tagged = compile_criteria(
        "**AC1-HP (api):** Given valid input, the module returns the result.\n",
        "Acceptance Criteria",
    )
    .unwrap();
    assert_eq!(codes(&tagged), ["AC1-HP"]);
    assert_eq!(kinds(&tagged), ["HP"]);
    assert_eq!(
        tagged[0].text,
        "Given valid input, the module returns the result."
    );

    let bare = compile_criteria(
        "**AC1-HP:** Real behavior.\n**AC2-ERR:**\n",
        "Acceptance Criteria",
    )
    .unwrap();
    assert_eq!(codes(&bare), ["AC1-HP"]);
    assert_eq!(bare[0].text, "Real behavior.");

    let lettered = compile_criteria(
        "**AC3-HP:** Base criterion.\n\n**AC3b-HP:** Sub-criterion.\n\n**AC7a:** A bare letter-suffixed criterion.\n",
        "Acceptance Criteria",
    )
    .unwrap();
    assert_eq!(codes(&lettered), ["AC3-HP", "AC3b-HP", "AC7a"]);
    assert_eq!(kinds(&lettered), ["HP", "HP", "GENERAL"]);
    for prefix in [
        "- **AC9-EDGE:**",
        "- [ ] **AC9-EDGE:**",
        "- [x] AC9-EDGE:",
        "1. **AC9-EDGE:**",
    ] {
        let criteria = compile_criteria(
            &format!("{prefix} Preserve the named requirement."),
            "Acceptance Criteria",
        )
        .unwrap();
        assert_eq!(codes(&criteria), ["AC9-EDGE"]);
        assert_eq!(kinds(&criteria), ["EDGE"]);
        assert_eq!(
            criteria.first().unwrap().text,
            "Preserve the named requirement."
        );
    }
}

#[test]
fn a_duplicate_explicit_code_is_refused_naming_both_lines() {
    let err = compile_criteria(
        "**AC1-HP:** First use.\n\n**AC1-HP:** Second use of the same code.\n",
        "Acceptance Criteria",
    )
    .unwrap_err();
    assert!(err.0.contains("AC1-HP"), "{err}");
    assert!(err.0.contains("first at line 1"), "{err}");
    assert!(err.0.contains("again at line 3"), "{err}");
}
