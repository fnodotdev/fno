use super::{fold_target, FoldTarget};

#[test]
fn fold_target_names_the_stacked_pr_or_the_reason_there_is_none() {
    // AC4: a merged PR whose base is another PR's branch binds that PR;
    // the default branch, a bare base, or a branchless stack binds none.
    assert_eq!(
        fold_target("feature/side-branch", Some("main"), Some(351)),
        FoldTarget::Bind(351)
    );
    assert_eq!(
        fold_target("main", Some("main"), Some(351)),
        FoldTarget::DefaultBranch
    );
    assert_eq!(
        fold_target("feature/absent", Some("main"), None),
        FoldTarget::NoStack
    );
    assert_eq!(
        fold_target("", Some("main"), Some(351)),
        FoldTarget::UnreadableBase
    );
}
