//! Accessibility event socket.
//!
//! tiny-dfr runs as an unprivileged system service and has no access to the
//! user's session, so it cannot talk to a screen reader itself. Instead it
//! publishes what happens on the Touch Bar on a Unix socket. A small helper
//! in the user's session (`tiny-dfr-speech`) connects to it and passes the
//! announcements on to Orca or Speech Dispatcher.
//!
//! The protocol is one JSON object per line, server to client only:
//!
//! ```text
//! {"event":"hello","version":1}
//! {"event":"focus","label":"Volume up"}
//! {"event":"activate","label":"Volume up"}
//! {"event":"layer","label":"Media keys"}
//! ```
//!
//! While at least one client is connected, the bar is in touch exploration
//! mode (see `explore.rs`), so connecting changes how the bar behaves, and
//! clients learn which Touch Bar buttons get pressed. Only root and the user
//! currently active on a local seat may be connected; a client whose user
//! switches away is dropped.

use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
use serde::Serialize;
use std::{
    fs,
    io::{ErrorKind, Read, Write},
    os::{
        fd::{AsFd, BorrowedFd},
        unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::Path,
    time::{Duration, Instant},
};

pub const SOCKET_PATH: &str = "/run/tiny-dfr/a11y.sock";
const LOGIND_USERS_DIR: &str = "/run/systemd/users";
const PROTOCOL_VERSION: u32 = 1;
const MAX_CLIENTS: usize = 4;
/// Clients have nothing to say to us. Anything beyond this many bytes per
/// wakeup gets them disconnected so they cannot keep the daemon spinning.
const MAX_CLIENT_CHATTER: usize = 256;
/// How often connected clients are checked to still be the active user.
const RECHECK_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Serialize)]
struct Message<'a> {
    event: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<u32>,
}

struct Client {
    stream: UnixStream,
    uid: u32,
}

pub struct A11yServer {
    listener: Option<UnixListener>,
    clients: Vec<Client>,
    last_recheck: Instant,
}

/// Does the logind state file for a user say they are active on a local seat?
fn has_active_seat(state: &str) -> bool {
    state
        .lines()
        .filter_map(|line| line.strip_prefix("ACTIVE_SEATS="))
        .any(|seats| !seats.trim().is_empty())
}

fn uid_allowed(uid: u32) -> bool {
    if uid == 0 {
        return true;
    }
    if !Path::new(LOGIND_USERS_DIR).is_dir() {
        // No logind to ask; fall back to the socket's file permissions.
        return true;
    }
    fs::read_to_string(format!("{LOGIND_USERS_DIR}/{uid}"))
        .map(|state| has_active_seat(&state))
        .unwrap_or(false)
}

fn bind(path: &Path) -> std::io::Result<UnixListener> {
    // Under systemd the directory comes from RuntimeDirectory=; create it
    // ourselves otherwise. This runs as root, before privileges are dropped.
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    match fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let listener = UnixListener::bind(path)?;
    // Any local user may reach the socket; who is let in is decided per
    // connection in uid_allowed().
    fs::set_permissions(path, fs::Permissions::from_mode(0o666))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

impl A11yServer {
    /// Must be called before privileges are dropped. Failure is not fatal:
    /// the Touch Bar simply works without accessibility output.
    pub fn new() -> A11yServer {
        let listener = bind(Path::new(SOCKET_PATH))
            .inspect_err(|e| eprintln!("Accessibility socket unavailable: {e}"))
            .ok();
        A11yServer {
            listener,
            clients: Vec::new(),
            last_recheck: Instant::now(),
        }
    }

    pub fn listener_fd(&self) -> Option<BorrowedFd<'_>> {
        self.listener.as_ref().map(|l| l.as_fd())
    }

    pub fn has_clients(&self) -> bool {
        !self.clients.is_empty()
    }

    /// Accept waiting connections and drop clients that went away. `accept`
    /// is false when the feature is switched off in the configuration.
    /// `ready` says whether any of our sockets woke the caller's event loop;
    /// without it this costs no system calls, which matters because the loop
    /// also wakes for every keyboard and pointer event. `register` is called
    /// with each new client so the caller can add it to its poll set.
    pub fn poll(&mut self, accept: bool, ready: bool, mut register: impl FnMut(BorrowedFd<'_>)) {
        if !accept {
            self.clients.clear();
        }
        if !self.clients.is_empty() && self.last_recheck.elapsed() >= RECHECK_INTERVAL {
            self.last_recheck = Instant::now();
            self.clients.retain(|client| uid_allowed(client.uid));
        }
        if !ready {
            return;
        }
        if let Some(listener) = &self.listener {
            // Bounded so a connection flood cannot stall the event loop.
            for _ in 0..MAX_CLIENTS * 2 {
                let Ok((stream, _)) = listener.accept() else {
                    break;
                };
                if !accept || self.clients.len() >= MAX_CLIENTS {
                    continue;
                }
                let Ok(uid) = getsockopt(&stream, PeerCredentials).map(|cred| cred.uid()) else {
                    continue;
                };
                if !uid_allowed(uid) || stream.set_nonblocking(true).is_err() {
                    continue;
                }
                let mut stream = stream;
                let hello = Self::encode(&Message {
                    event: "hello",
                    label: None,
                    version: Some(PROTOCOL_VERSION),
                });
                if !matches!(stream.write(&hello), Ok(n) if n == hello.len()) {
                    continue;
                }
                register(stream.as_fd());
                self.clients.push(Client { stream, uid });
            }
        }
        // Closing a stream also removes it from the caller's epoll set.
        self.clients.retain_mut(|client| {
            let mut buf = [0u8; MAX_CLIENT_CHATTER];
            match client.stream.read(&mut buf) {
                Ok(0) => false,
                Ok(n) => n < MAX_CLIENT_CHATTER,
                Err(e) => e.kind() == ErrorKind::WouldBlock,
            }
        });
    }

    fn encode(msg: &Message) -> Vec<u8> {
        // Serialising a struct of strings and integers cannot fail.
        let mut line = serde_json::to_vec(msg).unwrap();
        line.push(b'\n');
        line
    }

    /// Send an event to every client. A client that is not keeping up is
    /// dropped rather than buffered for.
    pub fn send(&mut self, event: &str, label: &str) {
        if self.clients.is_empty() {
            return;
        }
        let line = Self::encode(&Message {
            event,
            label: Some(label),
            version: None,
        });
        self.clients
            .retain_mut(|client| matches!(client.stream.write(&line), Ok(n) if n == line.len()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_with(listener: Option<UnixListener>, client: Option<UnixStream>) -> A11yServer {
        A11yServer {
            listener,
            // uid 0 always passes the periodic recheck.
            clients: client
                .into_iter()
                .map(|stream| Client { stream, uid: 0 })
                .collect(),
            last_recheck: Instant::now(),
        }
    }

    #[test]
    fn active_seat_detection() {
        assert!(has_active_seat(
            "NAME=u\nSTATE=active\nACTIVE_SEATS=seat0\nONLINE_SEATS=seat0\n"
        ));
        // Logged in over ssh only: active, but on no seat.
        assert!(!has_active_seat(
            "NAME=u\nSTATE=active\nACTIVE_SEATS=\nONLINE_SEATS=\n"
        ));
        assert!(!has_active_seat("NAME=u\nSTATE=lingering\n"));
        assert!(!has_active_seat("XACTIVE_SEATS=seat0\n"));
    }

    #[test]
    fn messages_are_single_json_lines() {
        let line = A11yServer::encode(&Message {
            event: "focus",
            label: Some("say \"hi\"\nnow"),
            version: None,
        });
        assert_eq!(
            String::from_utf8(line).unwrap(),
            "{\"event\":\"focus\",\"label\":\"say \\\"hi\\\"\\nnow\"}\n"
        );
    }

    #[test]
    fn events_reach_clients_and_dead_clients_are_dropped() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        let mut server = server_with(None, Some(ours));
        server.send("focus", "F1");
        let mut buf = [0u8; 64];
        let n = theirs.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"{\"event\":\"focus\",\"label\":\"F1\"}\n");

        server.poll(true, true, |_| {});
        assert!(server.has_clients());
        drop(theirs);
        server.poll(true, true, |_| {});
        assert!(!server.has_clients());
    }

    #[test]
    fn accepts_greets_and_refuses_when_disabled() {
        let dir = std::env::temp_dir().join(format!("tiny-dfr-test-{}", std::process::id()));
        let path = dir.join("sub/a11y.sock");
        let mut server = server_with(Some(bind(&path).unwrap()), None);
        // Rebinding over a stale socket must work (daemon restart).
        server.listener = Some(bind(&path).unwrap());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o666
        );

        // We are either root or have no logind record saying otherwise, so
        // whether we get in depends on the environment; check both outcomes.
        let me = std::os::unix::fs::MetadataExt::uid(&fs::metadata(&dir).unwrap());
        let mut client = UnixStream::connect(&path).unwrap();
        let mut registered = 0;
        server.poll(true, true, |_| registered += 1);
        assert_eq!(server.has_clients(), uid_allowed(me));
        if server.has_clients() {
            assert_eq!(registered, 1);
            let mut buf = [0u8; 64];
            let n = client.read(&mut buf).unwrap();
            assert_eq!(&buf[..n], b"{\"event\":\"hello\",\"version\":1}\n");
        }

        // Switched off in the config: existing clients go, new ones bounce.
        let mut late = UnixStream::connect(&path).unwrap();
        server.poll(false, true, |_| panic!("no client should be registered"));
        assert!(!server.has_clients());
        assert_eq!(late.read(&mut [0u8; 8]).unwrap(), 0);

        // Never more than MAX_CLIENTS at once.
        if uid_allowed(me) {
            let _conns: Vec<_> = (0..MAX_CLIENTS + 2)
                .map(|_| UnixStream::connect(&path).unwrap())
                .collect();
            server.poll(true, true, |_| {});
            assert_eq!(server.clients.len(), MAX_CLIENTS);
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clients_whose_user_is_no_longer_active_are_dropped() {
        if !Path::new(LOGIND_USERS_DIR).is_dir() {
            return; // without logind everyone is allowed
        }
        let (ours, _theirs) = UnixStream::pair().unwrap();
        let mut server = server_with(None, Some(ours));
        // No logind record can exist for this uid.
        server.clients[0].uid = u32::MAX - 1;
        server.poll(true, false, |_| {});
        assert!(server.has_clients(), "not rechecked before the interval");
        server.last_recheck = Instant::now() - RECHECK_INTERVAL;
        server.poll(true, false, |_| {});
        assert!(!server.has_clients());
    }

    #[test]
    fn idle_poll_leaves_pending_work_alone() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        let mut server = server_with(None, Some(ours));
        drop(theirs);
        server.poll(true, false, |_| {});
        assert!(server.has_clients());
        server.poll(true, true, |_| {});
        assert!(!server.has_clients());
    }

    #[test]
    fn chatty_clients_are_dropped() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        let mut server = server_with(None, Some(ours));
        theirs.write_all(&[b'x'; 1024]).unwrap();
        server.poll(true, true, |_| {});
        assert!(!server.has_clients());
    }
}
