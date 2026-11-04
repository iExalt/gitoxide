use gix_error::Class;
use gix_hash::Kind;
use gix_protocol::push::Command;

use super::{classes, id};

#[test]
fn create_update_and_delete_are_classified_by_their_ids() {
    for kind in Kind::all().iter().copied() {
        let create = Command::new("refs/heads/new", kind.null(), id(kind, 1)).expect("valid");
        assert!(
            create.is_create() && !create.is_delete(),
            "{kind}: the null old id means creation"
        );
        let update = Command::new("refs/heads/main", id(kind, 1), id(kind, 2)).expect("valid");
        assert!(
            !update.is_create() && !update.is_delete(),
            "{kind}: a changed id is an update"
        );
        assert_eq!(update.old_id(), id(kind, 1));
        assert_eq!(update.new_id(), id(kind, 2));
        assert_eq!(update.ref_name(), "refs/heads/main");
        let delete = Command::new("refs/tags/gone", id(kind, 2), kind.null()).expect("valid");
        assert!(
            delete.is_delete() && !delete.is_create(),
            "{kind}: the null new id means deletion"
        );
    }
}

#[test]
fn invalid_commands_are_rejected_as_validation_errors() {
    let kind = Kind::all()[0];
    let other = Kind::all().last().copied().expect("at least one kind");
    let mut cases: Vec<(&str, gix_error::Result<Command>)> = vec![
        ("name outside of refs/", Command::new("HEAD", id(kind, 1), id(kind, 2))),
        ("name without a path", Command::new("refs/", id(kind, 1), id(kind, 2))),
        (
            "name with a double dot",
            Command::new("refs/heads/a..b", id(kind, 1), id(kind, 2)),
        ),
        (
            "name with a space",
            Command::new("refs/heads/a b", id(kind, 1), id(kind, 2)),
        ),
        (
            "no-op update",
            Command::new("refs/heads/main", id(kind, 1), id(kind, 1)),
        ),
        (
            "deleting an absent ref",
            Command::new("refs/heads/main", kind.null(), kind.null()),
        ),
    ];
    if kind != other {
        cases.push((
            "ids of different kinds",
            Command::new("refs/heads/main", id(kind, 1), id(other, 2)),
        ));
    }
    for (name, result) in cases {
        let err = result.expect_err(name);
        assert_eq!(classes(&err), [Class::Validation], "{name}: {err:#}");
    }
}
