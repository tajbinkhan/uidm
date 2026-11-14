//! DNS over HTTPS (RFC 8484): host names looked up by posting DNS messages
//! (`application/dns-message`) to the resolver the settings choose, over
//! the same TLS as downloads, bound to the chosen network adapter too.
//!
//! - A preset resolver's own host is never looked up: its addresses come
//!   with it (`udm_domain::network::DnsChoice::endpoint`). A custom
//!   resolver's host is looked up once by the system, when it is first
//!   asked, unless it is an address.
//! - It fails closed: a lookup the resolver does not answer fails, and is
//!   never asked of the system instead.
//! - Answers are kept as long as their time to live says, within
//!   `udm_domain::network::dns_cache_seconds`.
//!
//! [`Doh`] is reqwest's resolver for every download's client
//! (`route::Route`), and answers the FTP engine's lookups with
//! [`Doh::lookup_blocking`].

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use udm_domain::network::{dns_cache_seconds, DohEndpoint};
use udm_domain::sync::LockExt;

use crate::route::Binding;

/// How long one query may take, connection included.
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);
/// The most answers kept; the oldest go first.
const CACHE_SIZE: usize = 1024;
/// The longest DNS message read back.
const MAX_ANSWER: usize = 64 * 1024;

const TYPE_A: u16 = 1;
const TYPE_CNAME: u16 = 5;
const TYPE_AAAA: u16 = 28;

/// Why a name was not found.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DohError {
    /// The resolver says the name does not exist, or has no address.
    #[error("{0} was not found by the DNS over HTTPS resolver")]
    NotFound(String),
    /// The resolver could not be reached or gave no usable answer; the
    /// lookup is not made another way.
    #[error("the DNS over HTTPS resolver did not answer: {0}")]
    Unanswered(String),
    /// The network adapter queries are bound to is down.
    #[error(transparent)]
    AdapterDown(#[from] udm_domain::network::AdapterDown),
}

/// A request to the resolver that failed, keeping why when the adapter is
/// down.
fn unanswered(e: &reqwest::Error) -> DohError {
    match crate::route::adapter_down_in(e) {
        Some(down) => DohError::AdapterDown(down.clone()),
        None => DohError::Unanswered(crate::engine::http::error_text(e)),
    }
}

/// A DNS over HTTPS resolver and its answers.
pub struct Doh {
    endpoint: DohEndpoint,
    binding: Option<Binding>,
    /// Made when first asked: building it may look up a custom resolver's
    /// host.
    client: OnceLock<reqwest::Client>,
    cache: Mutex<HashMap<String, (Vec<IpAddr>, Instant)>>,
}

impl std::fmt::Debug for Doh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Doh").field("url", &self.endpoint.url).finish_non_exhaustive()
    }
}

impl Doh {
    pub fn new(endpoint: DohEndpoint, binding: Option<Binding>) -> Doh {
        Doh { endpoint, binding, client: OnceLock::new(), cache: Mutex::new(HashMap::new()) }
    }

    /// The resolver's address, for logs.
    pub fn url(&self) -> &str {
        &self.endpoint.url
    }

    /// The client queries go out on: TLS to the resolver's host, at the
    /// addresses known for it, bound like every download's connection, and
    /// through no proxy.
    async fn client(&self) -> Result<reqwest::Client, DohError> {
        if let Some(client) = self.client.get() {
            return Ok(client.clone());
        }
        let host = self.endpoint.host.clone();
        let mut addresses: Vec<SocketAddr> =
            self.endpoint.bootstrap.iter().map(|ip| SocketAddr::new(*ip, 443)).collect();
        if addresses.is_empty() && host.trim_matches(['[', ']']).parse::<IpAddr>().is_err() {
            // A custom resolver: the system finds its host, once.
            let lookup = host.clone();
            addresses = tokio::task::spawn_blocking(move || {
                std::net::ToSocketAddrs::to_socket_addrs(&(lookup.as_str(), 443)).map(Iterator::collect::<Vec<_>>)
            })
            .await
            .map_err(|e| DohError::Unanswered(e.to_string()))?
            .map_err(|e| DohError::Unanswered(format!("{host} could not be found: {e}")))?;
        }
        let mut builder = reqwest::Client::builder()
            .use_native_tls()
            .connect_timeout(QUERY_TIMEOUT)
            .timeout(QUERY_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        if !addresses.is_empty() {
            builder = builder.resolve_to_addrs(&host, &addresses);
        }
        if let Some(binding) = &self.binding {
            builder = binding.bind(builder)?;
        }
        let client = builder.build().map_err(|e| DohError::Unanswered(crate::engine::http::error_text(&e)))?;
        Ok(self.client.get_or_init(|| client).clone())
    }

    /// The addresses of `name`: IPv4 ones first, then IPv6.
    pub async fn lookup(&self, name: &str) -> Result<Vec<IpAddr>, DohError> {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        if let Some(local) = loopback(&name) {
            return Ok(local);
        }
        if let Some((found, until)) = self.cache.lock_or_recover().get(&name) {
            if Instant::now() < *until {
                return Ok(found.clone());
            }
        }
        let client = self.client().await?;
        let v4 = self.ask(&client, &name, TYPE_A).await;
        let v6 = self.ask(&client, &name, TYPE_AAAA).await;
        let (found, ttl) = match (v4, v6) {
            (Err(e), Err(_)) => return Err(e),
            (Ok(a), Err(_)) | (Err(_), Ok(a)) => a,
            (Ok((mut a, t1)), Ok((b, t2))) => {
                a.extend(b);
                (a, t1.min(t2))
            }
        };
        if found.is_empty() {
            return Err(DohError::NotFound(name));
        }
        let mut cache = self.cache.lock_or_recover();
        if cache.len() >= CACHE_SIZE {
            let now = Instant::now();
            cache.retain(|_, (_, until)| *until > now);
            if cache.len() >= CACHE_SIZE {
                cache.clear();
            }
        }
        cache.insert(name, (found.clone(), Instant::now() + Duration::from_secs(dns_cache_seconds(ttl))));
        Ok(found)
    }

    /// `lookup`, from a thread outside the HTTP runtime (the FTP engine's).
    pub fn lookup_blocking(&self, name: &str) -> Result<Vec<IpAddr>, DohError> {
        let runtime = crate::engine::http::runtime().map_err(|e| DohError::Unanswered(e.to_string()))?;
        runtime.block_on(self.lookup(name))
    }

    /// One query for `name`'s records of `kind`: the addresses and the
    /// smallest time to live among the records that led to them.
    async fn ask(&self, client: &reqwest::Client, name: &str, kind: u16) -> Result<(Vec<IpAddr>, u32), DohError> {
        let query = query(name, kind).ok_or_else(|| DohError::NotFound(name.to_string()))?;
        let sent = client
            .post(&self.endpoint.url)
            .header(reqwest::header::CONTENT_TYPE, "application/dns-message")
            .header(reqwest::header::ACCEPT, "application/dns-message")
            .body(query)
            .send()
            .await
            .map_err(|e| unanswered(&e))?;
        if !sent.status().is_success() {
            return Err(DohError::Unanswered(format!("HTTP {}", sent.status().as_u16())));
        }
        let body = sent.bytes().await.map_err(|e| unanswered(&e))?;
        if body.len() > MAX_ANSWER {
            return Err(DohError::Unanswered("the answer is too long".into()));
        }
        read_answer(&body, name, kind)
    }
}

/// `localhost` is this computer, and is never asked of a resolver (RFC 6761).
fn loopback(name: &str) -> Option<Vec<IpAddr>> {
    (name == "localhost" || name.ends_with(".localhost"))
        .then(|| vec![IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V6(Ipv6Addr::LOCALHOST)])
}

/// reqwest's resolver over a shared [`Doh`].
pub struct Resolver(pub Arc<Doh>);

impl reqwest::dns::Resolve for Resolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let doh = Arc::clone(&self.0);
        let name = name.as_str().to_string();
        Box::pin(async move {
            // A down adapter goes to the engine as itself, so the download
            // stops for good rather than retrying.
            let found = match doh.lookup(&name).await {
                Ok(found) => found,
                Err(DohError::AdapterDown(down)) => {
                    return Err(Box::new(down) as Box<dyn std::error::Error + Send + Sync>)
                }
                Err(e) => return Err(Box::new(e)),
            };
            // The port is the address's, filled in by reqwest.
            let addrs: reqwest::dns::Addrs = Box::new(found.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

/// A query for `name`'s records of `kind`, as RFC 8484 asks: id 0, so
/// answers can be cached, and recursion wanted. `None` for a name DNS
/// cannot carry.
pub fn query(name: &str, kind: u16) -> Option<Vec<u8>> {
    let mut q = vec![0, 0, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    let name = name.trim_end_matches('.');
    if name.is_empty() || name.len() > 253 {
        return None;
    }
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&kind.to_be_bytes());
    q.extend_from_slice(&1u16.to_be_bytes());
    Some(q)
}

/// Reads a DNS message in order.
struct Message<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Message<'_> {
    fn u8(&mut self) -> Option<u8> {
        let b = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(b)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes([self.u8()?, self.u8()?]))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes([self.u8()?, self.u8()?, self.u8()?, self.u8()?]))
    }

    fn skip(&mut self, n: usize) -> Option<&[u8]> {
        let part = self.bytes.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(part)
    }

    /// A name at the read position, following compression pointers, in
    /// lower case without the final dot.
    fn name(&mut self) -> Option<String> {
        let (name, end) = name_at(self.bytes, self.at)?;
        self.at = end;
        Some(name)
    }
}

/// The name at `at` in `bytes` and where the record goes on after it. A
/// pointer (two bits set) jumps elsewhere in the message; jumps are bounded
/// so a looping message cannot hang the reader.
fn name_at(bytes: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut end = None;
    let mut length = 0;
    for _ in 0..128 {
        let len = *bytes.get(at)?;
        match len & 0xc0 {
            0x00 if len == 0 => {
                let after = end.unwrap_or(at + 1);
                return Some((labels.join("."), after));
            }
            0x00 => {
                let label = bytes.get(at + 1..at + 1 + usize::from(len))?;
                length += label.len() + 1;
                if length > 255 {
                    return None;
                }
                labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
                at += 1 + usize::from(len);
            }
            0xc0 => {
                let pointer = usize::from(u16::from_be_bytes([len & 0x3f, *bytes.get(at + 1)?]));
                end.get_or_insert(at + 2);
                at = pointer;
            }
            _ => return None,
        }
    }
    None
}

/// The addresses a response gives for `name`'s records of `kind`, following
/// CNAME records from `name` in whatever order they come, and the smallest
/// time to live along the way.
pub fn read_answer(bytes: &[u8], name: &str, kind: u16) -> Result<(Vec<IpAddr>, u32), DohError> {
    let bad = |what: &str| DohError::Unanswered(format!("a DNS answer that {what}"));
    let mut m = Message { bytes, at: 0 };
    let head = (|| Some((m.u16()?, m.u16()?, m.u16()?, m.u16()?)))().ok_or_else(|| bad("is cut short"))?;
    let (_id, flags, questions, answers) = head;
    m.skip(4).ok_or_else(|| bad("is cut short"))?;
    if flags & 0x8000 == 0 {
        return Err(bad("is not a response"));
    }
    match flags & 0x000f {
        0 => {}
        3 => return Err(DohError::NotFound(name.to_string())),
        code => return Err(DohError::Unanswered(format!("the resolver failed the lookup (error {code})"))),
    }
    for _ in 0..questions {
        m.name().ok_or_else(|| bad("has a damaged question"))?;
        m.skip(4).ok_or_else(|| bad("has a damaged question"))?;
    }
    struct Record {
        owner: String,
        kind: u16,
        ttl: u32,
        data: Data,
    }
    enum Data {
        Address(IpAddr),
        Alias(String),
        Other,
    }
    let mut records = Vec::new();
    for _ in 0..answers {
        let record = (|| {
            let owner = m.name()?;
            let (kind, _class, ttl, len) = (m.u16()?, m.u16()?, m.u32()?, usize::from(m.u16()?));
            let start = m.at;
            let rdata = m.skip(len)?;
            let data = match (kind, len) {
                (TYPE_A, 4) => Data::Address(IpAddr::V4(Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3]))),
                (TYPE_AAAA, 16) => {
                    let octets: [u8; 16] = rdata.try_into().ok()?;
                    Data::Address(IpAddr::V6(Ipv6Addr::from(octets)))
                }
                (TYPE_CNAME, _) => Data::Alias(name_at(bytes, start)?.0),
                _ => Data::Other,
            };
            Some(Record { owner, kind, ttl, data })
        })()
        .ok_or_else(|| bad("has a damaged record"))?;
        records.push(record);
    }
    // The names that stand for `name`: itself and its aliases, followed
    // until no new one turns up.
    let mut names = vec![name.trim_end_matches('.').to_ascii_lowercase()];
    let mut ttl = u32::MAX;
    loop {
        let more: Vec<(String, u32)> = records
            .iter()
            .filter(|r| names.contains(&r.owner))
            .filter_map(|r| match &r.data {
                Data::Alias(to) if !names.contains(to) => Some((to.clone(), r.ttl)),
                _ => None,
            })
            .collect();
        if more.is_empty() || names.len() > 16 {
            break;
        }
        for (alias, alias_ttl) in more {
            ttl = ttl.min(alias_ttl);
            if !names.contains(&alias) {
                names.push(alias);
            }
        }
    }
    let mut found = Vec::new();
    for r in records.iter().filter(|r| r.kind == kind && names.contains(&r.owner)) {
        if let Data::Address(ip) = r.data {
            ttl = ttl.min(r.ttl);
            if !found.contains(&ip) {
                found.push(ip);
            }
        }
    }
    Ok((found, if ttl == u32::MAX { 0 } else { ttl }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A DNS response to `query` with these answer records, each `(owner
    /// bytes, type, ttl, rdata)`; owners may be compression pointers.
    pub fn response(query: &[u8], rcode: u8, records: &[(Vec<u8>, u16, u32, Vec<u8>)]) -> Vec<u8> {
        let mut r = query.to_vec();
        r[2] = 0x81;
        r[3] = 0x80 | rcode;
        r[6..8].copy_from_slice(&(records.len() as u16).to_be_bytes());
        for (owner, kind, ttl, data) in records {
            r.extend_from_slice(owner);
            r.extend_from_slice(&kind.to_be_bytes());
            r.extend_from_slice(&1u16.to_be_bytes());
            r.extend_from_slice(&ttl.to_be_bytes());
            r.extend_from_slice(&(data.len() as u16).to_be_bytes());
            r.extend_from_slice(data);
        }
        r
    }

    /// The name in a query, at offset 12.
    const QNAME: [u8; 2] = [0xc0, 12];

    fn labels(name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for l in name.split('.') {
            out.push(l.len() as u8);
            out.extend_from_slice(l.as_bytes());
        }
        out.push(0);
        out
    }

    /// The question's name and type in a query.
    fn question(q: &[u8]) -> (String, u16) {
        let (name, end) = name_at(q, 12).unwrap();
        (name, u16::from_be_bytes([q[end], q[end + 1]]))
    }

    #[test]
    fn a_query_asks_for_one_name() {
        let q = query("Files.Example.com.", TYPE_AAAA).unwrap();
        assert_eq!(&q[..12], &[0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(question(&q), ("files.example.com".to_string(), TYPE_AAAA));
        assert!(query("a..b", TYPE_A).is_none());
        assert!(query(&"x".repeat(64), TYPE_A).is_none());
    }

    #[test]
    fn compressed_names_and_cname_chains_are_followed() {
        let q = query("www.example.com", TYPE_A).unwrap();
        assert_eq!(q.len(), 33);
        // www.example.com CNAME edge.cdn.net, its data at 45; "cdn.net" in
        // it at 50.
        let first = (QNAME.to_vec(), TYPE_CNAME, 300, labels("edge.cdn.net"));
        // edge.cdn.net (a pointer to 45) CNAME e1.cdn.net (its tail a
        // pointer to 50), its data at 71.
        let second = (vec![0xc0, 45], TYPE_CNAME, 60, vec![2, b'e', b'1', 0xc0, 50]);
        // e1.cdn.net (a pointer to 71) has the address.
        let third = (vec![0xc0, 71], TYPE_A, 120, vec![93, 184, 216, 34]);
        let answer = response(&q, 0, &[first, second, third]);
        let (found, ttl) = read_answer(&answer, "www.example.com", TYPE_A).unwrap();
        assert_eq!(found, vec![IpAddr::from([93, 184, 216, 34])]);
        assert_eq!(ttl, 60, "the shortest time to live along the chain");
    }

    #[test]
    fn a_chain_is_followed_in_any_order() {
        let q = query("a.example", TYPE_AAAA).unwrap();
        let six = "2001:db8::7".parse::<Ipv6Addr>().unwrap().octets().to_vec();
        let answer = response(
            &q,
            0,
            &[
                (labels("c.example"), TYPE_AAAA, 30, six),
                (labels("b.example"), TYPE_CNAME, 30, labels("c.example")),
                (QNAME.to_vec(), TYPE_CNAME, 30, labels("b.example")),
            ],
        );
        let (found, _) = read_answer(&answer, "a.example", TYPE_AAAA).unwrap();
        assert_eq!(found, vec!["2001:db8::7".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn records_for_other_names_are_left_out() {
        let q = query("a.example", TYPE_A).unwrap();
        let answer = response(
            &q,
            0,
            &[(labels("evil.example"), TYPE_A, 30, vec![6, 6, 6, 6]), (QNAME.to_vec(), TYPE_A, 30, vec![1, 2, 3, 4])],
        );
        let (found, _) = read_answer(&answer, "a.example", TYPE_A).unwrap();
        assert_eq!(found, vec![IpAddr::from([1, 2, 3, 4])]);
    }

    #[test]
    fn errors_and_damaged_answers_fail() {
        let q = query("gone.example", TYPE_A).unwrap();
        assert_eq!(
            read_answer(&response(&q, 3, &[]), "gone.example", TYPE_A),
            Err(DohError::NotFound("gone.example".into()))
        );
        assert!(matches!(read_answer(&response(&q, 2, &[]), "gone.example", TYPE_A), Err(DohError::Unanswered(_))));
        assert!(read_answer(&q, "gone.example", TYPE_A).is_err(), "a query is not an answer");
        let cut = response(&q, 0, &[(QNAME.to_vec(), TYPE_A, 30, vec![1, 2, 3, 4])]);
        assert!(read_answer(&cut[..cut.len() - 2], "gone.example", TYPE_A).is_err());
        // A pointer to itself.
        let mut looping = response(&q, 0, &[]);
        looping[6..8].copy_from_slice(&1u16.to_be_bytes());
        let at = looping.len() as u8;
        looping.extend_from_slice(&[0xc0, at]);
        assert!(read_answer(&looping, "gone.example", TYPE_A).is_err());
    }

    /// A stand-in DoH server on loopback: it answers each posted query from
    /// `answer`, and counts the queries.
    pub struct StandIn {
        pub url: String,
        pub queries: Arc<AtomicUsize>,
    }

    pub fn stand_in(answer: impl Fn(&str, u16, &[u8]) -> Vec<u8> + Send + Sync + 'static) -> StandIn {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/dns-query", listener.local_addr().unwrap());
        let queries = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&queries);
        let answer = Arc::new(answer);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (answer, counted) = (Arc::clone(&answer), Arc::clone(&counted));
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut stream = stream;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let mut length = 0;
                        loop {
                            let mut h = String::new();
                            reader.read_line(&mut h).unwrap();
                            if h == "\r\n" {
                                break;
                            }
                            if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                                length = v.trim().parse().unwrap();
                            }
                        }
                        let mut q = vec![0; length];
                        reader.read_exact(&mut q).unwrap();
                        counted.fetch_add(1, Ordering::SeqCst);
                        let (name, kind) = question(&q);
                        let body = answer(&name, kind, &q);
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/dns-message\r\nContent-Length: {}\r\n\r\n",
                            body.len()
                        );
                        stream.write_all(head.as_bytes()).unwrap();
                        stream.write_all(&body).unwrap();
                    }
                });
            }
        });
        StandIn { url, queries }
    }

    /// A resolver at `server`, which the tests reach over plain http.
    pub fn doh_at(server: &StandIn) -> Doh {
        Doh::new(
            DohEndpoint {
                url: server.url.clone(),
                host: "127.0.0.1".into(),
                bootstrap: vec![IpAddr::from([127, 0, 0, 1])],
            },
            None,
        )
    }

    #[test]
    fn queries_are_bound_to_the_adapter_and_stop_when_it_goes() {
        let server = stand_in(|_, kind, q| match kind {
            TYPE_A => response(q, 0, &[(QNAME.to_vec(), TYPE_A, 300, vec![127, 0, 0, 1])]),
            _ => response(q, 0, &[]),
        });
        let (route, stand_in) = crate::route::tests::bound_to_loopback();
        let endpoint = DohEndpoint {
            url: server.url.clone(),
            host: "127.0.0.1".into(),
            bootstrap: vec![IpAddr::from([127, 0, 0, 1])],
        };
        let doh = Doh::new(endpoint.clone(), route.binding.clone());
        assert_eq!(doh.lookup_blocking("bound.test").unwrap(), vec![IpAddr::from([127, 0, 0, 1])]);
        stand_in.gone.store(true, std::sync::atomic::Ordering::SeqCst);
        let fresh = Doh::new(endpoint, route.binding);
        match fresh.lookup_blocking("other.test") {
            Err(DohError::AdapterDown(down)) => assert_eq!(down.adapter, "Test adapter"),
            other => panic!("expected the adapter to be down, got {other:?}"),
        }
    }

    #[test]
    fn names_are_looked_up_cached_and_never_asked_of_the_system_when_it_fails() {
        let server = stand_in(|name, kind, q| match (name, kind) {
            ("files.test", TYPE_A) => response(q, 0, &[(QNAME.to_vec(), TYPE_A, 300, vec![127, 0, 0, 1])]),
            ("files.test", _) => response(q, 0, &[]),
            (_, _) => response(q, 3, &[]),
        });
        let doh = doh_at(&server);
        assert_eq!(doh.lookup_blocking("files.test").unwrap(), vec![IpAddr::from([127, 0, 0, 1])]);
        assert_eq!(server.queries.load(Ordering::SeqCst), 2, "A and AAAA");
        assert_eq!(doh.lookup_blocking("FILES.test.").unwrap(), vec![IpAddr::from([127, 0, 0, 1])]);
        assert_eq!(server.queries.load(Ordering::SeqCst), 2, "kept for its time to live");
        assert_eq!(doh.lookup_blocking("localhost").unwrap()[0], IpAddr::from([127, 0, 0, 1]), "not asked");
        assert_eq!(server.queries.load(Ordering::SeqCst), 2);
        // A name the resolver does not know fails, even one the system
        // knows.
        assert!(matches!(doh.lookup_blocking("missing.test"), Err(DohError::NotFound(_))));
        // A resolver that cannot be reached fails too.
        let gone = Doh::new(
            DohEndpoint {
                url: "http://127.0.0.1:9/dns-query".into(),
                host: "127.0.0.1".into(),
                bootstrap: vec![IpAddr::from([127, 0, 0, 1])],
            },
            None,
        );
        assert!(matches!(gone.lookup_blocking("files.test"), Err(DohError::Unanswered(_))));
    }
}
