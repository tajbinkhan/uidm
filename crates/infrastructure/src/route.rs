//! How a download's connections leave the computer: bound to the network
//! adapter the settings choose, if any, and with names looked up where the
//! settings say (`doh`).
//!
//! Binding, per system:
//! - Linux: the socket is bound to the interface (`SO_BINDTODEVICE`), so
//!   its packets can only leave through it. Unprivileged since Linux 5.7.
//! - macOS: likewise (`IP_BOUND_IF` / `IPV6_BOUND_IF`).
//! - Windows: the socket is bound to the adapter's address, which Windows'
//!   strong host model sends only through that adapter. A connection to
//!   an address of the other family than the one bound (IPv6 from an
//!   IPv4 address) cannot be made.
//!
//! The kill switch: before every connection, through the HTTP client's
//! connector ([`KillSwitch`]) and the FTP engine's own sockets alike, the
//! adapter is looked up afresh through the `NetworkAdapters` port. When it
//! is missing, stopped or has no address the connection fails with
//! [`AdapterDown`], and nothing falls back to another network. A running
//! download looks every [`Binding::watch_every`] too (`engine::run`), so
//! one already receiving stops as soon as the adapter goes.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use udm_domain::network::{bound_adapter, AdapterChoice, AdapterDown, NetworkAdapter};
use udm_domain::ports::NetworkAdapters;

use crate::doh::Doh;

/// How often a running download looks at its adapter.
pub const WATCH_EVERY: Duration = Duration::from_secs(2);

/// The adapter connections are bound to.
#[derive(Clone)]
pub struct Binding {
    pub choice: AdapterChoice,
    pub adapters: Arc<dyn NetworkAdapters>,
    /// How often a running download checks that the adapter is still up.
    pub watch_every: Duration,
}

impl std::fmt::Debug for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Binding").field("choice", &self.choice).finish_non_exhaustive()
    }
}

impl Binding {
    pub fn new(choice: AdapterChoice, adapters: Arc<dyn NetworkAdapters>) -> Binding {
        Binding { choice, adapters, watch_every: WATCH_EVERY }
    }

    /// The adapter as it is now, or why connections may not be made. An
    /// adapter list that cannot be read counts as the adapter being down.
    pub fn adapter(&self) -> Result<NetworkAdapter, AdapterDown> {
        let adapters = self.adapters.list().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "the network adapters could not be read");
            Vec::new()
        });
        bound_adapter(&self.choice, &adapters).cloned()
    }

    /// `builder` with its connections bound to the adapter, and each one
    /// checking the adapter first. Fails when it is down now.
    pub fn bind(&self, builder: reqwest::ClientBuilder) -> Result<reqwest::ClientBuilder, AdapterDown> {
        let adapter = self.adapter()?;
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios"))]
        let builder = {
            let _ = &adapter;
            builder.interface(self.choice.id.as_str())
        };
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios")))]
        let builder = builder.local_address(adapter.bind_address());
        Ok(builder.connector_layer(KillSwitch(self.clone())))
    }

    /// A socket for a connection to `peer`, bound to the adapter.
    fn socket(&self, peer: IpAddr) -> io::Result<Socket> {
        let adapter = self.adapter().map_err(io::Error::other)?;
        let socket = Socket::new(Domain::for_address(SocketAddr::new(peer, 0)), Type::STREAM, Some(Protocol::TCP))?;
        bind_socket(&socket, &adapter, peer)?;
        Ok(socket)
    }

    /// A socket listening on `ip` (any port), bound to the adapter.
    fn listener(&self, ip: IpAddr) -> io::Result<Socket> {
        let adapter = self.adapter().map_err(io::Error::other)?;
        let socket = Socket::new(Domain::for_address(SocketAddr::new(ip, 0)), Type::STREAM, Some(Protocol::TCP))?;
        bind_listener(&socket, &adapter, ip)?;
        socket.listen(1)?;
        Ok(socket)
    }
}

/// Bind a listening `socket` on `ip` to `adapter`: the interface, then the
/// address.
#[cfg(any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios"))]
fn bind_listener(socket: &Socket, adapter: &NetworkAdapter, ip: IpAddr) -> io::Result<()> {
    bind_socket(socket, adapter, ip)?;
    socket.bind(&SockAddr::from(SocketAddr::new(ip, 0)))
}

/// Bind a listening `socket` on `ip`, which must be the adapter's own
/// address: binding to it is binding to the adapter.
#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios")))]
fn bind_listener(socket: &Socket, adapter: &NetworkAdapter, ip: IpAddr) -> io::Result<()> {
    if !adapter.addresses.contains(&ip) {
        return Err(io::Error::new(io::ErrorKind::AddrNotAvailable, format!("{ip} is not the network adapter's")));
    }
    socket.bind(&SockAddr::from(SocketAddr::new(ip, 0)))
}

/// Bind `socket`, for a connection to `peer`, to `adapter`.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn bind_socket(socket: &Socket, adapter: &NetworkAdapter, _peer: IpAddr) -> io::Result<()> {
    socket.bind_device(Some(adapter.id.as_str().as_bytes()))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn bind_socket(socket: &Socket, adapter: &NetworkAdapter, peer: IpAddr) -> io::Result<()> {
    let name = std::ffi::CString::new(adapter.id.as_str()).map_err(io::Error::other)?;
    // SAFETY: `name` is a NUL-terminated string that outlives the call.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    let index = std::num::NonZeroU32::new(index).ok_or_else(io::Error::last_os_error)?;
    match peer {
        IpAddr::V4(_) => socket.bind_device_by_index_v4(Some(index)),
        IpAddr::V6(_) => socket.bind_device_by_index_v6(Some(index)),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios")))]
fn bind_socket(socket: &Socket, adapter: &NetworkAdapter, peer: IpAddr) -> io::Result<()> {
    let local = adapter.address_for(peer).ok_or_else(|| {
        io::Error::new(io::ErrorKind::AddrNotAvailable, format!("the network adapter has no address to reach {peer}"))
    })?;
    socket.bind(&SockAddr::from(SocketAddr::new(local, 0)))
}

/// Where a download's connections go out, and how names are looked up.
#[derive(Clone, Debug, Default)]
pub struct Route {
    /// The adapter they are bound to; `None` lets the system choose.
    pub binding: Option<Binding>,
    /// DNS over HTTPS; `None` asks the system.
    pub dns: Option<Arc<Doh>>,
}

impl Route {
    /// The adapter now, when bound to one; `Ok(None)` when not bound.
    pub fn check(&self) -> Result<Option<NetworkAdapter>, AdapterDown> {
        self.binding.as_ref().map(Binding::adapter).transpose()
    }

    /// `builder` going out this way: bound, checked before each
    /// connection, and with this resolver.
    pub fn client(&self, mut builder: reqwest::ClientBuilder) -> Result<reqwest::ClientBuilder, AdapterDown> {
        if let Some(binding) = &self.binding {
            builder = binding.bind(builder)?;
        }
        if let Some(doh) = &self.dns {
            builder = builder.dns_resolver(Arc::new(crate::doh::Resolver(Arc::clone(doh))));
        }
        Ok(builder)
    }

    /// The address a program UDM runs (yt-dlp, gallery-dl) is told to
    /// connect from with its own `--source-address`: `Ok(None)` when not
    /// bound, and an error when the adapter is down, so the program is not
    /// run at all.
    pub fn source_address(&self) -> Result<Option<IpAddr>, AdapterDown> {
        Ok(self.check()?.and_then(|a| a.bind_address()))
    }

    /// The addresses of `host` at `port`: an address as it is, `localhost`
    /// on this computer, else through DNS over HTTPS or the system.
    pub fn lookup(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = bare.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        let found: Vec<SocketAddr> = match &self.dns {
            Some(doh) => doh
                .lookup_blocking(bare)
                .map_err(|e| match e {
                    crate::doh::DohError::AdapterDown(down) => io::Error::other(down),
                    e => io::Error::new(io::ErrorKind::NotFound, e),
                })?
                .into_iter()
                .map(|ip| SocketAddr::new(ip, port))
                .collect(),
            None if bare.eq_ignore_ascii_case("localhost") => {
                vec![
                    SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port),
                    SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port),
                ]
            }
            None => (bare, port).to_socket_addrs()?.collect(),
        };
        if found.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{host} could not be found")));
        }
        Ok(found)
    }

    /// A connection to `host` at `port`, trying each of its addresses.
    pub fn connect(&self, host: &str, port: u16, timeout: Duration) -> io::Result<TcpStream> {
        // Down before any lookup, so a down adapter says so.
        self.check().map_err(io::Error::other)?;
        let mut last = io::Error::new(io::ErrorKind::NotFound, format!("{host} could not be found"));
        for addr in self.lookup(host, port)? {
            match self.connect_addr(addr, timeout) {
                Ok(stream) => return Ok(stream),
                Err(e) if is_adapter_down(&e) => return Err(e),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// A connection to `host:port` (an IPv6 address in brackets), as
    /// proxies are written.
    pub fn connect_to(&self, host_port: &str, default_port: u16, timeout: Duration) -> io::Result<TcpStream> {
        let (host, port) = split_host_port(host_port, default_port)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("bad address {host_port}")))?;
        self.connect(host, port, timeout)
    }

    /// A connection to `addr`, bound to the adapter when there is one.
    pub fn connect_addr(&self, addr: SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
        let Some(binding) = &self.binding else {
            return TcpStream::connect_timeout(&addr, timeout);
        };
        let socket = binding.socket(addr.ip())?;
        socket.connect_timeout(&SockAddr::from(addr), timeout)?;
        Ok(socket.into())
    }

    /// A listener on `ip` (any port), bound to the adapter when there is
    /// one, for an active FTP data connection.
    pub fn listen(&self, ip: IpAddr) -> io::Result<TcpListener> {
        let Some(binding) = &self.binding else {
            return TcpListener::bind(SocketAddr::new(ip, 0));
        };
        Ok(binding.listener(ip)?.into())
    }
}

/// `host:port`'s host and port: `[v6]:port` with its brackets dropped, a
/// bare address or name taking `default_port`.
fn split_host_port(text: &str, default_port: u16) -> Option<(&str, u16)> {
    if let Some(rest) = text.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        return match after.strip_prefix(':') {
            Some(port) => Some((host, port.parse().ok()?)),
            None if after.is_empty() => Some((host, default_port)),
            None => None,
        };
    }
    match text.split_once(':') {
        // More than one colon: a bare IPv6 address.
        Some((_, rest)) if rest.contains(':') => Some((text, default_port)),
        Some((host, port)) => Some((host, port.parse().ok()?)),
        None => Some((text, default_port)),
    }
}

/// Whether `e` is a connection refused because the adapter is down.
pub fn is_adapter_down(e: &io::Error) -> bool {
    adapter_down_in(e).is_some()
}

/// The [`AdapterDown`] somewhere in `e`'s causes, if any.
pub fn adapter_down_in<'a>(e: &'a (dyn std::error::Error + 'static)) -> Option<&'a AdapterDown> {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(c) = cause {
        if let Some(down) = c.downcast_ref::<AdapterDown>() {
            return Some(down);
        }
        if let Some(down) = c.downcast_ref::<io::Error>().and_then(|io| io.get_ref()).and_then(|inner| {
            let inner: &(dyn std::error::Error + 'static) = inner;
            inner.downcast_ref::<AdapterDown>()
        }) {
            return Some(down);
        }
        cause = c.source();
    }
    None
}

/// A connector layer for the HTTP client that looks at the adapter before
/// each new connection, the proxy's included, and refuses it when the
/// adapter is down.
#[derive(Clone)]
pub struct KillSwitch(Binding);

impl<S> tower_layer::Layer<S> for KillSwitch {
    type Service = KillSwitchService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        KillSwitchService { inner, binding: self.0.clone() }
    }
}

#[derive(Clone)]
pub struct KillSwitchService<S> {
    inner: S,
    binding: Binding,
}

type BoxFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>;

impl<S, R> tower_service::Service<R> for KillSwitchService<S>
where
    S: tower_service::Service<R> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: From<AdapterDown>,
    R: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<Result<S::Response, S::Error>>;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), S::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: R) -> Self::Future {
        // The service that was made ready is the one called, as tower asks.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        match self.binding.adapter() {
            Ok(_) => Box::pin(async move { inner.call(req).await }),
            Err(down) => {
                tracing::warn!(adapter = %down.adapter, "a connection was refused: the network adapter is down");
                Box::pin(async move { Err(S::Error::from(down)) })
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use udm_domain::network::{AdapterId, NetworkError};

    /// The system's loopback adapter, which tests bind to.
    pub fn loopback() -> NetworkAdapter {
        let all = crate::adapters::SystemAdapters.list().unwrap();
        all.into_iter()
            .find(|a| a.addresses.contains(&IpAddr::V4(Ipv4Addr::LOCALHOST)))
            .expect("a loopback adapter with 127.0.0.1")
    }

    /// A stand-in for the system's adapters: the loopback one, until it is
    /// made to disappear.
    pub struct StandIn {
        pub adapter: NetworkAdapter,
        pub gone: AtomicBool,
    }

    impl NetworkAdapters for StandIn {
        fn list(&self) -> Result<Vec<NetworkAdapter>, NetworkError> {
            Ok(if self.gone.load(Ordering::SeqCst) { Vec::new() } else { vec![self.adapter.clone()] })
        }
    }

    pub fn bound_to_loopback() -> (Route, Arc<StandIn>) {
        let adapter = loopback();
        let choice = AdapterChoice { id: adapter.id.clone(), name: "Test adapter".into() };
        let stand_in = Arc::new(StandIn { adapter, gone: AtomicBool::new(false) });
        let binding = Binding { watch_every: Duration::from_millis(100), ..Binding::new(choice, stand_in.clone()) };
        (Route { binding: Some(binding), dns: None }, stand_in)
    }

    #[test]
    fn sockets_bind_to_the_adapter_and_refuse_when_it_is_gone() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap();
        let (route, stand_in) = bound_to_loopback();
        let stream = route.connect("127.0.0.1", at.port(), Duration::from_secs(5)).unwrap();
        assert_eq!(stream.peer_addr().unwrap(), at);
        assert!(route.connect_to(&format!("localhost:{}", at.port()), 21, Duration::from_secs(5)).is_ok());
        let active = route.listen(IpAddr::V4(Ipv4Addr::LOCALHOST)).unwrap();
        assert!(TcpStream::connect(active.local_addr().unwrap()).is_ok());
        assert_eq!(route.source_address().unwrap(), Some(IpAddr::V4(Ipv4Addr::LOCALHOST)));

        stand_in.gone.store(true, Ordering::SeqCst);
        let refused = route.connect("127.0.0.1", at.port(), Duration::from_secs(5)).unwrap_err();
        assert_eq!(adapter_down_in(&refused).map(|d| d.adapter.as_str()), Some("Test adapter"));
        assert!(is_adapter_down(&route.listen(IpAddr::V4(Ipv4Addr::LOCALHOST)).unwrap_err()));
        assert!(route.source_address().is_err(), "no program is run unbound");
    }

    /// Bound to another adapter, a server on the loopback one cannot be
    /// reached: the binding is the system's, not only a check.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_socket_bound_elsewhere_cannot_reach_loopback() {
        let Some(other) = crate::adapters::SystemAdapters.list().unwrap().into_iter().find(|a| {
            a.usable() && !a.addresses.iter().any(IpAddr::is_loopback) && a.addresses.iter().any(IpAddr::is_ipv4)
        }) else {
            return;
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap();
        let choice = AdapterChoice { id: other.id.clone(), name: String::new() };
        let route = Route { binding: Some(Binding::new(choice, Arc::new(crate::adapters::SystemAdapters))), dns: None };
        assert!(route.connect_addr(at, Duration::from_millis(500)).is_err(), "{other:?}");
        assert!(Route::default().connect_addr(at, Duration::from_secs(2)).is_ok());
    }

    #[test]
    fn proxies_addresses_split_into_host_and_port() {
        assert_eq!(split_host_port("proxy:1080", 21), Some(("proxy", 1080)));
        assert_eq!(split_host_port("proxy", 21), Some(("proxy", 21)));
        assert_eq!(split_host_port("[::1]:8080", 21), Some(("::1", 8080)));
        assert_eq!(split_host_port("[::1]", 21), Some(("::1", 21)));
        assert_eq!(split_host_port("2001:db8::1", 21), Some(("2001:db8::1", 21)));
        assert_eq!(split_host_port("proxy:x", 21), None);
    }

    #[test]
    fn an_adapter_that_does_not_exist_is_down() {
        let choice = AdapterChoice { id: AdapterId::parse("udm-no-such-adapter0").unwrap(), name: String::new() };
        let route = Route { binding: Some(Binding::new(choice, Arc::new(crate::adapters::SystemAdapters))), dns: None };
        assert!(route.check().is_err());
        assert!(route.client(reqwest::Client::builder()).is_err());
        assert!(Route::default().check().unwrap().is_none());
    }
}
