//! The pushes that the transports have to carry in the same way, written once and run over each of them: updates with leases that
//! match, a lease that doesn't and is refused, and an atomic push of several updates. Each case checks the repository that was pushed to.
//!
//! The macro takes the name that the remote is bound to and an expression that connects it, as the connection borrows the remote.

/// Run the cases with `$remote` bound to the remote at `$url` of the repository `$repo`, which `$make_repo` opens from `$setup`, and
/// `$connect` evaluating to its connection. `$hash` is only used to say which object format failed.
macro_rules! push_scenarios {
    ($setup:expr, $hash:expr, $url:expr, $repo:ident = $make_repo:expr, |$remote:ident| $connect:expr) => {{
        use gix::remote::push::{Options, Status, Update};
        let setup: &$crate::Setup = $setup;
        let hash: &str = $hash;
        let remote_repo = gix::open_opts(setup.path("remote.git"), gix::open::Options::isolated())?;
        let remote_id = |name: &str| -> TestResult<Option<gix::ObjectId>> {
            Ok(remote_repo
                .try_find_reference(name)?
                .map(|reference| reference.id().detach()))
        };
        let name = |name: &str| gix::refs::FullName::try_from(name);
        let $repo = $make_repo;
        let head_id = $repo.head_id()?.detach();
        let one_id = $crate::grandparent_of_head(&$repo)?;
        let main_before = remote_id("refs/heads/main")?.expect("the fixture has `main` on the remote");
        assert_ne!(
            main_before, head_id,
            "{hash}: the work repository is ahead of the remote"
        );

        // A lease that doesn't match is refused before anything is sent, and an atomic push is refused as a whole.
        {
            let $remote = $repo.remote_at($url)?;
            let prepare = $connect.prepare_push(
                gix::progress::Discard,
                vec![
                    Update::change(name("refs/heads/main")?, one_id, head_id),
                    Update::create(name("refs/heads/not-created")?, head_id),
                ],
                Options::default(),
            )?;
            prepare
                .send(&std::sync::atomic::AtomicBool::new(false))
                .expect_err("an atomic push with a refused update sends nothing");
        }
        assert_eq!(
            remote_id("refs/heads/main")?,
            Some(main_before),
            "{hash}: the stale lease protected `main`"
        );
        assert_eq!(
            remote_id("refs/heads/not-created")?,
            None,
            "{hash}: the atomic push didn't create the other branch either"
        );

        // Without atomicity, the update that is refused is skipped and the other one is sent.
        {
            let $remote = $repo.remote_at($url)?;
            let options = Options {
                atomic: false,
                ..Default::default()
            };
            let outcome = $connect
                .prepare_push(
                    gix::progress::Discard,
                    vec![
                        Update::change(name("refs/heads/main")?, one_id, head_id),
                        Update::create(name("refs/heads/created")?, head_id),
                    ],
                    options,
                )?
                .send(&std::sync::atomic::AtomicBool::new(false))?;
            assert!(
                matches!(outcome.entries[0].status, Status::Refused(_)),
                "{hash}: {outcome:?}"
            );
            assert!(
                matches!(outcome.entries[1].status, Status::Applied { .. }),
                "{hash}: {outcome:?}"
            );
        }
        assert_eq!(
            remote_id("refs/heads/main")?,
            Some(main_before),
            "{hash}: `main` is as it was"
        );
        assert_eq!(
            remote_id("refs/heads/created")?,
            Some(head_id),
            "{hash}: the update that wasn't refused arrived"
        );

        // Atomic updates that all match their leases are applied together, including a deletion.
        {
            let $remote = $repo.remote_at($url)?;
            let outcome = $connect
                .prepare_push(
                    gix::progress::Discard,
                    vec![
                        Update::change(name("refs/heads/main")?, main_before, head_id),
                        Update::create(name("refs/heads/second")?, head_id),
                        Update::delete(name("refs/heads/created")?, head_id),
                    ],
                    Options::default(),
                )?
                .send(&std::sync::atomic::AtomicBool::new(false))?;
            assert!(outcome.is_success(), "{hash}: {outcome:?}");
        }
        assert_eq!(remote_id("refs/heads/main")?, Some(head_id), "{hash}: fast-forwarded");
        assert_eq!(remote_id("refs/heads/second")?, Some(head_id), "{hash}: created");
        assert_eq!(remote_id("refs/heads/created")?, None, "{hash}: deleted");
    }};
}
pub(crate) use push_scenarios;
