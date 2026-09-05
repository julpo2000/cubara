//! Running a world with nobody watching.
//!
//! # What this is, and what it is not
//!
//! It is the authoritative loop of `docs/RESEARCH_MULTIPLAYER.md` §3.3's
//! standalone deployment: load the world, tick it at a fixed rate, save it, shut
//! down cleanly. It is what `cubara-server` and `cubara server` both run.
//!
//! It is **not networked**. Nothing listens on a port, and no client can
//! connect. The loop that will service a socket is this loop — the socket is
//! the part that does not exist. Until it does, a dedicated server is a world
//! that keeps running rather than a world people can join.
//!
//! Building it in this order is deliberate. §8.5: the seam is cheap to move
//! while there is no netcode and expensive afterwards, so every piece that can
//! be built and *tested* before the transport should be. This is one: a
//! headless tick loop is testable to the tick, where a networked one is testable
//! to the flake.
//!
//! # The loop
//!
//! [`Session`] is the pure half — it advances by a tick count and never reads a
//! clock, so a test can run ten thousand ticks in a millisecond. [`run`] is the
//! half with the clock in it, and it is thin on purpose: everything worth
//! testing is on the other side of that line.

use crate::assets;
use crate::clock::Pacer;
use crate::net::{Acceptor, Link};
use crate::wire::{ClientMessage, ServerMessage};
use crate::{Action, Server};
use cubara_sim::{PlayerId, PlayerInputs};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How the server was asked to run.
pub struct Config {
    /// Where the world is saved and loaded from.
    pub world: PathBuf,
    /// Ticks per second. 60 matches `cubara_sim::TICK_DT`, and anything else is
    /// a different game — a world running at 30 is not the same world at half
    /// speed, because block timings are counted in ticks.
    pub tps: u32,
    /// How often to write the world to disk, in ticks.
    pub autosave_ticks: u64,
    /// Stop after this many ticks instead of running until interrupted. What
    /// makes the loop testable, and what a smoke test in CI uses.
    pub run_ticks: Option<u64>,
    /// Where to listen for clients, if anywhere (block 2.12).
    ///
    /// `None` is a world that runs with nobody able to join -- which is what
    /// this binary did before there was a protocol, and still the right default:
    /// opening a port should be something someone asked for.
    pub listen: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            world: assets::world_dir(),
            tps: 60,
            // Five minutes at 60 tps. Frequent enough that a power cut costs a
            // session rather than a world; rare enough that a big world is not
            // serialising itself constantly.
            autosave_ticks: 60 * 60 * 5,
            run_ticks: None,
            listen: None,
        }
    }
}

/// A world being run headlessly, advanced in whole ticks and never by seconds.
///
/// The clock is [`run`]'s business, not this type's — which is what makes the
/// loop testable, and what keeps this file inside Rule 1.
pub struct Session {
    pub server: Server,
    /// Ticks run since this session started. Not the world's tick count, which
    /// is `server.sim.tick` and survives a restart; this one is uptime.
    pub ticks: u64,
    /// The tick at which the world was last written to disk.
    last_save: u64,
    /// Where new clients arrive, when this world is listening (block 2.12).
    acceptor: Option<Acceptor>,
    /// One link per connected client, keyed by the player it drives.
    ///
    /// A `BTreeMap` for the reason everything else in this project is: the order
    /// clients are serviced must be a property of the id, not of whose packet
    /// landed first (Rule 1).
    clients: BTreeMap<PlayerId, Link<ServerMessage, ClientMessage>>,
}

impl Session {
    /// Load every definition and the save at `cfg.world`, and report whether a
    /// world was restored or a fresh one generated.
    pub fn open(cfg: &Config) -> Self {
        let mut server = Server::new();
        let loaded = server.open(&cfg.world);
        log::info!(
            "{} ({})",
            if loaded {
                "world loaded"
            } else {
                "new world generated"
            },
            cfg.world.display()
        );
        Self {
            server,
            ticks: 0,
            last_save: 0,
            acceptor: None,
            clients: BTreeMap::new(),
        }
    }

    /// Start listening, and report the address actually bound.
    ///
    /// Returned rather than only logged because a caller that asked for port 0
    /// -- every test does -- has no other way to find out which port it got.
    pub fn listen(&mut self, addr: &str) -> std::io::Result<std::net::SocketAddr> {
        let acceptor = Acceptor::bind(addr)?;
        let bound = acceptor.addr();
        self.acceptor = Some(acceptor);
        Ok(bound)
    }

    /// How many clients are connected.
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// Take in whoever has connected, and give each of them a player.
    ///
    /// The server assigns the `PlayerId`; the client is told which one it got in
    /// [`ServerMessage::Welcome`]. Section 3.4 -- a client may never be believed
    /// -- made structural: there is no message a client could send that names
    /// itself.
    ///
    /// The welcome carries the **seed**, not the world. Terrain is a pure
    /// function of it, so a joining client generates its own; what follows is
    /// the edit overlay for what that client can actually see, which is
    /// `snapshot_for` and therefore already interest-filtered (block 2.11).
    fn accept_new_clients(&mut self) {
        let Some(acceptor) = self.acceptor.as_mut() else {
            return;
        };
        for mut link in acceptor.accepted() {
            let spawn = self.server.sim.player(self.server.local).spawn;
            let player = cubara_sim::Player::new(
                spawn,
                cubara_voxel::Angle::ZERO,
                cubara_voxel::Angle::ZERO,
            );
            let id = self.server.sim.join(player);
            self.server.open_view(id);

            link.send(ServerMessage::Welcome {
                seed: self.server.world.seed(),
                you: id,
            });
            let handshake = self.server.snapshot_for(id);
            log::info!(
                "player {id:?} joined; handshake is {} effects",
                handshake.len()
            );
            link.send(ServerMessage::Effects(handshake));
            // The view has already been charged for the handshake, so whatever
            // `open_view` queued must not be sent again.
            let _ = self.server.drain_effects_for(id);
            self.clients.insert(id, link);
        }
    }

    /// Read whatever every client sent, and turn it into this tick's input.
    ///
    /// Several inputs from one client in one tick collapse to the last: a client
    /// that sends faster than the world ticks does not get to move faster than
    /// it. Actions are applied in the order they arrived, per client, and
    /// clients are visited in id order.
    fn collect_input(&mut self) -> PlayerInputs {
        let mut inputs = PlayerInputs::default();
        let mut actions: Vec<(PlayerId, Action)> = Vec::new();
        let mut gone: Vec<PlayerId> = Vec::new();

        for (&who, link) in self.clients.iter_mut() {
            for msg in link.poll() {
                match msg {
                    ClientMessage::Hello => {} // already welcomed on accept
                    ClientMessage::Input(i) => inputs.set(who, i),
                    ClientMessage::Act(a) => actions.push((who, a)),
                }
            }
            if link.is_closed() {
                gone.push(who);
            }
        }

        for (who, action) in actions {
            self.server.apply_as(who, action);
        }
        for who in gone {
            self.drop_client(who);
        }
        inputs
    }

    /// A client has hung up: forget its link, its view, and its player.
    ///
    /// Everyone still watching is told, so nobody is left drawing a person who
    /// left. What happens to their *things* is a gameplay question nobody has
    /// answered, so their inventory goes with them rather than being invented
    /// onto the floor.
    fn drop_client(&mut self, who: PlayerId) {
        log::info!("player {who:?} disconnected");
        self.clients.remove(&who);
        self.server.close_view(who);
        self.server.sim.leave(who);
        self.server.announce_departure(who);
    }

    /// Hand each client what it is owed.
    fn flush_effects(&mut self) {
        for (&who, link) in self.clients.iter_mut() {
            let owed = self.server.drain_effects_for(who);
            if !owed.is_empty() {
                link.send(ServerMessage::Effects(owed));
            }
            link.send(ServerMessage::Tick(self.server.sim.tick));
        }
    }

    /// Advance the world by `ticks`, autosaving if enough have passed.
    ///
    /// **No input.** A dedicated server with nobody connected has no one
    /// pressing anything, and [`InputFrame::default`] is what "nobody is doing
    /// anything" means — not a special no-player code path, which would be a
    /// second implementation of the tick (Rule 5) and would drift.
    pub fn advance(&mut self, ticks: u64, cfg: &Config) {
        for _ in 0..ticks {
            self.accept_new_clients();
            let inputs = self.collect_input();
            self.server.tick_sim_all(&inputs);
            self.server.tick_world();
            self.flush_effects();
            self.ticks += 1;
        }
        if cfg.autosave_ticks > 0 && self.ticks - self.last_save >= cfg.autosave_ticks {
            self.save(&cfg.world);
        }
    }

    /// Write the world to disk and remember that we did.
    pub fn save(&mut self, dir: &Path) {
        self.server.save_to(dir);
        self.last_save = self.ticks;
    }

    /// One line of "is it alive and is it keeping up", for the operator.
    pub fn status(&self) -> String {
        format!(
            "tick {} (uptime {} ticks) — {} chunks simulating, {} block entities",
            self.server.sim.tick,
            self.ticks,
            self.server.world.chunk_states().active().count(),
            self.server.world.block_entity_positions().len(),
        )
    }
}

/// Run a world until `cfg.run_ticks` is reached, or forever.
///
/// The only function in this crate that knows what a second is, and it delegates
/// even that to [`Pacer`].
///
/// **Ctrl-C is not trapped.** Handling it means either a signal crate (a
/// dependency, which needs asking about) or a hand-rolled handler, and an
/// unhandled Ctrl-C loses at most `autosave_ticks` of world rather than the
/// world. `--ticks` is the clean shutdown that exists today, and it is the one
/// CI uses.
pub fn run(cfg: &Config) {
    let mut session = Session::open(cfg);
    let mut pacer = Pacer::new(cfg.tps);

    log::info!(
        "cubara-server: {} tps, autosave every {} ticks, world {}",
        cfg.tps,
        cfg.autosave_ticks,
        cfg.world.display()
    );
    match cfg.listen.as_deref() {
        Some(addr) => match session.listen(addr) {
            // **Printed, not only logged.** A caller that asked for port 0 --
            // every test does -- has to be told which port it got, and stdout is
            // the one channel a spawned process always has. The prefix is part
            // of the contract: `tests/two_processes.rs` parses this line.
            Ok(bound) => println!("cubara-server listening on {bound}"),
            Err(e) => {
                eprintln!("could not listen on {addr}: {e}");
                std::process::exit(1);
            }
        },
        None => log::info!("not listening — pass --listen <addr> to let clients connect"),
    }
    log::info!("{}", session.status());

    // A status line a minute: enough to see the world is alive, quiet enough to
    // leave running in a terminal.
    let status_every = cfg.tps as u64 * 60;
    let mut last_status = 0;
    let mut last_dropped = 0;

    loop {
        if let Some(limit) = cfg.run_ticks {
            if session.ticks >= limit {
                break;
            }
        }

        let mut owed = pacer.wait();
        if let Some(limit) = cfg.run_ticks {
            owed = owed.min(limit - session.ticks);
        }
        session.advance(owed, cfg);

        if pacer.dropped > last_dropped {
            log::warn!(
                "behind by {} ticks — the world is running slower than real time",
                pacer.dropped - last_dropped
            );
            last_dropped = pacer.dropped;
        }
        if session.ticks - last_status >= status_every {
            log::info!("{}", session.status());
            last_status = session.ticks;
        }
    }

    log::info!("{}", session.status());
    session.save(&cfg.world);
    log::info!("stopped after {} ticks", session.ticks);
}

/// Parse the dedicated server's arguments.
///
/// `args` is everything *after* the subcommand, so `cubara-server --tps 30` and
/// `cubara server --tps 30` hand this the identical slice. That is the point:
/// two entry points, one parser, one loop — Rule 5 applied to a CLI.
///
/// Hand-rolled rather than pulling in an argument-parsing crate: this is four
/// flags, and a new dependency is something to ask about, not to assume.
/// Unknown flags are an error rather than ignored — a typo'd `--tick 30`
/// silently running at 60 is the sort of thing an operator finds out about
/// hours later.
pub fn parse_args(args: &[String]) -> Result<Config, String> {
    let mut cfg = Config::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| -> Result<String, String> {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--world" => cfg.world = PathBuf::from(value("--world")?),
            "--tps" => {
                cfg.tps = value("--tps")?
                    .parse()
                    .map_err(|_| "--tps needs a number".to_string())?
            }
            "--autosave" => {
                cfg.autosave_ticks = value("--autosave")?
                    .parse()
                    .map_err(|_| "--autosave needs a number of ticks".to_string())?
            }
            "--ticks" => {
                cfg.run_ticks = Some(
                    value("--ticks")?
                        .parse()
                        .map_err(|_| "--ticks needs a number".to_string())?,
                )
            }
            "--listen" => cfg.listen = Some(value("--listen")?),
            "--help" | "-h" => return Err(USAGE.to_string()),
            other => return Err(format!("unknown option {other}\n\n{USAGE}")),
        }
    }
    if cfg.tps == 0 {
        return Err("--tps must be at least 1".to_string());
    }
    Ok(cfg)
}

/// What both entry points print.
pub const USAGE: &str = "\
Run a Cubara world with no window and no GPU.

  cubara-server [options]      the dedicated server
  cubara server [options]      the same thing, from the game binary

  --world <dir>     world directory (default: saves/world)
  --tps <n>         ticks per second (default: 60 — anything else is a
                    different game, since block timings are counted in ticks)
  --autosave <n>    save every n ticks, 0 to disable (default: 18000, 5 min)
  --ticks <n>       run n ticks and exit, instead of running until interrupted
  --listen <addr>   accept clients on <addr>, e.g. 0.0.0.0:25650 or
                    127.0.0.1:0 to be given a free port. Without it the world
                    runs but nobody can join.
  -h, --help        this

Note: connections are neither authenticated nor encrypted. Anyone who can reach
the port can join and can edit the world. Do not expose this to the internet
yet -- untrusted clients are block 2.14.";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cubara-headless-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn cfg_in(dir: PathBuf) -> Config {
        Config {
            world: dir,
            run_ticks: Some(0),
            ..Config::default()
        }
    }

    /// The claim the whole crate exists to make: a world runs, with no window,
    /// no adapter and no client.
    #[test]
    fn a_world_runs_headlessly() {
        let cfg = cfg_in(scratch("runs"));
        let mut s = Session::open(&cfg);
        let before = s.server.sim.tick;

        s.advance(600, &cfg); // ten seconds of world
        assert_eq!(s.ticks, 600);
        assert_eq!(
            s.server.sim.tick,
            before + 600,
            "the simulation advanced by exactly the ticks it was given"
        );
    }

    /// Ticking with nobody connected must not be a special code path — and the
    /// evidence is a furnace that smelts while nobody is watching it.
    ///
    /// This is the claim a dedicated server exists to make: the world does not
    /// stop when the player does. It runs through `tick_world`, which is the
    /// half a server with no players connected is entirely made of.
    #[test]
    fn a_furnace_smelts_with_nobody_playing() {
        let cfg = cfg_in(scratch("nobody"));
        let mut s = Session::open(&cfg);

        let items = s.server.items.as_ref().expect("assets are loaded");
        let raw_iron = items.id_of("cubara:raw_iron").expect("raw iron exists");
        let ingot = items.id_of("cubara:iron_ingot").expect("iron ingot exists");
        let plank = items.id_of("cubara:plank").expect("plank exists");

        // Right next to the player, so it is inside the simulation radius.
        let p = s.server.sim.player(s.server.local).pos.to_f32();
        let pos = [p[0] as i32 + 1, p[1] as i32, p[2] as i32];
        {
            let world = Arc::make_mut(&mut s.server.world);
            world.add_furnace(pos);
            let f = world.furnace_at_mut(pos).expect("just added");
            f.input = Some((raw_iron, 1));
            // A plank burns 20 ticks and the recipe wants 200, so this is
            // deliberate headroom rather than a round number.
            f.fuel = Some((plank, 20));
        }

        // The recipe is 200 ticks; give it a little room.
        s.advance(260, &cfg);

        let f = s.server.world.furnace_at(pos).expect("still there");
        assert_eq!(
            f.output,
            Some((ingot, 1)),
            "nobody was playing, and the iron still smelted"
        );
    }

    /// Determinism, from the server's own entry point (Rule 1). Two sessions
    /// opened on the same seed and ticked the same number of times are the same
    /// world, byte for byte as the hash sees it.
    #[test]
    fn two_headless_sessions_agree() {
        let a_cfg = cfg_in(scratch("det-a"));
        let b_cfg = cfg_in(scratch("det-b"));
        let mut a = Session::open(&a_cfg);
        let mut b = Session::open(&b_cfg);

        a.advance(300, &a_cfg);
        b.advance(300, &b_cfg);

        assert_eq!(
            a.server.hash(),
            b.server.hash(),
            "two servers ran the same world and disagreed"
        );
    }

    /// A restart is not a new world: what the server saved is what it comes
    /// back to. This is the dedicated-server version of #179, and it goes
    /// through `Session` rather than through `Game`, so it holds for a host
    /// that has no client at all.
    #[test]
    fn a_world_survives_a_restart() {
        let dir = scratch("restart");
        let cfg = Config {
            world: dir.clone(),
            run_ticks: Some(0),
            // Never autosave: this asserts the explicit save, so an autosave
            // firing would hide a broken one.
            autosave_ticks: 0,
            ..Config::default()
        };

        let mut first = Session::open(&cfg);
        first.advance(120, &cfg);
        // An edit only a save can carry: dig out the block under the player.
        let p = first.server.sim.player(first.server.local).pos.to_f32();
        let block = [p[0] as i32, p[1] as i32 - 2, p[2] as i32];
        first.server.break_at(block);
        let hash = first.server.hash();
        first.save(&dir);

        let second = Session::open(&cfg);
        assert_eq!(
            second.server.hash(),
            hash,
            "the world that came back is not the world that was saved"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `run` is the part with a clock in it, so it gets one test: that it
    /// terminates on `run_ticks` and leaves a save behind. Kept to a handful of
    /// ticks at a high rate so it costs milliseconds.
    #[test]
    fn run_stops_at_its_tick_limit_and_saves() {
        let dir = scratch("run");
        let cfg = Config {
            world: dir.clone(),
            tps: 10_000,
            run_ticks: Some(20),
            ..Config::default()
        };
        run(&cfg);
        assert!(
            dir.join("level.ron").exists(),
            "a clean shutdown writes the world"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_defaults_are_the_game_speed_and_the_game_world() {
        let cfg = parse_args(&[]).expect("no arguments is valid");
        assert_eq!(
            cfg.tps, 60,
            "60 tps is TICK_DT; anything else is a different game"
        );
        assert_eq!(cfg.world, assets::world_dir());
        assert_eq!(cfg.run_ticks, None, "a server runs until stopped");
    }

    #[test]
    fn every_flag_parses() {
        let args: Vec<String> = "--world /tmp/w --tps 30 --autosave 100 --ticks 5"
            .split(' ')
            .map(String::from)
            .collect();
        let cfg = parse_args(&args).expect("valid");
        assert_eq!(cfg.world, PathBuf::from("/tmp/w"));
        assert_eq!(cfg.tps, 30);
        assert_eq!(cfg.autosave_ticks, 100);
        assert_eq!(cfg.run_ticks, Some(5));
    }

    /// A typo must not silently run the default. `--tick 30` looks like it
    /// worked and is not what was asked for -- the operator finds out hours
    /// later, from the wrong numbers.
    #[test]
    fn a_typo_is_an_error_rather_than_a_default() {
        assert!(parse_args(&["--tick".into(), "30".into()]).is_err());
        assert!(
            parse_args(&["--tps".into()]).is_err(),
            "a flag with no value"
        );
        assert!(parse_args(&["--tps".into(), "nope".into()]).is_err());
        assert!(
            parse_args(&["--tps".into(), "0".into()]).is_err(),
            "zero tps never ticks"
        );
    }
}
