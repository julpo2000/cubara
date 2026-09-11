//! A connection becomes a player only when it says who it is (#232).
//!
//! Seating used to happen on `accept`: the moment a TCP connection arrived it
//! was given a `PlayerId`, a body on spawn and a per-client view, before a
//! single byte had been read from it. A port scan made inhabitants.
//!
//! These tests drive a real `Acceptor` in-process rather than spawning the
//! binary -- `two_processes.rs` covers the binary, and what is being tested here
//! is a rule about the tick loop, which is testable to the tick and not to the
//! flake.
//!
//! **What they assert is the state of the world, never a log line.** A version
//! that still seated the connection and merely stopped announcing it would pass
//! a test written against the message; `player_count` is the behaviour.

use std::io::Write;
use std::net::TcpStream;

use cubara_server::headless::{Config, Session, HELLO_DEADLINE_TICKS, MAX_PENDING};

const TEST_SHIRT: cubara_server::wire::Shirt = [10, 20, 30];

fn listening() -> (Session, Config, std::net::SocketAddr) {
    let cfg = Config {
        world: std::path::PathBuf::from("cubara-nonexistent-handshake-fixture"),
        autosave_ticks: 0,
        ..Config::default()
    };
    let mut s = Session::open(&cfg);
    // Port 0: a hard-coded port fails whenever the developer is running the
    // game, and stops two copies of the suite running at once.
    let addr = s.listen("127.0.0.1:0").expect("bound a port");
    (s, cfg, addr)
}

/// Run the loop until `done` holds, or give up after `budget`.
///
/// **Waits in wall-clock time, deliberately, and this is the one place in the
/// suite that may.** A connection arrives when the operating system's accept
/// thread is scheduled, which is not a number of ticks -- the first version of
/// this helper spun a fixed 200 ticks, finished in microseconds, and three
/// tests failed because nothing had been accepted yet. Ticking faster does not
/// make the kernel hurry.
///
/// Rule 1 is untouched: the *world* still advances a tick at a time and no
/// game state depends on the clock. What is being waited for here is the
/// socket, which is why `two_processes.rs` waits the same way.
fn advance_until(
    s: &mut Session,
    cfg: &Config,
    budget: std::time::Duration,
    done: impl Fn(&Session) -> bool,
) {
    let deadline = std::time::Instant::now() + budget;
    loop {
        s.advance(1, cfg);
        if done(s) {
            return;
        }
        if std::time::Instant::now() > deadline {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Long enough that a loaded CI runner still accepts; short enough that a real
/// failure is a failure rather than a hang.
const SETTLE: std::time::Duration = std::time::Duration::from_secs(5);

/// **The defect, exactly.** Open a connection, say nothing, close it.
///
/// Found by accident: a `/dev/tcp` probe to check a LAN route was up appeared
/// in the server log as a player joining and leaving.
#[test]
fn a_connection_that_says_nothing_never_becomes_a_player() {
    let (mut s, cfg, addr) = listening();
    assert_eq!(s.server.sim.player_count(), 0, "the world starts empty");

    let stream = TcpStream::connect(addr).expect("connected");
    advance_until(&mut s, &cfg, SETTLE, |s| s.pending_count() == 1);

    assert_eq!(
        s.pending_count(),
        1,
        "the connection was never accepted, so this test proves nothing"
    );
    assert_eq!(
        s.server.sim.player_count(),
        0,
        "a connection that has said nothing was given a player"
    );

    drop(stream);
    advance_until(&mut s, &cfg, SETTLE, |s| s.pending_count() == 0);

    assert_eq!(
        s.pending_count(),
        0,
        "the closed connection was not forgotten"
    );
    assert_eq!(
        s.server.sim.player_count(),
        0,
        "closing the connection left a player behind"
    );
}

/// And it costs no id either.
///
/// Separate from the test above because "no player" and "no id" can come apart:
/// a version that reserved the id up front and seated the body later would pass
/// the first assertion and fail this one. Ids climbing for anyone who can reach
/// the port is half of what #232 is about.
#[test]
fn a_silent_connection_burns_no_player_id() {
    let (mut s, cfg, addr) = listening();

    let silent = TcpStream::connect(addr).expect("connected");
    advance_until(&mut s, &cfg, SETTLE, |s| s.pending_count() == 1);
    drop(silent);
    advance_until(&mut s, &cfg, SETTLE, |s| s.pending_count() == 0);

    // Now a client that does introduce itself. It should get the first id the
    // world ever hands out.
    let mut real = cubara_server::net::connect(addr).expect("connected");
    real.send(cubara_server::wire::ClientMessage::Hello(TEST_SHIRT));
    advance_until(&mut s, &cfg, SETTLE, |s| s.server.sim.player_count() == 1);

    let ids = s.server.sim.player_ids();
    assert_eq!(
        ids,
        vec![cubara_sim::PlayerId(1)],
        "the silent connection consumed an id before it said anything"
    );
}

/// Saying `Hello` is what seats you -- and the colour arrives with the seat.
///
/// The other half of the rule. Without this the suite could pass with a server
/// that never seats anybody at all.
#[test]
fn a_connection_that_says_hello_becomes_a_player() {
    let (mut s, cfg, addr) = listening();

    let mut client = cubara_server::net::connect(addr).expect("connected");
    client.send(cubara_server::wire::ClientMessage::Hello(TEST_SHIRT));
    advance_until(&mut s, &cfg, SETTLE, |s| s.server.sim.player_count() == 1);

    assert_eq!(
        s.server.sim.player_count(),
        1,
        "a client that introduced itself was not seated"
    );
    assert_eq!(s.pending_count(), 0, "a seated client is still queued");
    let id = s.server.sim.player_ids()[0];
    assert_eq!(
        s.server.shirt_of(id),
        Some(TEST_SHIRT),
        "the colour announced at the join was not recorded with the seat"
    );
}

/// A connection that stays open and stays silent is dropped, not held forever.
///
/// The closed-socket case cleans itself up; this one would not. Ten seconds of
/// ticks, which is the same patience the client has for its `Welcome`.
#[test]
fn a_connection_that_never_speaks_is_dropped_at_the_deadline() {
    let (mut s, cfg, addr) = listening();

    // Held open deliberately: dropping it would test the other path.
    let _stream = TcpStream::connect(addr).expect("connected");
    advance_until(&mut s, &cfg, SETTLE, |s| s.pending_count() == 1);
    assert_eq!(
        s.pending_count(),
        1,
        "nothing was waiting, so nothing is proved"
    );

    s.advance(u64::from(HELLO_DEADLINE_TICKS) + 1, &cfg);

    assert_eq!(
        s.pending_count(),
        0,
        "a silent connection was held past its deadline"
    );
    assert_eq!(s.silent_connections(), 1, "the drop was not counted");
    assert_eq!(
        s.server.sim.player_count(),
        0,
        "the silent connection became a player on its way out"
    );
}

/// Connections waiting to introduce themselves are capped.
///
/// Without a cap, the cheap half of the exchange is the other party's: a socket
/// costs them almost nothing and used to cost this server a player and a view.
#[test]
fn connections_beyond_the_limit_are_refused() {
    let (mut s, cfg, addr) = listening();

    // Held in a vec so none of them close early; a closed one would free a slot
    // and the cap would never be reached.
    let mut held = Vec::new();
    for _ in 0..MAX_PENDING + 8 {
        match TcpStream::connect(addr) {
            Ok(mut c) => {
                // Not a `Hello` -- enough traffic to be a real connection, not
                // enough to be an introduction.
                let _ = c.write_all(&[]);
                held.push(c);
            }
            // The listen backlog is the operating system's business and varies;
            // a refused connect here is not this server's decision.
            Err(_) => break,
        }
    }

    advance_until(&mut s, &cfg, SETTLE, |s| s.refused_connections() > 0);

    assert!(
        s.refused_connections() > 0,
        "{} connections were opened and none were refused; the cap did not bind",
        held.len()
    );
    assert!(
        s.pending_count() <= MAX_PENDING,
        "{} connections were queued, above the cap of {MAX_PENDING}",
        s.pending_count()
    );
    assert_eq!(
        s.server.sim.player_count(),
        0,
        "connections that never introduced themselves became players"
    );
}
