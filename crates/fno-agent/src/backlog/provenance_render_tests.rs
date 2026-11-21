use super::*;

#[test]
fn provenance_renders_primary_and_every_additional_pr() {
    // AC1-HP: the primary line carries the repo slug and merge status, and
    // every additional_prs entry renders its own line with status and note.
    let row = json!({
        "pr_number": 289,
        "pr_url": "https://github.com/fnodotdev/fno/pull/289",
        "merge_status": "merged",
        "additional_prs": [
            {"number": 351, "url": "https://github.com/fnodotdev/fno/pull/351", "merge_status": "merged", "note": "folded"},
            {"number": 244, "url": "https://github.com/footnote-sh/fno-web/pull/244", "merge_status": "merged", "note": "docs"},
            {"number": 7}
        ]
    });
    assert_eq!(
        render_pr_lines(&row),
        vec![
            "  pr: #289 https://github.com/fnodotdev/fno/pull/289 (fnodotdev/fno, merged)",
            "  pr: #351 https://github.com/fnodotdev/fno/pull/351 (fnodotdev/fno, merged, note: folded)",
            "  pr: #244 https://github.com/footnote-sh/fno-web/pull/244 (footnote-sh/fno-web, merged, note: docs)",
            "  pr: #7",
        ]
    );
}
