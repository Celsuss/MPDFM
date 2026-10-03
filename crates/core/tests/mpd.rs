//! The MPD client (task 13), one test per acceptance criterion.
//!
//! Three kinds of test, deliberately:
//!
//! **Transcripts.** Most of this file drives the client from the recorded
//! conversations in `tests/transcripts/`, captured from MPD 0.24.0 on
//! `127.0.0.1:6600`. They need no daemon, so CI runs them, and they assert the
//! bytes the client *sent* as well as what it made of the answer — a quoting
//! mistake fails on the wire even though a real daemon would have answered
//! anyway.
//!
//! **Real sockets.** Connection refused, a read that times out and a daemon that
//! vanishes mid-response cannot be recorded, so those use a real `TcpListener` on
//! an ephemeral port and no daemon at all.
//!
//! **The real daemon, when there is one.** Two tests connect to
//! `127.0.0.1:6600` and skip themselves with a message when nothing answers. They
//! only read, except for an `update` of a path that does not exist, which MPD
//! queues and then finds nothing to do — no test here changes the user's queue,
//! playlists or database.

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use mpdfm_core::config::{Config, MpdAddress};
use mpdfm_core::library::DirPath;
use mpdfm_core::mpd::{self, AckCode, JobId, Mpd, MpdError, PlayState};
use mpdfm_core::paths::RelPath;
use mpdfm_core::testing::{Fixture, Transcript};

/// Parse one of the recorded conversations.
macro_rules! transcript {
    ($file:literal) => {
        Transcript::parse(include_str!(concat!("transcripts/", $file)))
    };
}

/// Short enough that a test that waits for it does not slow the suite down, long
/// enough that a loopback connection has no excuse.
const TIMEOUT: Duration = Duration::from_millis(300);

fn rel(path: &str) -> RelPath {
    RelPath::parse(path).expect("a valid relative path")
}

/// The real daemon, or `None` with a reason printed — CI has none.
fn real_daemon() -> Option<Mpd> {
    let addr = MpdAddress::parse("127.0.0.1", 6600).expect("the default address");
    match Mpd::connect(&addr, Duration::from_millis(500)) {
        Ok(mpd) => Some(mpd),
        Err(err) => {
            eprintln!("skipped: no MPD on {addr} ({err})");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// The greeting
// ---------------------------------------------------------------------------

#[test]
fn the_greeting_gives_the_version() {
    let script = transcript!("greeting.txt");
    let mut mpd = Mpd::handshake(script.stream()).expect("the recorded greeting");

    assert_eq!(mpd.version().to_string(), "0.24.0");
    assert_eq!(
        (
            mpd.version().major,
            mpd.version().minor,
            mpd.version().patch
        ),
        (0, 24, 0)
    );
    assert!(mpd.version().at_least(0, 21));

    mpd.ping().expect("a bare OK is a complete response");

    let stream = mpd.into_inner();
    stream.assert_sent_the_script();
    assert!(stream.drained(), "the whole recording should be consumed");
}

#[test]
fn connects_to_the_real_daemon_and_reports_its_version() {
    let Some(mut mpd) = real_daemon() else { return };

    let version = mpd.version().clone();
    println!("the daemon on 127.0.0.1:6600 is MPD {version}");
    assert!(
        version.at_least(0, 16),
        "an MPD this old predates the protocol MPDFM speaks: {version}"
    );
    // The connection works in both directions, not just for the greeting.
    mpd.ping().expect("the real daemon answers a ping");
}

#[test]
fn something_that_is_not_mpd_is_refused_at_the_greeting() {
    let script = transcript!("not-mpd.txt");
    let err = Mpd::handshake(script.stream()).expect_err("an HTTP server is not MPD");

    assert!(matches!(err, MpdError::Greeting { .. }), "{err:?}");
    assert!(err.to_string().contains("400 Bad Request"), "{err}");
    // Worth distinguishing: a misconfigured port is not "MPD is not running".
    assert!(!err.is_unreachable(), "{err}");
}

// ---------------------------------------------------------------------------
// Quoting — the acceptance criterion about the wire bytes
// ---------------------------------------------------------------------------

#[test]
fn update_quotes_a_path_with_every_awkward_character() {
    let script = transcript!("update-quoting.txt");
    let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");

    // Spaces, `..` inside a component, brackets.
    assert_eq!(
        mpd.update(Some(&rel(
            "hiphop/MF DOOM - Mm..Food (2004) [V0] scene-tag"
        )))
        .expect("an update is queued"),
        JobId(20)
    );
    // `&`, `+`, an apostrophe, parentheses.
    assert_eq!(
        mpd.update(Some(&rel(
            "hiphop/Snoop Dogg & Wiz Khalifa - Mac + Devin Go To High School (Soundtrack) (2011) [320] vtwin88cube/01.Smokin' On.mp3"
        )))
        .expect("an update is queued"),
        JobId(21)
    );
    // Not ASCII.
    assert_eq!(
        mpd.update(Some(&rel("electronic/KREAM - So Hï [c0D2h71bFFI]")))
            .expect("an update is queued"),
        JobId(22)
    );
    // A `"`, which has to be escaped inside the quotes.
    assert_eq!(
        mpd.update(Some(&rel(r#"quotes/a "quoted" name & a + sign"#)))
            .expect("an update is queued"),
        JobId(23)
    );
    // A `\`, the other character that is not a literal there. `RelPath` refuses
    // one (`paths::PathError::Backslash`), so it can only be sent as a raw
    // argument — and the escaping still has to be right when it is.
    assert_eq!(
        mpd.command("update", &[r"quotes/a back\slash"])
            .expect("an update is queued")
            .find("updating_db"),
        Some("24")
    );

    // The assertion that matters: every byte sent matches what the real daemon
    // was given when this transcript was recorded.
    mpd.into_inner().assert_sent_the_script();
}

#[test]
fn update_and_rescan_with_no_directory_name_nothing_at_all() {
    let script = transcript!("update-whole-library.txt");
    let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");

    assert_eq!(mpd.update(None).expect("queued"), JobId(20));
    assert_eq!(mpd.rescan(None).expect("queued"), JobId(21));

    // `update`, not `update ""`: an empty quoted argument is a different command.
    mpd.into_inner().assert_sent_the_script();
}

#[test]
fn update_dirs_sends_one_update_per_directory_and_the_root_as_none() {
    // Hand-written: the subject is how a `DirPath` becomes an argument, not what
    // MPD answers. `affected_dirs` has already reduced the list to the shallowest
    // directories that cover the transaction, so this sends them unchanged.
    let script = Transcript::parse(
        "S: OK MPD 0.24.0\n\
         C: update \"hiphop/MF DOOM - Mm..Food (2004) [V0] scene-tag\"\n\
         S: updating_db: 1\n\
         S: OK\n\
         C: update \"pop\"\n\
         S: updating_db: 2\n\
         S: OK\n\
         C: update\n\
         S: updating_db: 3\n\
         S: OK\n",
    );
    let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");

    let dirs = [
        DirPath::parse("hiphop/MF DOOM - Mm..Food (2004) [V0] scene-tag").expect("a directory"),
        DirPath::parse("pop").expect("a directory"),
        DirPath::root(),
    ];
    assert_eq!(
        mpd.update_dirs(&dirs).expect("three updates are queued"),
        vec![JobId(1), JobId(2), JobId(3)]
    );
    mpd.into_inner().assert_sent_the_script();
}

#[test]
fn a_filename_the_protocol_cannot_carry_is_refused_before_it_is_sent() {
    let script = Transcript::parse("S: OK MPD 0.24.0\n");
    let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");

    // ext4 allows a newline in a filename; MPD's tokenizer has no escape for one.
    let err = mpd
        .update(Some(&rel("pop/two\nlines.mp3")))
        .expect_err("a newline cannot be sent");
    assert!(matches!(err, MpdError::BadArgument { .. }), "{err:?}");

    // And nothing went out, so the connection is still usable.
    assert!(mpd.into_inner().written().is_empty());
}

#[test]
fn the_real_daemon_accepts_an_update_for_an_awkward_path() {
    let Some(mut mpd) = real_daemon() else { return };

    // A path that does not exist, with every character the quoting has to survive.
    // MPD queues the job, finds no such directory and changes nothing — the point
    // is that it accepted the command rather than answering `ACK [5@0]` about a
    // line it could not tokenize.
    let awkward = rel(r#"mpdfm wire test [&+'"] (does not exist)/album"#);
    match mpd.update(Some(&awkward)) {
        Ok(job) => println!("the real daemon queued update {job} for {awkward}"),
        // A daemon configured without update permission is a legitimate answer:
        // it still proves the command line was tokenized as one argument.
        Err(MpdError::Ack(ack)) if ack.code == AckCode::Permission => {
            println!("the real daemon is not allowed to update: {ack}");
        }
        Err(err) => panic!("the real daemon rejected a quoted path: {err}"),
    }
}

// ---------------------------------------------------------------------------
// ACK → a typed error
// ---------------------------------------------------------------------------

#[test]
fn an_ack_becomes_a_typed_error_carrying_its_code() {
    let script = transcript!("acks.txt");
    let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");

    // 5 — and MPD did not even know which command it was running, so `{}`.
    let err = mpd
        .command("nosuchcommand", &[])
        .expect_err("an unknown command is refused");
    assert_eq!(err.ack_code(), Some(AckCode::Unknown));
    let MpdError::Ack(ack) = &err else {
        panic!("{err:?}")
    };
    assert!(ack.command.is_empty());
    assert_eq!(ack.message, r#"unknown command "nosuchcommand""#);
    assert_eq!(ack.list_index, 0);
    assert!(!err.is_unreachable(), "a refusal is not unreachability");

    // 3 — a wrong password, and the error never quotes the password.
    let err = mpd.password("hunter2").expect_err("there is no password");
    assert_eq!(err.ack_code(), Some(AckCode::Password));
    assert!(!err.to_string().contains("hunter2"), "{err}");

    // 50 — the one a post-commit `update` can produce for a directory that has
    // just been moved away.
    let err = mpd
        .command("load", &["mpdfm no such playlist"])
        .expect_err("no such playlist");
    assert_eq!(err.ack_code(), Some(AckCode::NoExist));
    assert_eq!(
        err.to_string(),
        "MPD refused `load`: No such playlist (error 50 (no such entity))"
    );

    // The connection survives a refusal: all three went over one of them.
    mpd.into_inner().assert_sent_the_script();
}

// ---------------------------------------------------------------------------
// status, currentsong, playlistinfo
// ---------------------------------------------------------------------------

#[test]
fn a_recorded_status_becomes_a_status() {
    let script = transcript!("status-paused.txt");
    let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");

    let status = mpd.status().expect("the recorded status");
    assert_eq!(status.state, PlayState::Pause);
    assert!(status.state.has_song());
    assert_eq!(status.queue_len, 2);
    assert_eq!(status.queue_version, 644);
    assert_eq!(status.song, Some(0));
    assert_eq!(status.song_id, Some(438));
    assert_eq!(status.updating_db, None);

    mpd.into_inner().assert_sent_the_script();
}

#[test]
fn status_reports_an_update_already_running() {
    let script = transcript!("status-stopped-updating.txt");
    let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");

    let status = mpd.status().expect("the recorded status");
    assert_eq!(status.state, PlayState::Stop);
    assert!(!status.state.has_song());
    assert_eq!(status.song, None);
    assert_eq!(status.queue_len, 0);
    assert_eq!(status.updating_db, Some(JobId(14)));
}

#[test]
fn current_song_is_the_playing_track() {
    let script = transcript!("currentsong.txt");
    let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");

    assert_eq!(
        mpd.current_song().expect("a current song"),
        Some(rel(
            "hiphop-lofi/mf-doom/MF_DOOM_Lofi_Villain_IV_[8R6riJrP1rU].mp3"
        ))
    );
    mpd.into_inner().assert_sent_the_script();
}

#[test]
fn current_song_is_none_when_nothing_is_loaded() {
    let script = transcript!("currentsong-stopped.txt");
    let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");

    assert_eq!(mpd.current_song().expect("an empty response"), None);
}

#[test]
fn queue_paths_returns_the_queue_as_relpaths() {
    let script = transcript!("playlistinfo.txt");
    let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");

    assert_eq!(
        mpd.queue_paths().expect("the recorded queue"),
        vec![
            rel("hiphop-lofi/mf-doom/MF_DOOM_Lofi_Villain_IV_[8R6riJrP1rU].mp3"),
            rel(
                "hiphop-lofi/mf-doom/MF_DOOM_-_Lofi_Villain_(Lofi_Remix_Full_Album)_[z9_prlDi8L0].mp3"
            ),
        ],
        "the stream URL is not a library path and has no place in this list"
    );
    mpd.into_inner().assert_sent_the_script();
}

#[test]
fn queue_uris_keeps_the_entries_that_are_not_library_paths() {
    let script = transcript!("playlistinfo.txt");
    let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");

    let uris = mpd.queue_uris().expect("the recorded queue");
    assert_eq!(uris.len(), 3, "{uris:?}");
    assert_eq!(uris[2], "https://ice1.somafm.com/groovesalad-256-mp3");
}

#[test]
fn an_empty_queue_is_not_a_failure() {
    let script = transcript!("playlistinfo-empty.txt");
    let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");

    assert_eq!(mpd.queue_paths().expect("an empty response"), Vec::new());
}

#[test]
fn the_real_daemon_reports_a_queue_of_relpaths() {
    let Some(mut mpd) = real_daemon() else { return };

    let status = mpd.status().expect("the real daemon answers `status`");
    let paths = mpd
        .queue_paths()
        .expect("the real daemon answers the queue");
    let uris = mpd.queue_uris().expect("the real daemon answers the queue");

    assert_eq!(
        uris.len() as u32,
        status.queue_len,
        "`playlistinfo` and `playlistlength` disagree"
    );
    assert!(
        paths.len() <= uris.len(),
        "a library path is one of the URIs"
    );
    // Whatever is in there, every path MPDFM took from it is a real identity:
    // relative, `/`-separated, and round-tripping to the bytes MPD sent.
    for path in &paths {
        assert!(uris.contains(&path.as_str().to_owned()), "{path}");
    }
    if let Some(song) = mpd.current_song().expect("`currentsong` answers") {
        assert!(status.state.has_song() || uris.contains(&song.as_str().to_owned()));
    }
}

// ---------------------------------------------------------------------------
// Broken connections
// ---------------------------------------------------------------------------

#[test]
fn a_response_cut_short_is_an_error_not_half_a_status() {
    let script = transcript!("disconnect-mid-response.txt");
    let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");

    let err = mpd.status().expect_err("half a response is not a response");
    assert!(matches!(err, MpdError::Disconnected), "{err:?}");
    assert!(err.is_unreachable(), "the daemon went away: {err}");
}

#[test]
fn connecting_to_a_closed_port_fails_within_the_timeout() {
    // Bind and drop, so the port is one nothing is listening on.
    let closed = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let port = closed.local_addr().expect("its address").port();
    drop(closed);
    let addr = MpdAddress::parse(&format!("127.0.0.1:{port}"), 6600).expect("an address");

    let started = Instant::now();
    let err = Mpd::connect(&addr, TIMEOUT).expect_err("nothing is listening");
    let took = started.elapsed();

    assert!(matches!(err, MpdError::Connect { .. }), "{err:?}");
    assert!(err.is_unreachable(), "{err}");
    assert!(
        took < TIMEOUT * 10,
        "a refused connection should not wait: {took:?}"
    );
}

#[test]
fn a_daemon_that_accepts_and_says_nothing_times_out() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let addr = MpdAddress::parse(
        &listener.local_addr().expect("its address").to_string(),
        6600,
    )
    .expect("an address");
    // `listener` is never accepted from and never answers, and stays bound to the
    // end of the test: the kernel completes the handshake from its backlog, so the
    // read is what has to give up.

    let started = Instant::now();
    let err = Mpd::connect(&addr, TIMEOUT).expect_err("no greeting is coming");
    let took = started.elapsed();

    assert!(matches!(err, MpdError::Timeout), "{err:?}");
    assert!(
        took >= TIMEOUT && took < TIMEOUT * 10,
        "it should wait the timeout and then stop: {took:?}"
    );
}

#[test]
fn a_real_socket_that_closes_mid_response_is_an_error_not_a_hang() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let addr = MpdAddress::parse(
        &listener.local_addr().expect("its address").to_string(),
        6600,
    )
    .expect("an address");

    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("the client connects");
        socket
            .write_all(b"OK MPD 0.24.0\n")
            .expect("the greeting goes out");
        // Wait for the command, then die like a restarted daemon: half a response
        // and a closed socket.
        let mut buf = [0u8; 64];
        let _ = socket.read(&mut buf);
        socket
            .write_all(b"volume: 75\nstate: pa")
            .expect("half a response goes out");
        drop(socket);
    });

    let mut mpd = Mpd::connect(&addr, TIMEOUT).expect("the greeting arrives");
    let started = Instant::now();
    let err = mpd.status().expect_err("the daemon went away mid-response");
    let took = started.elapsed();

    assert!(matches!(err, MpdError::Disconnected), "{err:?}");
    assert!(
        took < TIMEOUT * 10,
        "a closed socket should not wait for the timeout: {took:?}"
    );
    server.join().expect("the fake daemon finishes");
}

// ---------------------------------------------------------------------------
// --no-mpd
// ---------------------------------------------------------------------------

/// A config whose roots are a throwaway fixture's, pointed at `addr`.
fn config_for(fx: &Fixture, addr: &MpdAddress, enabled: bool) -> Config {
    Config {
        mpd_address: addr.clone(),
        mpd_enabled: enabled,
        ..fx.config()
    }
}

#[test]
fn no_mpd_opens_no_socket() {
    let fx = Fixture::builder().build();
    let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let addr = MpdAddress::parse(
        &listener.local_addr().expect("its address").to_string(),
        6600,
    )
    .expect("an address");

    // `mpd_enabled: false` is what `--no-mpd` resolves to (task 04).
    let config = config_for(&fx, &addr, false);
    let connection =
        mpd::connect_if_enabled(&config, TIMEOUT, None).expect("not connecting cannot fail");
    assert!(connection.is_none(), "--no-mpd must not connect");

    // Nothing ever reached the listener. `accept` would otherwise block forever,
    // so ask without waiting.
    listener
        .set_nonblocking(true)
        .expect("a non-blocking listener");
    match listener.accept() {
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
        Ok((socket, from)) => panic!("--no-mpd opened a socket from {from}: {socket:?}"),
        Err(err) => panic!("unexpected listener error: {err}"),
    }
}

#[test]
fn mpd_enabled_does_connect_to_the_same_address() {
    // The other half of the previous test: without it, "no socket was opened"
    // could just as well mean the address was unreachable all along.
    let fx = Fixture::builder().build();
    let listener = TcpListener::bind("127.0.0.1:0").expect("an ephemeral port");
    let addr = MpdAddress::parse(
        &listener.local_addr().expect("its address").to_string(),
        6600,
    )
    .expect("an address");

    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("the client connects");
        socket
            .write_all(b"OK MPD 0.24.0\n")
            .expect("the greeting goes out");
        socket
    });

    let config = config_for(&fx, &addr, true);
    let mpd = mpd::connect_if_enabled(&config, TIMEOUT, None)
        .expect("the fake daemon greets us")
        .expect("mpd_enabled means connect");
    assert_eq!(mpd.version().to_string(), "0.24.0");

    let socket: TcpStream = server.join().expect("the fake daemon accepted");
    drop(socket);
}

// ---------------------------------------------------------------------------
// The other address form, and the password
// ---------------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn a_unix_socket_address_connects_too() {
    use std::os::unix::net::UnixListener;

    // MPD can be configured with `bind_to_address "/run/mpd/socket"` instead of a
    // port, and task 04 parses that into `MpdAddress::Unix`. This setup uses TCP,
    // so the only way to prove the other arm works is a socket of our own.
    let dir = tempfile::tempdir().expect("a temp directory");
    let path = dir.path().join("mpd.socket");
    let listener = UnixListener::bind(&path).expect("a unix socket");

    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("the client connects");
        socket
            .write_all(b"OK MPD 0.24.0\n")
            .expect("the greeting goes out");
        let mut buf = [0u8; 64];
        let read = socket.read(&mut buf).expect("the command arrives");
        assert_eq!(&buf[..read], b"ping\n");
        socket.write_all(b"OK\n").expect("the answer goes out");
    });

    let addr = MpdAddress::parse(path.to_str().expect("a UTF-8 temp path"), 6600)
        .expect("a socket address");
    assert!(matches!(addr, MpdAddress::Unix(_)), "{addr:?}");

    let mut mpd = Mpd::connect(&addr, TIMEOUT).expect("the fake daemon greets us");
    assert_eq!(mpd.version().to_string(), "0.24.0");
    mpd.ping().expect("and answers a ping");
    server.join().expect("the fake daemon finishes");
}

#[test]
fn an_abstract_socket_is_refused_with_a_reason() {
    // `@name` is Linux's abstract namespace, which MPD supports and `std` has no
    // stable way to connect to. Saying so beats failing with "no such file".
    let addr = MpdAddress::parse("@mpd", 6600).expect("an address");
    let err = Mpd::connect(&addr, TIMEOUT).expect_err("there is no way to connect");

    assert!(matches!(err, MpdError::Unsupported { .. }), "{err:?}");
    assert!(err.to_string().contains("@mpd"), "{err}");
}

#[test]
fn a_password_is_sent_quoted_and_never_appears_in_an_error() {
    // Hand-written: this setup has no password, so there is nothing to record.
    // The shape of both answers is captured in `acks.txt`.
    let script = Transcript::parse(
        "S: OK MPD 0.24.0\n\
         C: password \"a secret with a \\\" in it\"\n\
         S: OK\n\
         C: status\n\
         S: state: stop\n\
         S: OK\n",
    );
    let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");

    mpd.password(r#"a secret with a " in it"#)
        .expect("the daemon accepts it");
    // Authenticated, and the connection carries on as before.
    assert_eq!(mpd.status().expect("a status").state, PlayState::Stop);
    mpd.into_inner().assert_sent_the_script();
}

#[test]
fn a_password_that_cannot_be_sent_is_refused_without_quoting_it() {
    let script = Transcript::parse("S: OK MPD 0.24.0\n");
    let mut mpd = Mpd::handshake(script.stream()).expect("the greeting");

    let err = mpd
        .password("two\nlines")
        .expect_err("a newline would be a second command");
    assert!(matches!(err, MpdError::BadArgument { .. }), "{err:?}");
    assert!(
        !err.to_string().contains("two"),
        "the password must not reach the message: {err}"
    );
    assert!(mpd.into_inner().written().is_empty());
}
