//! Phase 2's *"a real socket, two processes"* gate criterion (`ROADMAP.md`).
//!
//! Block 2.12. Everything else about the transport is tested inside one
//! process — `net.rs` has a loopback test, and `client_view.rs` drives two
//! clients through a `Server` directly. Those are worth having and they all
//! share one blind spot: nothing in them proves the **binary** works.
//!
//! So this test spawns the real `cubara-server` executable, connects to it over
//! a real TCP socket from this process, and completes a scripted exchange. Two
//! processes, one socket, no shared memory and no shared `Server`. If the
//! protocol, the framing, the argument parsing or the tick loop's servicing of
//! clients is wrong, this is where it shows.
//!
//! `CARGO_BIN_EXE_cubara-server` is Cargo's own path to the binary it just
//! built, so the test always exercises the current code rather than whatever is
//! on `$PATH`.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cubara_server::net::connect;
use cubara_server::wire::{ClientMessage, ServerMessage};
use cubara_sim::InputFrame;

/// Kills the server when the test ends, however it ends.
///
/// Without this a failing assertion leaves a world ticking in the background
/// for the rest of the run, holding its port and its save directory.
struct ServerProcess(Child);

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start `cubara-server --listen 127.0.0.1:0` and wait for it to say which port
/// it got.
///
/// Port 0 rather than a fixed number on purpose: a hard-coded port makes a test
/// that fails when the developer happens to be running the game, and makes two
/// copies of the suite unable to run at once.
fn start_server(world: &std::path::Path) -> (ServerProcess, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cubara-server"))
        .arg("--listen")
        .arg("127.0.0.1:0")
        .arg("--world")
        .arg(world)
        // Long enough to outlast the exchange, short enough that a wedged test
        // does not leave a process behind forever if the guard is bypassed.
        .arg("--ticks")
        .arg("3600")
        .arg("--autosave")
        .arg("0")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the cubara-server binary runs");

    let stdout = child.stdout.take().expect("stdout was piped");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();

    // The binary prints its bound address as its first line of stdout. Reading
    // it is also how we know the listener is actually up: connecting before
    // that is a race, and a sleep would be a slower, flakier way to lose it.
    reader
        .read_line(&mut line)
        .expect("the server announced itself");
    let addr = line
        .trim()
        .rsplit_once(' ')
        .map(|(_, a)| a.to_string())
        .unwrap_or_else(|| panic!("could not read an address out of {line:?}"));

    (ServerProcess(child), addr)
}

fn scratch_world(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cubara-two-process-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Poll a link until `want` says it has seen enough, or time runs out.
///
/// A deadline rather than a fixed number of sleeps: the server is a separate
/// process on a shared CI runner, and how long it takes to answer is not
/// something this test should pretend to know.
fn collect_until(
    link: &mut cubara_server::net::Link<ClientMessage, ServerMessage>,
    timeout: Duration,
    mut want: impl FnMut(&[ServerMessage]) -> bool,
) -> Vec<ServerMessage> {
    let deadline = Instant::now() + timeout;
    let mut all = Vec::new();
    while Instant::now() < deadline {
        all.extend(link.poll());
        if want(&all) {
            break;
        }
        assert!(!link.is_closed(), "the server closed the connection");
        std::thread::sleep(Duration::from_millis(10));
    }
    all
}

/// **The gate criterion.** A server process, a client process, one socket, and
/// a scripted exchange that completes.
#[test]
fn a_client_process_joins_a_server_process_over_a_real_socket() {
    let world = scratch_world("join");
    let (_server, addr) = start_server(&world);

    let mut link = connect(&addr).expect("connect to the server process");
    link.send(ClientMessage::Hello);

    // 1. The welcome. This is the whole of "the server never sends terrain":
    //    a seed and an id, and the client generates the world from the first.
    let messages = collect_until(&mut link, Duration::from_secs(20), |all| {
        all.iter()
            .any(|m| matches!(m, ServerMessage::Welcome { .. }))
    });

    let welcome = messages
        .iter()
        .find_map(|m| match m {
            ServerMessage::Welcome { seed, you, .. } => Some((*seed, *you)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no welcome arrived; got {messages:?}"));

    let (seed, me) = welcome;
    assert_ne!(
        seed, 0,
        "the welcome carried no seed to generate terrain from"
    );
    assert_ne!(
        me,
        cubara_sim::PlayerId::LOCAL,
        "a joining client was handed the server's own local player"
    );

    // 2. The world keeps turning, and says so. A `Tick` after the welcome is
    //    what proves the loop is servicing this client every tick rather than
    //    only at the handshake.
    link.send(ClientMessage::Input {
        seq: 0,
        frame: InputFrame::default(),
    });
    let messages = collect_until(&mut link, Duration::from_secs(20), |all| {
        all.iter().any(|m| matches!(m, ServerMessage::Tick(_)))
    });
    let ticks: Vec<u64> = messages
        .iter()
        .filter_map(|m| match m {
            ServerMessage::Tick(t) => Some(*t),
            _ => None,
        })
        .collect();
    assert!(
        !ticks.is_empty(),
        "the server never reported a tick; got {messages:?}"
    );
    assert!(
        ticks.windows(2).all(|w| w[1] >= w[0]),
        "tick numbers went backwards: {ticks:?}"
    );

    let _ = std::fs::remove_dir_all(&world);
}

/// Two clients, two connections, one server process — and each is given its own
/// player.
///
/// The single-client test above would pass just as happily if the server handed
/// every connection the same id, which is the kind of thing that only shows up
/// when the second person joins.
#[test]
fn two_client_processes_get_two_different_players() {
    let world = scratch_world("two-clients");
    let (_server, addr) = start_server(&world);

    let mut first = connect(&addr).expect("first client connects");
    first.send(ClientMessage::Hello);
    let id_of = |link: &mut cubara_server::net::Link<ClientMessage, ServerMessage>| {
        let messages = collect_until(link, Duration::from_secs(20), |all| {
            all.iter()
                .any(|m| matches!(m, ServerMessage::Welcome { .. }))
        });
        messages
            .iter()
            .find_map(|m| match m {
                ServerMessage::Welcome { you, .. } => Some(*you),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no welcome; got {messages:?}"))
    };
    let a = id_of(&mut first);

    let mut second = connect(&addr).expect("second client connects");
    second.send(ClientMessage::Hello);
    let b = id_of(&mut second);

    assert_ne!(a, b, "both connections were given the same player");

    let _ = std::fs::remove_dir_all(&world);
}

/// A server that was not asked to listen does not open a port.
///
/// Opening one by default would mean every world anyone ran was reachable, which
/// is a thing to do on purpose or not at all -- and doubly so while the protocol
/// has no authentication (block 2.14).
#[test]
fn a_server_without_listen_serves_nobody() {
    let world = scratch_world("silent");
    let child = Command::new(env!("CARGO_BIN_EXE_cubara-server"))
        .arg("--world")
        .arg(&world)
        .arg("--ticks")
        .arg("5")
        .arg("--autosave")
        .arg("0")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("runs");

    let out = child.wait_with_output().expect("the server exits");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("listening on"),
        "a server with no --listen announced a port: {stdout}"
    );
    assert!(out.status.success(), "the server did not exit cleanly");

    let _ = std::fs::remove_dir_all(&world);
}

/// **A dedicated server has nobody standing on spawn.**
///
/// Block 2.10 wrote "a dedicated server has many players and no local one" into
/// `Server::local`'s doc comment and then made the field a bare `PlayerId`, so
/// the sentence was a promise rather than a fact. A LAN test found the
/// consequence: a motionless `PlayerId(0)` on spawn, replicated to every client
/// every tick, that nobody was driving.
///
/// Checked from the client's side rather than by reading the server's fields,
/// because "what a client is told" is the thing that actually mattered.
#[test]
fn a_served_world_has_no_ghost_standing_on_spawn() {
    let world = scratch_world("no-ghost");
    let (_server, addr) = start_server(&world);

    let mut link = connect(&addr).expect("connect");
    link.send(ClientMessage::Hello);

    let messages = collect_until(&mut link, Duration::from_secs(20), |all| {
        // Wait for a few ticks to pass, so anyone who was going to be announced
        // has been.
        all.iter()
            .filter(|m| matches!(m, ServerMessage::Tick(_)))
            .count()
            >= 5
    });

    let strangers: Vec<cubara_sim::PlayerId> = messages
        .iter()
        .filter_map(|m| match m {
            ServerMessage::Effects(v) => Some(v.clone()),
            _ => None,
        })
        .flatten()
        .filter_map(|e| match e {
            cubara_server::Effect::PlayerMoved { who, .. } => Some(who),
            _ => None,
        })
        .collect();

    assert!(
        strangers.is_empty(),
        "the only client on a dedicated server was told about {} other player(s): \
         {strangers:?} — a dedicated server should have nobody but its clients",
        strangers.len()
    );
}

/// **A joining client is told about people who are already standing there.**
///
/// The join used to send `snapshot_for` and then discard whatever the client's
/// view had queued, on the grounds that the same edits must not be sent twice.
/// That was true when a view only backfilled edits. Once it backfilled *players*
/// too — and `snapshot_for` has never carried them — the discard threw those
/// away, so a joining client saw an empty world until somebody moved.
///
/// Two changes that were each correct on their own. This is the test that would
/// have caught the second one.
#[test]
fn a_joining_client_is_told_who_is_already_here() {
    let world = scratch_world("already-here");
    let (_server, addr) = start_server(&world);

    // Somebody is already in the world and standing perfectly still, which is
    // the case the bug hid behind: a motionless player generates no updates.
    let mut first = connect(&addr).expect("first connects");
    first.send(ClientMessage::Hello);
    let first_id = collect_until(&mut first, Duration::from_secs(20), |all| {
        all.iter()
            .any(|m| matches!(m, ServerMessage::Welcome { .. }))
    })
    .iter()
    .find_map(|m| match m {
        ServerMessage::Welcome { you, .. } => Some(*you),
        _ => None,
    })
    .expect("first welcome");

    let mut second = connect(&addr).expect("second connects");
    second.send(ClientMessage::Hello);
    let messages = collect_until(&mut second, Duration::from_secs(20), |all| {
        all.iter()
            .filter(|m| matches!(m, ServerMessage::Tick(_)))
            .count()
            >= 3
    });

    let seen: Vec<cubara_sim::PlayerId> = messages
        .iter()
        .filter_map(|m| match m {
            ServerMessage::Effects(v) => Some(v.clone()),
            _ => None,
        })
        .flatten()
        .filter_map(|e| match e {
            cubara_server::Effect::PlayerMoved { who, .. } => Some(who),
            _ => None,
        })
        .collect();

    assert!(
        seen.contains(&first_id),
        "a client that joined a world with somebody already in it was never told \
         they were there. Saw: {seen:?}"
    );
}
