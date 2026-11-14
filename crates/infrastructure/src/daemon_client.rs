//! A client's connection to the daemon: the adapter for
//! `udm_application::daemon::DaemonClient` over `ipc` and `protocol`,
//! for the desktop window (`new`), for `udm-cli` (`program`) and for
//! `udm-mcp` (`assistant`).
//!
//! It connects (starting or restarting the daemon as `ipc` does, with its
//! handshake), introduces itself, maps each `Command` to an `Incoming`
//! frame and each `Outgoing` frame to an `Event`, and says when the
//! connection goes.

use std::io::{self, ErrorKind};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use udm_domain::sync::LockExt;

use udm_application::daemon::{Command, DaemonClient, DaemonError, Event, Lost};
use udm_domain::download::CommandLine;

use crate::app::MAX_MESSAGE_BYTES;
use crate::ipc;
use crate::protocol::{read_message, write_frame, Incoming, Outgoing};
use crate::wire::{self, event, incoming};

/// How long a command line waits for the daemon's answer.
const COMMAND_LINE_TIMEOUT: Duration = Duration::from_secs(10);

/// Opens a connection to the daemon, past its handshake; `true` restarts
/// the daemon first.
pub type Connector = dyn Fn(bool) -> io::Result<TcpStream> + Send + Sync;

/// A connection to the daemon over loopback TCP.
pub struct IpcDaemonClient {
    /// The `client` this connection names in its `hello`.
    client: String,
    /// The connection to write to, while there is one.
    writer: Arc<Mutex<Option<TcpStream>>>,
    connector: Arc<Connector>,
    /// Which events the client hears.
    hears: fn(&Event) -> bool,
}

/// Lock `m`. The guarded value is a plain stream slot, so one a panicking
/// thread left behind is still usable.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock_or_recover()
}

impl IpcDaemonClient {
    /// A connection that names itself `client` (the desktop window says
    /// `window`), starting the daemon when none runs.
    pub fn new(client: &str) -> Self {
        Self::with_connector(
            client,
            Arc::new(|restart| if restart { ipc::restart_daemon() } else { ipc::connect_or_spawn() }),
        )
    }

    /// A program's connection that names itself `client` and hears every
    /// event, the replies a window leaves out (`ack`, `hello`)
    /// among them, starting the daemon when none runs.
    pub fn program(client: &str) -> Self {
        Self::new(client).hearing(|_| true)
    }

    /// A program's connection that stays open between its requests, as
    /// `udm-mcp`'s does: it hears what `program` hears but downloads'
    /// progress, which nobody follows there and which would pile up unread
    /// between requests.
    pub fn assistant(client: &str) -> Self {
        Self::new(client).hearing(|e| !matches!(e, Event::Progress { .. } | Event::TorrentStats { .. }))
    }

    /// This connection, hearing the events `hears` lets through.
    fn hearing(mut self, hears: fn(&Event) -> bool) -> Self {
        self.hears = hears;
        self
    }

    /// A window's connection made by `connector` instead of `ipc`.
    pub fn with_connector(client: &str, connector: Arc<Connector>) -> Self {
        IpcDaemonClient { client: client.to_string(), writer: Arc::new(Mutex::new(None)), connector, hears: for_window }
    }

    fn hello(client: &str) -> Incoming {
        Incoming::Hello { version: Some(env!("CARGO_PKG_VERSION").to_string()), client: Some(client.to_string()) }
    }
}

/// How long a closing client waits for the daemon to close its end.
const CLOSE_WAIT: Duration = Duration::from_secs(1);

impl Drop for IpcDaemonClient {
    /// Close the connection the polite way: stop sending, then let the
    /// reader take what the daemon still sends until the daemon closes its
    /// end. Dropping a socket with unread events resets it, which the daemon
    /// logs as a read error (`udm-cli` exits right after its answer).
    fn drop(&mut self) {
        match lock(&self.writer).as_ref() {
            Some(stream) => {
                let _ = stream.shutdown(Shutdown::Write);
            }
            None => return,
        }
        let until = Instant::now() + CLOSE_WAIT;
        while lock(&self.writer).is_some() && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Write `msg` to the connection in `writer`, if there is one.
fn write(writer: &Mutex<Option<TcpStream>>, msg: &Incoming) -> bool {
    let mut guard = lock(writer);
    let Some(stream) = guard.as_mut() else {
        return false;
    };
    match serde_json::to_vec(msg) {
        Ok(frame) => write_frame(stream, &frame).is_ok(),
        Err(_) => false,
    }
}

impl DaemonClient for IpcDaemonClient {
    fn connect(&self, restart: bool, mut on_event: Box<dyn FnMut(Event) + Send>) {
        let (writer, connector, client) = (Arc::clone(&self.writer), Arc::clone(&self.connector), self.client.clone());
        let hears = self.hears;
        thread::spawn(move || {
            let stream = match connector(restart) {
                Ok(s) => s,
                Err(e) => return on_event(Event::Lost(Lost::Failed(e.to_string()))),
            };
            let mut reader = match stream.try_clone() {
                Ok(r) => r,
                Err(e) => return on_event(Event::Lost(Lost::Failed(e.to_string()))),
            };
            *lock(&writer) = Some(stream);
            on_event(Event::Connected);
            for msg in [Self::hello(&client), Incoming::List, Incoming::GetSettings] {
                write(&writer, &msg);
            }
            while let Ok(Some(raw)) = read_message(&mut reader, MAX_MESSAGE_BYTES) {
                // A message this build does not know is skipped.
                if let Some(event) = serde_json::from_slice::<Outgoing>(&raw).ok().map(event).filter(hears) {
                    on_event(event);
                }
            }
            *lock(&writer) = None;
            on_event(Event::Lost(Lost::Closed));
        });
    }

    fn send(&self, command: Command) -> bool {
        write(&self.writer, &incoming(command))
    }

    fn run_command_line(&self, command: CommandLine) -> Result<bool, DaemonError> {
        let connect = |e: io::Error| DaemonError::Connect(e.to_string());
        let mut stream = (self.connector)(false).map_err(connect)?;
        stream.set_read_timeout(Some(COMMAND_LINE_TIMEOUT)).map_err(connect)?;
        for msg in [Self::hello("command-line"), Incoming::CommandLine(command)] {
            let frame = serde_json::to_vec(&msg).map_err(|e| DaemonError::Connect(e.to_string()))?;
            write_frame(&mut stream, &frame).map_err(connect)?;
        }
        loop {
            let raw = match read_message(&mut stream, MAX_MESSAGE_BYTES) {
                Ok(Some(raw)) => raw,
                Ok(None) => return Err(DaemonError::Closed),
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Err(DaemonError::Closed),
                Err(e) => return Err(connect(e)),
            };
            match serde_json::from_slice::<Outgoing>(&raw) {
                Ok(Outgoing::CommandLine { window_open }) => return Ok(window_open),
                // A daemon older than this window does not know the message.
                Ok(Outgoing::Error { message, error_kind }) => {
                    return Err(DaemonError::Refused(wire::failure(message, error_kind)))
                }
                _ => {}
            }
        }
    }
}

/// Whether `event` is for a window. Replies only the host or the browser
/// extension uses, and the command line's answer, are not. `Pong` is: the
/// window pings to see that the daemon still answers.
fn for_window(event: &Event) -> bool {
    !matches!(
        event,
        Event::Hello { .. }
            | Event::Ack { .. }
            | Event::MediaInfo { .. }
            | Event::CookiesWanted { .. }
            | Event::RenderPage { .. }
            | Event::CommandLine { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc::{channel, Receiver};
    use udm_application::daemon::Failure;
    use udm_domain::id::DownloadId;

    /// A pretend daemon on a local port: it hands each accepted
    /// connection to `serve`.
    fn fake_daemon(serve: impl Fn(TcpStream) + Send + Sync + 'static) -> Arc<Connector> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let serve = Arc::new(serve);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let serve = Arc::clone(&serve);
                thread::spawn(move || serve(stream));
            }
        });
        Arc::new(move |_restart| TcpStream::connect(addr))
    }

    fn read(stream: &mut TcpStream) -> Option<Incoming> {
        let raw = read_message(stream, MAX_MESSAGE_BYTES).ok()??;
        serde_json::from_slice(&raw).ok()
    }

    fn send(stream: &mut TcpStream, msg: &Outgoing) {
        write_frame(stream, &serde_json::to_vec(msg).unwrap()).unwrap();
    }

    fn events(client: &IpcDaemonClient) -> Receiver<Event> {
        let (tx, rx) = channel();
        client.connect(
            false,
            Box::new(move |e| {
                let _ = tx.send(e);
            }),
        );
        rx
    }

    fn next(rx: &Receiver<Event>) -> Event {
        rx.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn it_introduces_itself_then_relays_both_ways() {
        let (got_tx, got) = channel();
        let got_tx = Mutex::new(got_tx);
        let connector = fake_daemon(move |mut stream| {
            // Hello, list and settings come first.
            for _ in 0..3 {
                let _ = lock(&got_tx).send(read(&mut stream));
            }
            send(&mut stream, &Outgoing::Added { id: DownloadId::new(4), url: "http://x/f".into() });
            send(&mut stream, &Outgoing::Pong);
            send(&mut stream, &Outgoing::Removed { id: DownloadId::new(4) });
            let _ = lock(&got_tx).send(read(&mut stream));
        });
        let client = IpcDaemonClient::with_connector("window", connector);
        let rx = events(&client);
        assert_eq!(next(&rx), Event::Connected);
        let hello = got.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(hello, Some(Incoming::Hello { client: Some(ref c), .. }) if c == "window"));
        assert!(matches!(got.recv().unwrap(), Some(Incoming::List)));
        assert!(matches!(got.recv().unwrap(), Some(Incoming::GetSettings)));
        assert_eq!(next(&rx), Event::Added { id: DownloadId::new(4), url: "http://x/f".into() });
        // The window pings to see that the daemon answers.
        assert_eq!(next(&rx), Event::Pong);
        assert_eq!(next(&rx), Event::Removed { id: DownloadId::new(4) });
        assert!(client.send(Command::Cancel { id: DownloadId::new(4) }));
        assert!(
            matches!(got.recv_timeout(Duration::from_secs(5)).unwrap(), Some(Incoming::Cancel { id }) if id == DownloadId::new(4))
        );
        // The pretend daemon hangs up after that.
        assert_eq!(next(&rx), Event::Lost(Lost::Closed));
        assert!(!client.send(Command::List));
    }

    #[test]
    fn a_program_hears_the_replies_a_window_does_not() {
        let connector = fake_daemon(move |mut stream| {
            for _ in 0..3 {
                let _ = read(&mut stream);
            }
            send(&mut stream, &Outgoing::Hello { name: "udm".into(), version: "1".into(), api: None });
            send(&mut stream, &Outgoing::Ack { id: DownloadId::new(2), url: "http://x/f".into(), status: "s".into() });
        });
        let client = IpcDaemonClient::with_connector("udm-cli", connector).hearing(|_| true);
        let rx = events(&client);
        assert_eq!(next(&rx), Event::Connected);
        assert!(matches!(next(&rx), Event::Hello { .. }));
        assert!(matches!(next(&rx), Event::Ack { id, .. } if id == DownloadId::new(2)));
    }

    #[test]
    fn dropping_a_client_closes_the_connection_cleanly() {
        let (ended_tx, ended) = channel();
        let ended_tx = Mutex::new(ended_tx);
        let connector = fake_daemon(move |mut stream| {
            for _ in 0..3 {
                let _ = read(&mut stream);
            }
            // Events the client never reads, as a busy daemon sends them.
            for _ in 0..50 {
                send(&mut stream, &Outgoing::Pong);
            }
            // The daemon reads until the client stops sending: a clean end
            // of stream, not a reset.
            let end = read_message(&mut stream, MAX_MESSAGE_BYTES);
            let _ = lock(&ended_tx).send(matches!(end, Ok(None)));
        });
        let client = IpcDaemonClient::with_connector("udm-cli", connector).hearing(|_| true);
        let rx = events(&client);
        assert_eq!(next(&rx), Event::Connected);
        drop(client);
        assert!(ended.recv_timeout(Duration::from_secs(5)).unwrap());
    }

    #[test]
    fn an_assistant_hears_the_replies_but_not_the_progress() {
        let hears = IpcDaemonClient::assistant("udm-mcp").hears;
        assert!(hears(&Event::Pong));
        assert!(hears(&Event::Ack { id: DownloadId::new(2), url: "http://x/f".into(), status: "s".into() }));
        assert!(!hears(&Event::Progress {
            id: DownloadId::new(2),
            url: "http://x/f".into(),
            done: udm_domain::bytes::ByteCount::new(1),
            total: None,
            parts: None,
            segments: None,
        }));
    }

    #[test]
    fn a_failed_connection_says_why() {
        let connector: Arc<Connector> = Arc::new(|_| Err(io::Error::other("no daemon here")));
        let client = IpcDaemonClient::with_connector("window", connector);
        let rx = events(&client);
        assert_eq!(next(&rx), Event::Lost(Lost::Failed("no daemon here".into())));
        assert!(!client.send(Command::List));
    }

    #[test]
    fn a_command_line_waits_for_its_answer() {
        let connector = fake_daemon(|mut stream| {
            assert!(matches!(read(&mut stream), Some(Incoming::Hello { client: Some(c), .. }) if c == "command-line"));
            match read(&mut stream) {
                Some(Incoming::CommandLine(cmd)) if cmd.start_queue => {
                    send(&mut stream, &Outgoing::Pong);
                    send(&mut stream, &Outgoing::CommandLine { window_open: true });
                }
                _ => send(&mut stream, &Outgoing::Error { message: "unexpected".into(), error_kind: None }),
            }
        });
        let client = IpcDaemonClient::with_connector("window", connector);
        let cmd = CommandLine { start_queue: true, ..CommandLine::default() };
        assert_eq!(client.run_command_line(cmd), Ok(true));
        let refused = client.run_command_line(CommandLine::default());
        assert_eq!(refused, Err(DaemonError::Refused(Failure::Other("unexpected".into()))));
    }

    #[test]
    fn a_daemon_that_hangs_up_on_a_command_line_closed_it() {
        let connector = fake_daemon(|mut stream| {
            let _ = read(&mut stream);
            let _ = read(&mut stream);
        });
        let client = IpcDaemonClient::with_connector("window", connector);
        assert_eq!(client.run_command_line(CommandLine::default()), Err(DaemonError::Closed));
    }
}
