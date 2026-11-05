//! Compare what the checker finds with what `unpack-objects --strict` and `index-pack --strict` of Git find.
mod corpus;
mod gitmodules;
mod oracle;

use gix_error::TestResult;
use gix_fsck::{Checker, Options};
use oracle::{Base, Case, Compare, Driver, checker_observed, differences};

/// Cases whose outcome depends on objects other than the one that is checked, which Git words differently per program.
fn is_about_links(case: &Case) -> bool {
    [
        "missing",
        "mismatch",
        "is-a-blob",
        "-is-tree",
        "entry-is",
        "of-blob",
        "of-tag",
        "gitlink",
        "type-mismatch",
        "tree-link",
        "same-id",
    ]
    .iter()
    .any(|needle| case.name.contains(needle))
}

/// How much of Git's output to compare for `case`. The text of what is said when linking objects of different kinds depends on
/// the objects Git saw before, which is not something that is observable with a single object.
fn compare(case: &Case) -> Compare {
    if case.name.contains("same-id") {
        Compare::Verdict
    } else if is_about_links(case) {
        Compare::WithoutFatal
    } else {
        Compare::Full
    }
}

/// Like [`compare()`], but without what is said when giving up as `index-pack` words that differently.
fn compare_index_pack(case: &Case) -> Compare {
    // The walk over the links of a tree that can't be decoded prints the error again, but only
    // if it gets that far, which depends on the order in which `index-pack` checks the links.
    if compare(case).is_verdict() || case.name.contains("many-skipped") {
        Compare::Verdict
    } else {
        Compare::WithoutFatal
    }
}

fn all_cases(base: &Base) -> Vec<Case> {
    let mut cases = corpus::commits(base);
    cases.extend(corpus::tags(base));
    cases.extend(corpus::trees(base));
    cases
}

fn assert_no_differences(differences: Vec<String>, total: usize) {
    assert!(
        differences.is_empty(),
        "{} of {total} cases differ from Git:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

#[test]
fn strict_check_of_one_object_matches_unpack_objects() -> TestResult {
    let base = Base::new()?;
    let cases = all_cases(&base);
    let problems = differences(&base, &cases, Driver::UnpackObjects, &compare)?;
    assert_no_differences(problems, cases.len());
    Ok(())
}

#[test]
fn strict_check_of_one_object_matches_index_pack() -> TestResult {
    let base = Base::new()?;
    let cases = all_cases(&base);
    let problems = differences(&base, &cases, Driver::IndexPack, &compare_index_pack)?;
    assert_no_differences(problems, cases.len());
    Ok(())
}

#[test]
fn truncated_objects_match_unpack_objects() -> TestResult {
    let base = Base::new()?;
    let cases = corpus::truncations(&base);
    let problems = differences(&base, &cases, Driver::UnpackObjects, &compare)?;
    assert_no_differences(problems, cases.len());
    Ok(())
}

#[test]
fn no_input_makes_the_checker_panic() -> TestResult {
    let base = Base::new()?;
    let mut cases = all_cases(&base);
    cases.extend(corpus::truncations(&base));
    for valid in corpus::valid(&base) {
        for pos in 0..valid.data.len() {
            for replacement in [0u8, b'\n', b' ', b'0', b'<', b'>', 0xff] {
                let mut data = valid.data.clone();
                data[pos] = replacement;
                cases.push(oracle::case(
                    format!("{}-{pos}-{replacement}", valid.name),
                    valid.kind,
                    data,
                ));
            }
        }
    }
    for options in [Options::strict(), Options::lenient()] {
        for case in &cases {
            let id = gix_object::compute_hash(gix_testtools::object_hash(), case.kind, &case.data)?;
            let mut checker = Checker::new(options.clone(), gix_testtools::object_hash());
            checker
                .check(
                    &id,
                    case.kind,
                    case.data.len() as u64,
                    Some(&case.data),
                    &mut Vec::new(),
                )
                .ok();
        }
    }
    Ok(())
}

#[test]
fn overrides_and_skip_lists_match_unpack_objects() -> TestResult {
    let base = Base::new()?;
    let cases = corpus::configured(&base);
    let problems = differences(&base, &cases, Driver::UnpackObjects, &compare)?;
    assert_no_differences(problems, cases.len());
    Ok(())
}

#[test]
fn overrides_and_skip_lists_match_index_pack() -> TestResult {
    let base = Base::new()?;
    let cases = corpus::configured(&base);
    let problems = differences(&base, &cases, Driver::IndexPack, &compare_index_pack)?;
    assert_no_differences(problems, cases.len());
    Ok(())
}

mod checker;

#[test]
fn gitmodules_checks_match_unpack_objects() -> TestResult {
    let base = Base::new()?;
    let cases = gitmodules::cases(&base);
    let problems = differences(&base, &cases, Driver::UnpackObjects, &|_| Compare::WithoutFatal)?;
    assert_no_differences(problems, cases.len());
    Ok(())
}

#[test]
fn gitmodules_checks_match_index_pack() -> TestResult {
    let base = Base::new()?;
    // What is in the file doesn't depend on the program that is run, so only a sample is needed to see
    // that both orders of blob and tree work. 7 is coprime with the 2 that decides the order.
    let cases: Vec<_> = gitmodules::cases(&base)
        .into_iter()
        .enumerate()
        .filter_map(|(idx, case)| (idx % 7 == 0).then_some(case))
        .collect();
    let problems = differences(&base, &cases, Driver::IndexPack, &|_| Compare::WithoutFatal)?;
    assert_no_differences(problems, cases.len());
    Ok(())
}

#[test]
fn random_submodule_urls_match_unpack_objects() -> TestResult {
    let base = Base::new()?;
    let cases = gitmodules::random_url_cases(&base, 2000);
    // The corpus is only useful if it has enough of what's refused and what isn't.
    let odb = base.odb()?;
    let refused = cases
        .iter()
        .filter(|case| !checker_observed(&odb, case.options(), case, Driver::UnpackObjects).accepted)
        .count();
    assert!(
        (300..1700).contains(&refused),
        "{refused} of {} are refused, which isn't a good mix",
        cases.len()
    );
    let problems = differences(&base, &cases, Driver::UnpackObjects, &|_| Compare::WithoutFatal)?;
    assert_no_differences(problems, cases.len());
    Ok(())
}
