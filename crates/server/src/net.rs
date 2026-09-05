//! The transport: how messages get from one process to another, or from one
//! half of a process to the other.
//!
//! Block 2.12, designed in `docs/PHASE2_MULTIPLAYER.md` §2 and §5.
//!
//! # One type, two deployments
//!
//! §2's rule, and the reason this module is shaped the way it is:
//!
//! > Anything that is true only because the client and server share a process is
//! > a bug waiting for the socket. The in-process transport must be a real
//! > implementation of the same trait the socket implements — never a shortcut
//! > around it.
//!
//! So there is exactly one type, [`Link`], and it is a pair of channel ends. A
//! local link joins two halves of one process; a TCP link joins two machines.
//! Neither the server nor the client can tell which it has, because there is
//! nothing to tell: both are `Link<Outbound, Inbound>`.
//!
//! That is stronger than a trait with two implementations, which is what the
//! issue for this block asked for. A trait leaves two code paths that must be
//! kept in step; this leaves one, and moves the difference into how the channel
//! ends are wired up. Singleplayer is not a special case of multiplayer here —
//! it is multiplayer with a very short wire.
//!
//! # Why `std::sync::mpsc` and `std::net`
//!
//! No dependency, per the owner's standing constraint. `mpsc` gives the
//! non-blocking mailbox a tick loop wants, and TCP gives reliable ordered
//! delivery, which suits a protocol where a missed edit is a desync rather than
//! a dropped frame. UDP with a reliability layer is the better endpoint under
//! packet loss and is its own project (`RESEARCH_MULTIPLAYER.md` §3.2); it can
//! be added as another way of wiring a `Link` without anything above here
//! noticing.
//!
//! # What is deliberately not here
//!
//! **No authentication and no encryption.** A `Link` will carry whatever it is
//! given, to whoever connected. Untrusted clients are block 2.14, and saying so
//! plainly is better than a comment implying this is ready for the open
//! internet. It is not.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

use crate::wire::{frame, unframe, ClientMessage, ServerMessage, WireError};

/// One end of a conversation: send `S`, receive `R`.
///
/// The server holds `Link<ServerMessage, ClientMessage>`; a client holds the
/// mirror image. Nothing about the type says whether the other end is a function
/// call away or a network away, which is the point.
pub struct Link<S, R> {
    out: Sender<S>,
    inbox: Receiver<R>,
    /// Set once the far end has gone. Sticky: a link does not come back.
    closed: bool,
}

impl<S, R> Link<S, R> {
    /// Queue a message. Never blocks, and never fails loudly: a send to a link
    /// whose far end has gone marks it closed and drops the message, which is
    /// what actually happened.
    pub fn send(&mut self, msg: S) {
        if self.out.send(msg).is_err() {
            self.closed = true;
        }
    }

    /// Everything that has arrived since the last call. Never blocks.
    ///
    /// Returning a `Vec` rather than one message at a time is deliberate: a tick
    /// consumes a tick's worth of input, and a loop that took one message per
    /// tick would fall further behind the faster a client sent.
    pub fn poll(&mut self) -> Vec<R> {
        let mut got = Vec::new();
        loop {
            match self.inbox.try_recv() {
                Ok(m) => got.push(m),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.closed = true;
                    break;
                }
            }
        }
        got
    }

    /// Whether the far end has gone. Only ever observed through [`send`] or
    /// [`poll`]: a channel does not report a disconnect until it is asked.
    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

/// Two links wired to each other, in one process.
///
/// What singleplayer runs on. The round trip is a channel push, so it is
/// effectively free — and it goes through the same `send`/`poll` a socket does,
/// so nothing above can accidentally depend on it being free.
pub fn local_pair() -> (
    Link<ServerMessage, ClientMessage>,
    Link<ClientMessage, ServerMessage>,
) {
    let (to_client, from_server) = channel();
    let (to_server, from_client) = channel();
    (
        Link {
            out: to_client,
            inbox: from_client,
            closed: false,
        },
        Link {
            out: to_server,
            inbox: from_server,
            closed: false,
        },
    )
}

/// Wire a `Link` onto a TCP stream.
///
/// Two threads per connection: one reading frames off the socket into the
/// inbox, one draining the outbox onto it. The threads exist so neither `send`
/// nor `poll` can block the tick loop — a client on a slow connection must not
/// be able to stall the world for everyone else, which it would if the server
/// wrote to its socket inline.
///
/// Both threads end when their channel or the socket does, so dropping the
/// `Link` cleans up without a shutdown protocol.
fn wire_stream<S, R>(
    stream: TcpStream,
    encode: fn(&S) -> Vec<u8>,
    decode: fn(&[u8]) -> Result<R, WireError>,
) -> std::io::Result<Link<S, R>>
where
    S: Send + 'static,
    R: Send + 'static,
{
    // Nagle off: these are small messages on a tick clock, and coalescing them
    // trades latency for a bandwidth saving that does not matter here.
    stream.set_nodelay(true)?;
    let reader = stream.try_clone()?;
    let mut writer = stream;

    let (inbound_tx, inbox) = channel::<R>();
    let (out, outbound_rx) = channel::<S>();

    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break, // the far end hung up
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) => {
                    log::debug!("connection read failed: {e}");
                    break;
                }
            }
            // A read is not a message: it may hold several, or half of one.
            loop {
                match unframe(&buf) {
                    Ok(Some((payload, used))) => {
                        match decode(payload) {
                            Ok(msg) => {
                                if inbound_tx.send(msg).is_err() {
                                    return; // nobody is listening any more
                                }
                            }
                            Err(e) => {
                                // Malformed from the far end. Drop the
                                // connection rather than guess: block 2.14 will
                                // have more to say about who gets to send this.
                                log::warn!("undecodable message, closing connection: {e}");
                                return;
                            }
                        }
                        buf.drain(..used);
                    }
                    Ok(None) => break, // the rest is still in flight
                    Err(e) => {
                        log::warn!("bad framing, closing connection: {e}");
                        return;
                    }
                }
            }
        }
    });

    std::thread::spawn(move || {
        let mut framed = Vec::new();
        while let Ok(msg) = outbound_rx.recv() {
            framed.clear();
            frame(&encode(&msg), &mut framed);
            if writer.write_all(&framed).is_err() {
                break;
            }
        }
        let _ = writer.flush();
    });

    Ok(Link {
        out,
        inbox,
        closed: false,
    })
}

fn encode_server(m: &ServerMessage) -> Vec<u8> {
    let mut b = Vec::new();
    m.encode(&mut b);
    b
}

fn encode_client(m: &ClientMessage) -> Vec<u8> {
    let mut b = Vec::new();
    m.encode(&mut b);
    b
}

/// The server's side of one accepted connection.
pub fn serve(stream: TcpStream) -> std::io::Result<Link<ServerMessage, ClientMessage>> {
    wire_stream(stream, encode_server, ClientMessage::decode)
}

/// A client's side, connected to `addr`.
pub fn connect(addr: impl ToSocketAddrs) -> std::io::Result<Link<ClientMessage, ServerMessage>> {
    wire_stream(
        TcpStream::connect(addr)?,
        encode_client,
        ServerMessage::decode,
    )
}

/// A listener that hands over connections without blocking the tick loop.
///
/// `accept` on a `TcpListener` blocks, and a world must keep turning whether or
/// not anyone is knocking. So the accepting happens on its own thread and
/// arrives here as a queue the loop drains once a tick — the same shape as
/// everything else in this module, for the same reason.
pub struct Acceptor {
    incoming: Receiver<Link<ServerMessage, ClientMessage>>,
    addr: std::net::SocketAddr,
}

impl Acceptor {
    pub fn bind(addr: impl ToSocketAddrs) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        let addr = listener.local_addr()?;
        let (tx, incoming) = channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                match serve(stream) {
                    Ok(link) => {
                        if tx.send(link).is_err() {
                            return; // the server has shut down
                        }
                    }
                    Err(e) => log::warn!("could not set up a connection: {e}"),
                }
            }
        });
        Ok(Self { incoming, addr })
    }

    /// The address actually bound. Not the one asked for: a test binds port 0
    /// and needs to be told which port it got.
    pub fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }

    /// Whoever has connected since the last call.
    pub fn accepted(&mut self) -> Vec<Link<ServerMessage, ClientMessage>> {
        let mut got = Vec::new();
        while let Ok(link) = self.incoming.try_recv() {
            got.push(link);
        }
        got
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cubara_sim::PlayerId;

    /// The local link is a real link: messages go both ways through the same
    /// `send`/`poll` a socket uses.
    #[test]
    fn a_local_pair_carries_messages_both_ways() {
        let (mut server, mut client) = local_pair();

        client.send(ClientMessage::Hello);
        assert_eq!(server.poll(), vec![ClientMessage::Hello]);

        server.send(ServerMessage::Welcome {
            seed: 7,
            you: PlayerId(0),
        });
        assert_eq!(
            client.poll(),
            vec![ServerMessage::Welcome {
                seed: 7,
                you: PlayerId(0)
            }]
        );
    }

    /// Polling an empty link is not an error and does not block. A tick calls
    /// this every time round whether or not anything arrived.
    #[test]
    fn polling_an_idle_link_returns_nothing_and_does_not_block() {
        let (mut server, _client) = local_pair();
        assert!(server.poll().is_empty());
        assert!(!server.is_closed());
    }

    #[test]
    fn a_link_notices_when_the_far_end_goes_away() {
        let (mut server, client) = local_pair();
        drop(client);
        let _ = server.poll();
        assert!(server.is_closed(), "the server did not notice the hang-up");
    }

    /// Over a real socket, on localhost. Two threads rather than two processes
    /// -- the two-*process* test is `tests/two_processes.rs`, which is the gate
    /// criterion; this one is here to catch a framing bug without paying for a
    /// process spawn.
    #[test]
    fn a_tcp_link_carries_the_same_messages_a_local_one_does() {
        let mut acceptor = Acceptor::bind("127.0.0.1:0").expect("bind");
        let addr = acceptor.addr();

        let mut client = connect(addr).expect("connect");
        client.send(ClientMessage::Hello);

        // Accepting and delivery are on other threads, so this is the one place
        // the test has to wait for something.
        let mut server = None;
        for _ in 0..200 {
            if let Some(link) = acceptor.accepted().into_iter().next() {
                server = Some(link);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut server = server.expect("the connection was accepted");

        let mut got = Vec::new();
        for _ in 0..200 {
            got = server.poll();
            if !got.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(got, vec![ClientMessage::Hello], "over a real socket");
    }

    /// A batch bigger than one `read` must come back whole, in order. This is
    /// the bug the framing exists to prevent, and the one that only appears
    /// under load if it is not tested for.
    #[test]
    fn many_messages_at_once_survive_the_stream() {
        let mut acceptor = Acceptor::bind("127.0.0.1:0").expect("bind");
        let mut client = connect(acceptor.addr()).expect("connect");

        const N: u64 = 500;
        for i in 0..N {
            client.send(ClientMessage::Act(crate::Action::ClickFurnace {
                pos: [i as i32, 0, 0],
                slot: crate::FurnaceSlot::Input,
            }));
        }

        let mut server = None;
        for _ in 0..200 {
            if let Some(link) = acceptor.accepted().into_iter().next() {
                server = Some(link);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut server = server.expect("accepted");

        let mut all = Vec::new();
        for _ in 0..400 {
            all.extend(server.poll());
            if all.len() as u64 >= N {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        assert_eq!(all.len() as u64, N, "messages were lost across reads");
        for (i, m) in all.iter().enumerate() {
            let crate::wire::ClientMessage::Act(crate::Action::ClickFurnace { pos, .. }) = m else {
                panic!("wrong message came back: {m:?}");
            };
            assert_eq!(pos[0], i as i32, "messages arrived out of order");
        }
    }
}
