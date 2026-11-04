//! Complete pushes to a real `git receive-pack` with packs from `gix-pack`, and what the server does about them,
//! like refusing, vetoing, and failing to unpack.
//!
//! Everything that is a decision of the server uses its own words, as a client has to deal with them. Every
//! push that is expected to change the remote is followed by `git fsck` of it.
use std::{
    io::Write,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

use gix_error::Class;
use gix_hash::{Kind, ObjectId};
use gix_pack::data::output::missing;
use gix_protocol::push::{
    Command, Options, Request, ServerCapabilities, StatusFormat, push,
    report::{Report, Status, Unpack},
};
use gix_testtools::{Creation, TestResult};

use super::{
    classes,
    real_peer::{Fixture, connect, fixtures, refs, rev, write_all},
};

/// What the remote said besides the report, as `(is_error, text)`.
type Messages = Vec<(bool, String)>;

/// A pack producer for `tips` that the receiver, which has `haves`, doesn't have, made from the objects of the repository at `source`.
/// The size of the pack in bytes is stored in `size` once it was written.
fn real_pack(
    kind: Kind,
    source: &Path,
    tips: Vec<ObjectId>,
    haves: Vec<ObjectId>,
    size: Arc<AtomicU64>,
) -> impl FnOnce(&mut dyn Write) -> gix_error::Result + use<> {
    let objects = source.join(".git/objects");
    move |out| {
        let store = gix_odb::Store::at_opts(
            objects,
            kind,
            &mut None.into_iter(),
            gix_odb::store::init::Options::default(),
        )
        .map_err(gix_error::Error::from_error)?;
        let mut db = Arc::new(store).to_cache_arc();
        db.prevent_pack_unload();
        let options = missing::Options::default();
        let counts = missing::counts(
            db.clone(),
            &tips,
            &haves,
            &gix_utils::progress::Discard,
            &AtomicBool::new(false),
            options,
        )?;
        let outcome = missing::write_pack(db, counts, out, kind, options)?;
        size.store(outcome.bytes_written, Ordering::Relaxed);
        Ok(())
    }
}

fn pack_for(
    kind: Kind,
    source: &Path,
    tips: &[ObjectId],
    haves: &[ObjectId],
) -> impl FnOnce(&mut dyn Write) -> gix_error::Result + use<> {
    real_pack(kind, source, tips.to_vec(), haves.to_vec(), Arc::default())
}

/// Push `commands` to `remote` and return the report or error along with the remote's messages.
fn push_collecting(
    kind: Kind,
    remote: &Path,
    commands: Vec<Command>,
    options: Options,
    pack: impl FnOnce(&mut dyn Write) -> gix_error::Result,
) -> TestResult<(gix_error::Result<Report>, Messages)> {
    let (mut transport, handshake) = connect(remote)?;
    let request = Request::new(
        commands,
        kind,
        &ServerCapabilities::from_capabilities(&handshake.capabilities),
        options,
    )?;
    let mut messages = Messages::new();
    let mut on_message =
        |is_error: bool, text: &[u8]| messages.push((is_error, String::from_utf8_lossy(text).into_owned()));
    let result = push(&mut transport, &request, pack, Some(&mut on_message), false);
    Ok((result, messages))
}

fn push_ok(
    kind: Kind,
    remote: &Path,
    commands: Vec<Command>,
    options: Options,
    pack: impl FnOnce(&mut dyn Write) -> gix_error::Result,
) -> TestResult<(Report, Messages)> {
    let (report, messages) = push_collecting(kind, remote, commands, options, pack)?;
    Ok((report?, messages))
}

fn assert_fsck(remote: &Path, context: &str) -> TestResult {
    gix_testtools::git(remote, "fsck --full --strict").map_err(|err| format!("{context}: fsck failed: {err}"))?;
    Ok(())
}

fn install_hook(remote: &Path, name: &str, script: &str) -> TestResult {
    let hook = remote.join("hooks").join(name);
    std::fs::create_dir_all(hook.parent().expect("a parent"))?;
    std::fs::write(&hook, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn rejected_reason(report: &Report, ref_name: &str) -> String {
    match &report
        .commands
        .iter()
        .find(|command| command.ref_name == ref_name)
        .unwrap_or_else(|| panic!("{ref_name} is in the report: {report:?}"))
        .status
    {
        Status::Rejected { reason } => reason.to_string(),
        other => panic!("{ref_name} was expected to be rejected, but is {other:?}"),
    }
}

/// The ids of the commits "one", "two" and "three" of the source, as the remote has "one" and "two".
fn ids(f: &Fixture) -> TestResult<(ObjectId, ObjectId, ObjectId)> {
    Ok((
        rev(&f.source, "victim")?,
        rev(&f.source, "main")?,
        rev(&f.source, "other")?,
    ))
}

#[test]
fn a_real_pack_creates_updates_and_deletes_in_one_push() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for f in fixtures()? {
        let kind = f.kind;
        let (one, two, three) = ids(&f)?;
        let (report, _) = push_ok(
            kind,
            &f.remote,
            vec![
                Command::new("refs/heads/main", two, three)?,
                Command::new("refs/heads/other", kind.null(), three)?,
                Command::new("refs/tags/victim", one, kind.null())?,
            ],
            Options::default(),
            pack_for(kind, &f.source, &[three], &[two, one]),
        )?;
        assert!(report.is_success(), "{kind}: {report:?}");
        assert_eq!(
            refs(&f.remote)?,
            format!("refs/heads/main {three}\nrefs/heads/other {three}\n"),
            "{kind}: the update, the creation and the deletion all happened"
        );
        assert_fsck(&f.remote, &format!("{kind}"))?;
    }
    Ok(())
}

#[test]
fn atomic_pushes_apply_everything_or_nothing() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    let atomic = || Options {
        atomic: true,
        ..Default::default()
    };
    for f in fixtures()? {
        let kind = f.kind;
        let (one, two, three) = ids(&f)?;
        let (report, _) = push_ok(
            kind,
            &f.remote,
            vec![
                Command::new("refs/heads/main", two, three)?,
                Command::new("refs/tags/victim", one, kind.null())?,
            ],
            atomic(),
            pack_for(kind, &f.source, &[three], &[two, one]),
        )?;
        assert!(report.is_success(), "{kind}: {report:?}");
        assert_eq!(
            refs(&f.remote)?,
            format!("refs/heads/main {three}\n"),
            "{kind}: all commands were applied"
        );
    }

    for f in fixtures()? {
        let kind = f.kind;
        let (one, two, three) = ids(&f)?;
        let before = refs(&f.remote)?;
        // The caller expects `victim` at "two" even though it is at "one".
        let (report, _) = push_ok(
            kind,
            &f.remote,
            vec![
                Command::new("refs/heads/other", kind.null(), three)?,
                Command::new("refs/tags/victim", two, kind.null())?,
            ],
            atomic(),
            pack_for(kind, &f.source, &[three], &[two, one]),
        )?;
        assert!(!report.is_success() && report.is_definite(), "{kind}: {report:?}");
        for name in ["refs/heads/other", "refs/tags/victim"] {
            assert_eq!(
                rejected_reason(&report, name),
                "atomic transaction failed",
                "{kind}: the stale expectation fails the transaction, and with it every command"
            );
        }
        assert_eq!(refs(&f.remote)?, before, "{kind}: nothing was applied");
    }
    Ok(())
}

#[test]
fn push_options_reach_the_servers_hooks_if_it_offers_them() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for f in fixtures()? {
        let kind = f.kind;
        let (one, two, three) = ids(&f)?;
        let options = || Options {
            push_options: vec!["ci.skip".into(), "topic=a b".into()],
            ..Default::default()
        };
        let command = || Command::new("refs/heads/main", two, three);

        let (_transport, handshake) = connect(&f.remote)?;
        let err = Request::new(
            vec![command()?],
            kind,
            &ServerCapabilities::from_capabilities(&handshake.capabilities),
            options(),
        )
        .expect_err("push options are not offered by default");
        assert_eq!(classes(&err), [Class::Unsupported], "{kind}: {err:#}");

        gix_testtools::git(&f.remote, "config receive.advertisePushOptions true")?;
        install_hook(
            &f.remote,
            "pre-receive",
            r#"#!/bin/sh
i=0
while [ "$i" -lt "${GIT_PUSH_OPTION_COUNT:-0}" ]; do
  eval "printf '%s\n' \"\$GIT_PUSH_OPTION_$i\"" >>"$GIT_DIR/push-options-seen"
  i=$((i + 1))
done
"#,
        )?;
        let (report, _) = push_ok(
            kind,
            &f.remote,
            vec![command()?],
            options(),
            pack_for(kind, &f.source, &[three], &[two, one]),
        )?;
        assert!(report.is_success(), "{kind}: {report:?}");
        assert_eq!(
            std::fs::read_to_string(f.remote.join("push-options-seen"))?,
            "ci.skip\ntopic=a b\n",
            "{kind}: the hook saw the options in order, spaces included"
        );
    }
    Ok(())
}

#[test]
fn report_status_v2_is_accepted() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for f in fixtures()? {
        let kind = f.kind;
        let (one, two, three) = ids(&f)?;
        let (report, _) = push_ok(
            kind,
            &f.remote,
            vec![
                Command::new("refs/heads/main", two, three)?,
                Command::new("refs/heads/other", kind.null(), three)?,
            ],
            Options {
                status_format: StatusFormat::V2,
                ..Default::default()
            },
            pack_for(kind, &f.source, &[three], &[two, one]),
        )?;
        assert!(report.is_success(), "{kind}: {report:?}");
        assert!(
            report
                .commands
                .iter()
                .all(|c| matches!(&c.status, Status::Accepted { updates } if updates.len() == 1)),
            "{kind}: each ok is one update, which has no options without a hook that redirects: {report:?}"
        );
    }
    Ok(())
}

#[test]
fn refusals_by_server_policy_are_definite_rejections_in_its_words() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for f in fixtures()? {
        let kind = f.kind;
        let (one, two, _three) = ids(&f)?;

        gix_testtools::git(&f.remote, "config receive.denyDeletes true")?;
        gix_testtools::git(&f.remote, &format!("update-ref refs/heads/spare {two}"))?;
        let before = refs(&f.remote)?;
        let (report, _) = push_ok(
            kind,
            &f.remote,
            vec![Command::new("refs/heads/spare", two, kind.null())?],
            Options::default(),
            |_| panic!("a pack is not sent when only deleting"),
        )?;
        assert_eq!(
            rejected_reason(&report, "refs/heads/spare"),
            "deletion prohibited",
            "{kind}"
        );
        assert!(report.is_definite(), "{kind}: {report:?}");
        assert_eq!(refs(&f.remote)?, before, "{kind}: the branch is still there");

        gix_testtools::git(&f.remote, "config receive.denyNonFastForwards true")?;
        let (report, _) = push_ok(
            kind,
            &f.remote,
            vec![Command::new("refs/heads/main", two, one)?],
            Options::default(),
            pack_for(kind, &f.source, &[one], &[two]),
        )?;
        assert_eq!(
            rejected_reason(&report, "refs/heads/main"),
            "non-fast-forward",
            "{kind}"
        );
        assert!(report.is_definite(), "{kind}: {report:?}");
        assert_eq!(refs(&f.remote)?, before, "{kind}: the branch wasn't rewound");
    }
    Ok(())
}

#[test]
fn a_pre_receive_hook_rejects_everything_and_its_messages_are_passed_on() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for f in fixtures()? {
        let kind = f.kind;
        let (one, two, three) = ids(&f)?;
        install_hook(
            &f.remote,
            "pre-receive",
            "#!/bin/sh\necho 'no pushing today' >&2\nexit 1\n",
        )?;
        let before = refs(&f.remote)?;
        let (report, messages) = push_ok(
            kind,
            &f.remote,
            vec![
                Command::new("refs/heads/main", two, three)?,
                Command::new("refs/heads/other", kind.null(), three)?,
            ],
            Options::default(),
            pack_for(kind, &f.source, &[three], &[two, one]),
        )?;
        for name in ["refs/heads/main", "refs/heads/other"] {
            assert_eq!(rejected_reason(&report, name), "pre-receive hook declined", "{kind}");
        }
        assert!(report.is_definite(), "{kind}: {report:?}");
        assert!(
            messages.iter().any(|(_, text)| text.contains("no pushing today")),
            "{kind}: the hook's words reach the caller: {messages:?}"
        );
        assert_eq!(refs(&f.remote)?, before, "{kind}: nothing was applied");
    }
    Ok(())
}

#[test]
fn an_update_hook_rejects_single_references_while_others_are_applied() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for f in fixtures()? {
        let kind = f.kind;
        let (one, two, three) = ids(&f)?;
        install_hook(
            &f.remote,
            "update",
            "#!/bin/sh\nif [ \"$1\" = refs/heads/other ]; then echo 'other is closed' >&2; exit 1; fi\n",
        )?;
        let (report, messages) = push_ok(
            kind,
            &f.remote,
            vec![
                Command::new("refs/heads/main", two, three)?,
                Command::new("refs/heads/other", kind.null(), three)?,
            ],
            Options::default(),
            pack_for(kind, &f.source, &[three], &[two, one]),
        )?;
        assert!(
            matches!(report.commands[0].status, Status::Accepted { .. }),
            "{kind}: {report:?}"
        );
        assert_eq!(rejected_reason(&report, "refs/heads/other"), "hook declined", "{kind}");
        assert!(
            report.is_definite() && !report.is_success(),
            "{kind}: a mixed, but certain, outcome"
        );
        assert!(
            messages.iter().any(|(_, text)| text.contains("other is closed")),
            "{kind}: {messages:?}"
        );
        assert_eq!(
            refs(&f.remote)?,
            format!("refs/heads/main {three}\nrefs/tags/victim {one}\n"),
            "{kind}: only the reference that wasn't vetoed changed"
        );
        assert_fsck(&f.remote, &format!("{kind}"))?;
    }
    Ok(())
}

#[test]
fn a_pack_the_server_cannot_unpack_rejects_every_command() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for f in fixtures()? {
        let kind = f.kind;
        let (_one, two, three) = ids(&f)?;
        // A pack that promises an object and has none. Its trailer is right, so the driver passes it on.
        let mut broken = b"PACK\0\0\0\x02\0\0\0\x01".to_vec();
        let mut hasher = gix_hash::hasher(kind);
        hasher.update(&broken);
        broken.extend_from_slice(hasher.try_finalize()?.as_slice());

        let before = refs(&f.remote)?;
        let (report, _) = push_ok(
            kind,
            &f.remote,
            vec![
                Command::new("refs/heads/main", two, three)?,
                Command::new("refs/heads/other", kind.null(), three)?,
            ],
            Options::default(),
            write_all(broken),
        )?;
        assert!(matches!(report.unpack, Unpack::Failed(_)), "{kind}: {report:?}");
        assert!(
            report
                .commands
                .iter()
                .all(|c| matches!(c.status, Status::Rejected { .. })),
            "{kind}: {report:?}"
        );
        assert!(report.is_definite(), "{kind}: nothing can be applied without objects");
        assert_eq!(refs(&f.remote)?, before, "{kind}: nothing was applied");
    }
    Ok(())
}

/// Push more than a megabyte of poorly compressible data with a pack from `gix-pack` into `remote`, and make sure it finishes in time,
/// as a push that stalls never does. What is proven is that a pack bigger than the pipes between the driver and the server doesn't
/// make them wait for each other, which `progress` could do if the server's output wasn't small. That the pack is produced with bounded
/// memory is proven in `gix-pack`.
fn push_a_big_pack(kind: Kind, progress: bool) -> TestResult<(Report, Messages, u64)> {
    let dir = gix_testtools::scripted_fixture_writable_with_args(
        "make_push_big_source.sh",
        [kind.to_string()],
        Creation::CopyFromReadOnly,
    )?;
    let (source, remote) = (dir.path().join("source"), dir.path().join("remote.git"));
    let (base, big) = (rev(&source, "base")?, rev(&source, "main")?);

    let (tx, rx) = mpsc::channel();
    let size = Arc::new(AtomicU64::new(0));
    let (thread_remote, thread_size) = (remote.clone(), size.clone());
    std::thread::spawn(move || {
        let result = push_ok(
            kind,
            &thread_remote,
            vec![
                Command::new("refs/heads/main", base, big).expect("valid"),
                Command::new("refs/heads/copy", kind.null(), big).expect("valid"),
            ],
            Options {
                progress,
                ..Default::default()
            },
            real_pack(kind, &source, vec![big], vec![base], thread_size),
        );
        tx.send(result.map_err(|err| format!("{err:?}"))).ok();
    });
    let (report, messages) = rx
        .recv_timeout(Duration::from_secs(90))
        .map_err(|_| {
            format!("{kind}: the push didn't finish in time, as if the server and the driver wait for each other")
        })?
        .map_err(|err| format!("{kind}: {err}"))?;
    assert_eq!(
        refs(&remote)?,
        format!("refs/heads/copy {big}\nrefs/heads/main {big}\n"),
        "{kind}: both references were updated"
    );
    assert_fsck(&remote, &format!("{kind}"))?;
    Ok((report, messages, size.load(Ordering::Relaxed)))
}

/// Without asking for progress, which is the default, and for one hash with it, as that is what could stall a driver that
/// can't read while it writes. Whether a server writes enough progress for that can't be provoked with a pack that
/// fits a test, so for packs of this size, the messages that arrive are the only thing there is to check.
#[test]
fn a_pack_bigger_than_a_pipe_buffer_completes_with_and_without_progress() -> TestResult {
    if gix_testtools::run_in_isolated_process()? {
        return Ok(());
    }
    for (index, kind) in Kind::all().iter().copied().enumerate() {
        let progress = index % 2 == 1;
        let (report, messages, pack_bytes) = push_a_big_pack(kind, progress)?;
        assert!(pack_bytes > 1024 * 1024, "{kind}: {pack_bytes} bytes");
        assert!(report.is_success(), "{kind}: {report:?}");
        assert!(
            messages.iter().all(|(is_error, _)| !is_error),
            "{kind} (progress: {progress}): {messages:?}"
        );
    }
    Ok(())
}
