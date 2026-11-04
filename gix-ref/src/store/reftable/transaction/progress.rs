//! What a failure to publish the tables of a transaction means for its edits.
//!
//! Every stack is published on its own, one after the other, so a failure after the first stack was published leaves the
//! transaction applied in part. Which edits are done, uncertain or untouched follows from the stack that failed and how.
use crate::transaction::commit_failure::Progress;

/// How publishing a stack failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Failed {
    /// The list of the stack was not replaced, and the tables that were written are gone.
    Cleanly,
    /// The list of the stack may or may not have been replaced.
    Uncertain,
}

/// What became of the transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outcome {
    /// Nothing was published.
    NothingApplied,
    /// The progress of each edit.
    Partial(Vec<Progress>),
}

/// Say what became of the edits after publishing the stack at `failed_at` failed, in the order in which stacks are
/// published. `stack_of_edit` has the position of the stack of every edit in that order, and `published_any` tells if a stack
/// before the failed one replaced its list, as opposed to having nothing to publish.
pub(super) fn outcome(stack_of_edit: &[usize], failed_at: usize, published_any: bool, failure: Failed) -> Outcome {
    if !published_any && failure == Failed::Cleanly {
        return Outcome::NothingApplied;
    }
    Outcome::Partial(
        stack_of_edit
            .iter()
            .map(|stack| match stack.cmp(&failed_at) {
                std::cmp::Ordering::Less => Progress::Applied,
                std::cmp::Ordering::Equal => match failure {
                    Failed::Cleanly => Progress::NotAttempted,
                    Failed::Uncertain => Progress::InDoubt,
                },
                std::cmp::Ordering::Greater => Progress::NotAttempted,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use Progress::{Applied, InDoubt, NotAttempted};

    #[test]
    fn a_clean_failure_of_the_first_stack_changed_nothing() {
        assert_eq!(outcome(&[0, 1, 0], 0, false, Failed::Cleanly), Outcome::NothingApplied);
    }

    #[test]
    fn an_uncertain_failure_is_in_doubt_for_its_stack_only() {
        assert_eq!(
            outcome(&[0, 1, 2, 1], 1, true, Failed::Uncertain),
            Outcome::Partial(vec![Applied, InDoubt, NotAttempted, InDoubt])
        );
        assert_eq!(
            outcome(&[0, 1], 0, false, Failed::Uncertain),
            Outcome::Partial(vec![InDoubt, NotAttempted]),
            "even the first stack may have been published"
        );
    }

    #[test]
    fn a_clean_failure_after_other_stacks_were_published_is_partial_but_not_in_doubt() {
        assert_eq!(
            outcome(&[0, 1, 2], 1, true, Failed::Cleanly),
            Outcome::Partial(vec![Applied, NotAttempted, NotAttempted]),
            "the tables of the failed stack were removed, so retrying it is safe"
        );
    }

    #[test]
    fn a_clean_failure_after_stacks_with_nothing_to_publish_changed_nothing() {
        assert_eq!(
            outcome(&[0, 1], 1, false, Failed::Cleanly),
            Outcome::NothingApplied,
            "a no-op stack before the failed one didn't change anything, so the failure left the repository as it was"
        );
    }
}
