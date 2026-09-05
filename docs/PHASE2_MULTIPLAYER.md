# Phase 2, multiplayer — the binding design

[`docs/RESEARCH_MULTIPLAYER.md`](RESEARCH_MULTIPLAYER.md) is *research*: it
surveys the three shapes, argues for one, and records what the codebase already
has. This document is the **design**, and its decisions are binding the way
[`PHASE2_ARCHITECTURE.md`](PHASE2_ARCHITECTURE.md)'s are — blocks 2.10 – 2.17
implement it, and a block that needs to contradict it changes it in the same PR,
with reasoning, rather than diverging quietly.

## §0 What the owner decided, and when

**2026-09-05.** Three answers, all of them the owner's to give:

1. **Multiplayer belongs to phase 2**, in full — including interest management
   and large player counts, not only a LAN transport.
2. **`RESEARCH_MULTIPLAYER.md` §3.6 is answered: one shared world.** The exact
   words were *"ik wil dat de structuur er is om 5000 spelers in 1 wereld aan te
   kunnen"* — **the structure must be able to carry it.** That is a requirement
   about architecture, not a promise to demonstrate five thousand live clients,
   and §6 below is careful about the difference.
3. **Phase 2's exit gate grows** to cover multiplayer, and goes red until it
   passes. Blocks 2.7a and 1.0 set that precedent deliberately: a gate's job is
   to say what is missing before the work is done.

The owner's reason for putting this in phase 2 rather than phase 3 was that
retrofitting multiplayer later would mean demolishing a lot. That is right, but
it is worth being precise about *which* part, because it changes the order of
work — see §1.

## §1 What is already built, and the one thing that is not

The demolition `RESEARCH_MULTIPLAYER.md` §8.5 warned about — *"the largest
refactor the project has attempted ... it cuts through the middle of the type
every gameplay test drives"* — **has already happened**, on purpose, while it was
still cheap:

| Prerequisite | State |
|---|---|
| Determinism, tick-driven, seeded RNG in world state | Rule 1, enforced |
| Simulation runs with no GPU | Rule 4, enforced by `check-architecture.sh` |
| `InputFrame` as a value, not a key snapshot | shipped |
| Client/server split, server in-process | #186 |
| `cubara-server` as its own crate, headless binary | #186 |
| Client-side replica world | `a9f238e` |
| `Action` / `Effect` as the messages that will cross a socket | shipped |
| Fixed-point positions and angles — nothing in the authority hash is a float | `24e4789`, `bbb09c2` |
| World-state hash, agreeing on Windows and macOS in CI | every merged PR |
| Save format, dormancy, bounded catch-up | 2.6 – 2.8 |

So steps 1 and 2 of `RESEARCH_MULTIPLAYER.md` §7 are done, and the seam is not
what needs demolishing.

**What does:** the simulation models exactly one player.

```rust
pub struct Sim {
    pub tick: u64,
    rng: WorldRng,
    pub player: Player,           // singular
    pub target: Option<[i32; 3]>, // also per-player
    pub entities: Entities,
}
```

`Sim.player` and `Sim.target` are singular, and they reach the world hash
(`hash.rs`), the save format (`save.rs`), and every gameplay path in
`cubara-server`. Making the world hold *N* players changes all three. That is
the structural change that gets more expensive the moment netcode sits on top of
it, and it is therefore **block 2.10, first, before any socket exists**.

## §2 The shape: authoritative server, one architecture, two deployments

Settled in `RESEARCH_MULTIPLAYER.md` §3 and not reopened here. Lockstep is ruled
out: cost per player is O(all players) and one bad connection stalls the world.

Private play runs the server **in-process**; public play runs the same server
binary standalone (§3.3). There is one `Server`, one protocol, and one code path.
A local client is a client whose transport happens to be a function call.

**The consequence that governs every block below:** anything that is true only
because the client and server share a process is a bug waiting for the socket.
The in-process transport must therefore be a real implementation of the same
trait the socket implements — never a shortcut around it.

## §3 What the server sends, and what it never sends

`RESEARCH_MULTIPLAYER.md` §3.4's table is binding. The load-bearing line:

> **The server never sends terrain. It sends the seed once, and edits thereafter.**

`WorldGen::density` is a pure function of `(seed, x, y, z)` and is proven
bit-identical on both CI platforms on every merged PR. A joining client generates
the world itself and applies an edit overlay — which is exactly what `World`
already is (`worldgen` + `edits`), and exactly what the client replica already
does.

Bandwidth then scales with **how much players have changed the world**, not with
how much world they can see. For a five-thousand target that is not an
optimisation; it is the difference between feasible and not. §6's gate criterion
asserts it rather than trusting it.

The rest of the table, restated as the rule underneath it:

> A client may simulate anything it can derive from data it already has, and may
> never be **believed** about any of it. Prediction is for latency, never for
> truth.

## §4 Rule 8 — no code assumes one `World` owns everything

`RESEARCH_MULTIPLAYER.md` §3.2 item 5 is the only item on its list that must be
**designed for early even if built late**:

> Region sharding across processes, once one machine's tick budget runs out. ...
> it requires that no code assume a single `World` owns everything.

Given the owner's answer — the structure must carry 5,000 in one world — this
stops being advice and becomes a constraint on every block below. `CLAUDE.md`
says what to do with a constraint like that:

> If a new rule has no answer to *"what fails when someone breaks this?"*, write
> a check rather than a paragraph.

So it becomes **`ARCHITECTURE.md` Rule 8**, with a check in
`scripts/check-architecture.sh`:

- No global or process-wide `World`. (Rule 2 already forbids globals; this is the
  specific case that matters most, and the check names it.)
- Nothing addresses a chunk by coordinate alone where it could be addressed by
  `(world, coordinate)`. A shard owns a region, not "the" region.
- No API takes "the world" implicitly. `Server` owns its `World`; a process may
  own several `Server`s. Rule 2 already makes that possible — this keeps it true.

**What fails when someone breaks it:** the day one machine's tick budget runs
out, sharding is a rewrite instead of a deployment change. That is exactly the
failure the owner is trying to avoid by putting this in phase 2.

## §5 The ordered blocks

Each is useful on its own, and each is testable **before** the one after it
exists. That ordering is deliberate and comes from §8.5: everything that can be
built and tested before the transport should be, because a headless tick loop is
testable to the tick where a networked one is testable to the flake.

| # | Block | Why here |
|---|---|---|
| **2.10** | **The world holds many players.** `PlayerId`, `Sim.players` in id order, `target` moves onto `Player`, the hash and the save format carry all of them. Singleplayer becomes one player with one id. | The structural change §1 identifies. Must land before netcode, and needs no netcode to land. |
| **2.11** | **The per-client view.** What one client has been told, and the delta owed to it. Interest management lives here: a client is sent only what it can perceive. Built and tested **in-process**, with no socket. | §3.2 items 2–3, the single largest determinant of whether the player-count target is reachable. Testable headlessly, so it is built where it is testable. |
| **2.12** | **The transport.** A `Transport` trait with two implementations — in-process and a socket — carrying `InputFrame`/`Action` up and `Effect`/state down. Two machines on a LAN: the Mac and the Windows laptop. | Only now, and behind a trait, so §2's rule holds. |
| **2.13** | **Prediction and reconciliation.** The client predicts its own player and its own edits, and reconciles against the server. | §8.4: *not* before there is latency, because a round trip that is always zero cannot show whether it works. |
| **2.14** | **Untrusted clients.** Every action validated server-side: reach, speed, inventory, rate. | What "public" actually costs (§3.2 item 6). |
| **2.15** | **Persistence that is not one `level.ron`.** Per-player state, and a chunk store a live server can write concurrently. | §3.2 item 4. One RON file is right for one player and wrong for a live server. |
| **2.16** | **Sharding, in one process.** Rule 8's check, plus two `Server`s owning disjoint worlds that cannot see each other's state, and a region's simulation moving between them. | §3.2 item 5. Built late, designed for from 2.10 — this is where "designed for" is made true rather than claimed. |
| **2.17** | **Distributed simulation across machines** (§7). A peer claims a region and keeps it active; handoff at boundaries; reclaim when a machine leaves; audit by replay. | The owner's *"elke speler zijn eigen gebied actief"*. Last, because it is a distributed-systems problem standing on a transport and an interest layer that must already work. |

Prediction is at 2.13 rather than earlier for the reason §8.4 gives, and interest
management is at 2.11 rather than after the transport because it is the part the
player-count answer depends on and the part that is cheapest to test with no
network in the way.

## §6 The exit gate, and the honest reading of "5000"

Phase 2's gate grows by four criteria. They go red on landing, as 2.7a's did.

1. **Two clients, one world, one hash.** Two clients join one server in-process,
   run a fixed scripted input sequence, and both replicas plus the server agree
   on the world-state hash. The multiplayer sibling of the survival replay.
2. **The server never sends terrain.** A joining client is given the seed and the
   edit overlay, and the join handshake is asserted to contain no terrain. §3's
   feasibility claim, checked rather than trusted.
3. **Bandwidth per client does not grow with player count.** The interest-
   management criterion, and the honest version of "5,000": with players spread
   across the world, bytes sent to *one* client per tick must not grow as the
   others are added. Measured at two counts an order of magnitude apart, on
   simulated clients.
4. **A real socket, two processes.** Not only the in-process transport: a
   server process and a client process on localhost, completing a scripted
   exchange.

**On criterion 3, and being straight about it.** The owner asked for *the
structure* to carry 5,000 in one world, not for a demonstration of 5,000 live
clients — which would need thousands of machines or a load generator that is its
own project. What is testable, and what actually decides feasibility, is the
**scaling shape**: O(what a client can perceive) rather than O(all players). A
test that shows bytes-per-client flat from 10 players to 1,000 is evidence the
structure holds; a live 5,000 test is not something a CI runner can give.

`RESEARCH_MULTIPLAYER.md` §3.2 is worth repeating here rather than leaving in the
research doc, because it is the thing most likely to be forgotten later:

> 5,000 concurrent players in one shared world is beyond what this genre normally
> achieves. Large public Minecraft servers run in the hundreds. ... "5,000
> players" is not a bigger version of "5 players" — it is a different engineering
> commitment, and it should be entered knowingly rather than discovered at step 5.

It is being entered knowingly. This document is that record.

## §7 Distributed simulation — the work follows the players

**Owner, 2026-09-05:** *"als 2 pcs een wereld runnen hoeven ze niet allebei alles
te doen. ze kunnen ook taken verspreiden zodat elke speler zijn eigen gebied
actief kan houden."*

This is region sharding (§3.2 item 5) with the shards placed on **player
machines** rather than only on server processes. It is admitted, and it is the
reason Rule 8 exists rather than being a nice-to-have.

### §7.1 Why it fits this engine unusually well

Three pieces it needs are already built, for other reasons:

- **`Active ⇄ Dormant` per chunk** (block 2.6). A shard boundary *is* a set of
  chunks somebody keeps active. The state machine that decides which chunks tick
  already exists, and `update_simulation_radius` already centres it on a player.
- **Bounded catch-up** (block 2.7a). A region nobody held for *k* ticks does not
  have to be simulated tick-by-tick when it is claimed — `advance` takes an
  elapsed count and is proved equal to ticking one at a time. Handing a region
  over is therefore not a stall.
- **Determinism** (Rule 1). Which gives the property below, and it is the one
  most games doing this do not have.

### §7.2 The advantage almost nobody else has: a shard's work is checkable

The usual objection to letting a player's machine simulate anything is §3.4's
rule — *a client may never be believed*. A peer authoritative over its own region
can conjure items, rewrite blocks, and no one is the wiser. That is the classic
peer-to-peer host problem and it is why most games do not do this publicly.

Here it is different in a way that matters. A shard's output is a **pure function
of** `(seed, the region's state at tick N, the inputs applied between N and N+k)`.
Every one of those is already a value this project can serialise, and the world
hash already covers exactly that state. So any other machine — the coordinator, a
peer, a CI runner — can **replay a shard's slice and compare hashes**.

That does not make cheating impossible. It makes it *detectable*, cheaply, and
after the fact. Which changes the question from "can a peer be trusted" to "how
often is a peer audited", and that is a policy dial rather than an architecture.

### §7.3 Trust is a deployment property, not a second architecture

§3.3's principle applied again, so that the hard question does not have to be
answered before the work starts:

- **A shard runs wherever it is placed.** The code does not know whether it is on
  the coordinator, another server process, or a player's laptop.
- **Whether its output is believed is policy on the coordinator.** Private play
  among friends trusts its peers and gets their CPU for free. Public play either
  audits by replay (§8.2), or places shards only on machines it controls. Same
  binary, same protocol, one dial.

This is deliberately *not* a decision that peer-sharding is safe for public play.
It is a decision that the architecture must not foreclose either answer, so the
answer can be made from measurements instead of guesses.

### §7.4 The three genuinely hard parts, named

Being straight about the cost, because §3.2 warns that this is the item that is a
distributed-systems project rather than a netcode one:

1. **Handoff at boundaries.** A player walking from region A to region B moves
   authority for those chunks between machines. An entity crossing a boundary
   must not be duplicated or dropped. A furnace exactly on the seam has one owner
   and must keep having exactly one.
2. **Reclaim on failure.** A machine that crashes or quits was holding a region.
   Its state must be recoverable — which means a shard's state is replicated or
   checkpointed to the coordinator, not only held in its own RAM. Without this,
   a peer disconnecting eats part of the world.
3. **Nondeterminism has nowhere to hide.** Two machines simulating adjacent
   regions must agree about the seam. Rule 1 is what makes this tractable, and it
   is also what makes any violation of Rule 1 catastrophic here rather than
   merely annoying — which is worth knowing before, not after.

None of the three is a reason not to do it. All three are reasons it is the
**last** block, built on top of a transport and an interest-management layer that
already work, rather than the first.

## §8 What this does not decide

- **How many players a single `Server` process should hold** before sharding is
  the answer. That is a measurement, and 2.11's scaling test is what will produce
  the number. Guessing it now would be inventing.
- **Identity and accounts.** Block 2.14 needs *a* notion of who a client is; who
  is allowed to connect, and how that is proved, is a product decision nobody has
  made.
- **Whether shards run on one host or many.** 2.16 makes sharding possible; it
  does not choose a deployment.
- **Mobs.** Deferred to phase 3 with block 2.9b, and nothing here should be
  written as though they exist.
