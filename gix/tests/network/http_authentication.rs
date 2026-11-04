//! How credentials are obtained, used, stored and erased when a peer asks for them, for fetching and for pushing.
use gix::{protocol::transport::client::blocking_io::http, remote::Direction};
use gix_testtools::{
    TestResult,
    http_peer::{Config, HttpPeer, Moved, Protect},
};
use std::sync::atomic::AtomicBool;

use crate::{
    HASHES, Helper, PASSWORD, Setup, USER, for_each_backend, isolated, push_handshake_and_flush, statuses, transport,
};

fn fetch_refs<H: http::Http + Default + 'static>(setup: &Setup, url: &str, helper: &Helper) -> Result<(), gix::Error> {
    let mut dest = setup.dest().expect("the fixture repository can be opened");
    helper.configure(&mut dest).expect("a valid configuration");
    let remote = dest
        .remote_at(url)?
        .with_refspecs(Some("+refs/heads/*:refs/remotes/origin/*"), Direction::Fetch)?;
    remote
        .to_connection_with_transport(transport::<H>(url).expect("a valid URL"))
        .prepare_fetch(gix::progress::Discard, Default::default())?
        .receive(gix::progress::Discard, &AtomicBool::new(false))?;
    Ok(())
}

fn credentials_are_obtained_after_a_challenge_and_stored_once_accepted<H: http::Http + Default + 'static>(
    hash: &'static str,
) -> TestResult {
    let setup = Setup::new(hash)?;
    let peer = setup.peer(Protect::Everything)?;
    let helper = Helper::new(&setup.path("."), USER, PASSWORD)?;
    fetch_refs::<H>(&setup, &peer.url("remote.git"), &helper)?;
    assert_eq!(
        helper.actions(),
        ["get", "store"],
        "{hash}: the helper is asked once, and told to remember what worked"
    );
    let statuses = statuses(&peer);
    assert_eq!(
        statuses[..2],
        ["GET remote.git/info/refs 401", "GET remote.git/info/refs 200"],
        "{hash}: the first request is challenged, and the second is authenticated: {statuses:?}"
    );
    assert!(
        statuses[2..].iter().all(|status| status.ends_with(" 200")),
        "{hash}: all later requests are authenticated: {statuses:?}"
    );
    Ok(())
}

fn rejected_credentials_are_erased_and_fail_as_authentication<H: http::Http + Default + 'static>(
    hash: &'static str,
) -> TestResult {
    let setup = Setup::new(hash)?;
    let peer = setup.peer(Protect::Everything)?;
    let helper = Helper::new(&setup.path("."), USER, "wrong")?;
    let err = fetch_refs::<H>(&setup, &peer.url("remote.git"), &helper).expect_err("the password is wrong");
    assert_eq!(
        helper.actions(),
        ["get", "erase"],
        "{hash}: what the peer refused isn't remembered, and the helper is told to forget it"
    );
    assert!(
        err.is_unauthenticated(),
        "{hash}: the failure is about authentication: {err:?}"
    );
    Ok(())
}

#[test]
fn fetch_asks_for_credentials_after_a_challenge() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for hash in HASHES {
        for_each_backend!(credentials_are_obtained_after_a_challenge_and_stored_once_accepted(
            hash
        ));
        for_each_backend!(rejected_credentials_are_erased_and_fail_as_authentication(hash));
    }
    Ok(())
}

fn push_is_authenticated<H: http::Http + Default + 'static>(
    hash: &'static str,
    protect: Protect,
    credentials_are_good: bool,
) -> TestResult {
    let setup = Setup::new(hash)?;
    let peer = setup.peer(protect)?;
    let helper = Helper::new(&setup.path("."), USER, PASSWORD)?;
    let mut client = setup.work()?;
    helper.configure(&mut client)?;
    let url = peer.url("remote.git");
    let result = push_handshake_and_flush(&client, transport::<H>(&url)?);
    let statuses = statuses(&peer);
    let push = "POST remote.git/git-receive-pack";
    let advertisement = "GET remote.git/info/refs";
    if credentials_are_good {
        result.map_err(|err| format!("{hash} {protect:?}: {err:?}, requests: {statuses:?}"))?;
        assert_eq!(
            helper.actions(),
            ["get", "store"],
            "{hash} {protect:?}: credentials are obtained when the push needs them, and stored after they worked: {statuses:?}"
        );
        if protect == Protect::Post {
            assert_eq!(
                statuses,
                [
                    format!("{advertisement} 200"),
                    format!("{push} 401"),
                    format!("{advertisement} 200"),
                    format!("{push} 200"),
                    format!("{push} 200")
                ],
                "{hash}: like `git`, a request without commands probes if pushing needs credentials, after `info/refs` and before anything is sent"
            );
        }
    } else {
        let err = result.expect_err("the peer refuses to take pushes from these credentials");
        assert_eq!(
            helper.actions(),
            ["get", "erase"],
            "{hash} {protect:?}: credentials that can't push aren't remembered: {statuses:?}"
        );
        assert_eq!(
            statuses,
            [
                format!("{advertisement} 401"),
                format!("{advertisement} 200"),
                format!("{push} 401")
            ],
            "{hash}: the probe is rejected after the credentials that read were used, and nothing else is sent"
        );
        assert!(
            format!("{err:?}").contains("401") || format!("{err:?}").contains("not accepted"),
            "{hash} {protect:?}: the error says that authentication failed: {err:?}"
        );
    }
    Ok(())
}

#[test]
fn push_authenticates_where_the_peer_challenges_it() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for hash in HASHES {
        for protect in [Protect::ReceivePack, Protect::Everything] {
            for_each_backend!(push_is_authenticated(hash, protect, true));
        }
    }
    Ok(())
}

#[test]
fn push_authenticates_when_only_the_post_is_challenged() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for hash in HASHES {
        for_each_backend!(push_is_authenticated(hash, Protect::Post, true));
    }
    Ok(())
}

#[test]
fn credentials_that_work_for_reading_but_not_for_pushing_are_not_remembered() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for hash in HASHES {
        for_each_backend!(push_is_authenticated(hash, Protect::ReadOnlyCredentials, false));
    }
    Ok(())
}

fn push_follows_a_renamed_repository<H: http::Http + Default + 'static>(hash: &'static str) -> TestResult {
    let setup = Setup::new(hash)?;
    let peer = HttpPeer::start(Config {
        project_root: setup.path("."),
        moved: Some(Moved {
            from: "old/remote.git".into(),
            to: "remote.git".into(),
        }),
        ..Default::default()
    })?;
    let client = setup.work()?;
    push_handshake_and_flush(&client, transport::<H>(&peer.url("old/remote.git"))?)?;
    assert_eq!(
        statuses(&peer),
        [
            "GET old/remote.git/info/refs 301",
            "GET remote.git/info/refs 200",
            "POST remote.git/git-receive-pack 200",
            "POST remote.git/git-receive-pack 200",
        ],
        "{hash}: the redirect of `info/refs` is followed, and the probe and the push go to where it led"
    );
    Ok(())
}

#[test]
fn push_follows_the_redirect_of_a_renamed_repository() -> TestResult {
    let Some(_environment) = isolated()? else {
        return Ok(());
    };
    for hash in HASHES {
        for_each_backend!(push_follows_a_renamed_repository(hash));
    }
    Ok(())
}
