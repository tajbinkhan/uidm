//! The debrid account's adapters. `DebridApi` reaches the services' official
//! APIs (the `Debrid` port) through the proxy the settings choose:
//! Real-Debrid's REST API 1.0, AllDebrid's API v4 and Premiumize's API. Each
//! answer and error code is read into the domain's `DebridError`. `KeyFile`
//! keeps the account's API key sealed (`dpapi`) in `debrid.bin` beside the
//! history (the `DebridKeys` port), as the proxies' passwords are.
//!
//! The key never goes in a log line: only the service and a link's host are
//! logged, and the key travels in an `Authorization` header, or for
//! Premiumize, whose API takes it only as a parameter, in the form body of
//! a POST and the query of a GET.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use udm_domain::address::Address;
use udm_domain::bytes::ByteCount;
use udm_domain::debrid::{DebridAccount, DebridError, DebridKey, DebridService, SupportedHosts, Unrestricted};
use udm_domain::download::Download;
use udm_domain::ports::{Debrid, DebridKeys, SettingsError};

use crate::connect::{config_for, ProxyRetries};
use crate::engine;
use crate::live_settings::LiveSettings;

/// The API's addresses, which tests point at a stand-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    pub real_debrid: String,
    pub all_debrid: String,
    pub premiumize: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            real_debrid: "https://api.real-debrid.com/rest/1.0".into(),
            all_debrid: "https://api.alldebrid.com/v4".into(),
            premiumize: "https://www.premiumize.me/api".into(),
        }
    }
}

/// The name AllDebrid's API asks each program to give as `agent`.
const AGENT: &str = "udm";
/// The most of an answer read: lists of hosts run to tens of kilobytes.
const MAX_ANSWER: ByteCount = ByteCount::mib(4);

/// The services' APIs, reached through the settings' proxy.
pub struct DebridApi {
    settings: Arc<LiveSettings>,
    retries: Arc<ProxyRetries>,
    endpoints: Endpoints,
}

/// A request to one of the APIs.
struct Call<'a> {
    method: &'a str,
    url: String,
    /// The `Authorization` header's bearer token.
    bearer: Option<&'a str>,
    /// A form to POST.
    form: Option<Vec<(&'a str, &'a str)>>,
}

impl DebridApi {
    pub fn new(settings: Arc<LiveSettings>, retries: Arc<ProxyRetries>) -> Self {
        DebridApi { settings, retries, endpoints: Endpoints::default() }
    }

    /// The same, asking the APIs at `endpoints`.
    pub fn at(mut self, endpoints: Endpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    /// Send `call` and read its answer as JSON, with its status.
    fn send(&self, call: Call<'_>) -> Result<(u16, Value), DebridError> {
        let settings = self.settings.get();
        let net =
            config_for(&Download { url: call.url.as_str().into(), ..Download::default() }, &settings, &self.retries);
        let mut headers = vec![
            ("User-Agent".to_string(), format!("UDM/{}", env!("CARGO_PKG_VERSION"))),
            ("Accept".to_string(), "application/json".to_string()),
        ];
        if let Some(token) = call.bearer {
            headers.push(("Authorization".to_string(), format!("Bearer {token}")));
        }
        let body = call.form.map(|fields| {
            headers.push(("Content-Type".to_string(), "application/x-www-form-urlencoded".to_string()));
            url::form_urlencoded::Serializer::new(String::new()).extend_pairs(fields).finish().into_bytes()
        });
        let (status, answer) =
            engine::call_api(call.method, &call.url, &headers, body.as_deref(), &net, MAX_ANSWER).map_err(network)?;
        match serde_json::from_slice::<Value>(&answer) {
            Ok(json) => Ok((status, json)),
            // A 204 or an empty answer reads as nothing said.
            Err(_) if answer.is_empty() && status < 400 => Ok((status, Value::Null)),
            // A gateway's or a load balancer's error page: the API is down.
            Err(_) if status >= 500 => Err(DebridError::Network(format!("the service answered HTTP {status}"))),
            Err(_) => Err(DebridError::Service(format!("an answer that is not the API's (HTTP {status})"))),
        }
    }

    /// `base`'s `path` with `query`, escaped.
    fn address(base: &str, path: &str, query: &[(&str, &str)]) -> String {
        let mut url = format!("{}/{path}", base.trim_end_matches('/'));
        if !query.is_empty() {
            url.push('?');
            url.push_str(&url::form_urlencoded::Serializer::new(String::new()).extend_pairs(query).finish());
        }
        url
    }

    fn rd(
        &self,
        method: &'static str,
        path: &str,
        key: &DebridKey,
        form: Option<Vec<(&str, &str)>>,
    ) -> Result<Value, DebridError> {
        let url = Self::address(&self.endpoints.real_debrid, path, &[]);
        let (status, json) = self.send(Call { method, url, bearer: Some(key.expose()), form })?;
        if status >= 400 || json.get("error").is_some() {
            return Err(real_debrid_error(status, &json));
        }
        Ok(json)
    }

    fn ad(&self, path: &str, key: &DebridKey, query: &[(&str, &str)]) -> Result<Value, DebridError> {
        let mut all = vec![("agent", AGENT)];
        all.extend_from_slice(query);
        let url = Self::address(&self.endpoints.all_debrid, path, &all);
        let (status, json) = self.send(Call { method: "GET", url, bearer: Some(key.expose()), form: None })?;
        if json.get("status").and_then(Value::as_str) != Some("success") {
            return Err(all_debrid_error(status, &json));
        }
        Ok(json.get("data").cloned().unwrap_or(Value::Null))
    }

    fn pm(
        &self,
        method: &'static str,
        path: &str,
        key: &DebridKey,
        fields: &[(&str, &str)],
    ) -> Result<Value, DebridError> {
        let mut all = vec![("apikey", key.expose())];
        all.extend_from_slice(fields);
        let (url, form) = match method {
            "POST" => (Self::address(&self.endpoints.premiumize, path, &[]), Some(all)),
            _ => (Self::address(&self.endpoints.premiumize, path, &all), None),
        };
        let (status, json) = self.send(Call { method, url, bearer: None, form })?;
        // Some answers (`services/list`) carry no status; only an error says
        // so.
        if json.get("status").and_then(Value::as_str) == Some("error") || status >= 400 {
            return Err(premiumize_error(status, &json));
        }
        Ok(json)
    }
}

/// An engine failure: the service was not reached. Its text may name the
/// address asked, which for Premiumize carries the key: that is hidden.
fn network(e: engine::EngineError) -> DebridError {
    DebridError::Network(udm_domain::report::scrub(&e.to_string()).into_owned())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn text(json: &Value, key: &str) -> Option<String> {
    json.get(key).and_then(Value::as_str).map(str::to_string).filter(|s| !s.is_empty())
}

fn size(json: &Value, key: &str) -> Option<ByteCount> {
    json.get(key).and_then(Value::as_u64).filter(|n| *n > 0).map(ByteCount::new)
}

/// A direct link from an answer, which must be an http or https address.
fn direct(link: Option<&str>) -> Result<Address, DebridError> {
    link.and_then(|l| Address::parse(l).ok())
        .filter(Address::is_web)
        .ok_or_else(|| DebridError::Service("no download link in the answer".into()))
}

/// Real-Debrid's errors: `{"error": "...", "error_code": n}`. The `error`
/// is an identifier such as `hoster_in_maintenance`; the ones a user can
/// meet get words of their own.
fn real_debrid_error(status: u16, json: &Value) -> DebridError {
    let message = match text(json, "error") {
        Some(id) => match id.as_str() {
            "internal_error" => "the service had an internal error".to_string(),
            "bad_parameter_value" | "missing_parameter" => "the service did not take the link".to_string(),
            "service_unavailable" => "the service is unavailable for now".to_string(),
            "action_already_done" => "the service did this already".to_string(),
            _ => id.replace('_', " "),
        },
        None => format!("HTTP {status}"),
    };
    match json.get("error_code").and_then(Value::as_i64) {
        // IP address not allowed: a VPN, a proxy or a data center.
        Some(22) => DebridError::IpNotAllowed,
        // Hoster in maintenance, or out of order for now.
        Some(17 | 19) => DebridError::HostUnavailable,
        // Service unavailable.
        Some(25) => DebridError::Network("the service is unavailable for now".into()),
        // Bad token, invalid login or password, account locked or not
        // activated.
        Some(8 | 12 | 13 | 14 | 15) => DebridError::BadKey,
        // Permission denied, hoster not available for free users.
        Some(9 | 20) => DebridError::NotPremium,
        Some(16) => DebridError::HostNotSupported,
        // Resource not found, file unavailable or not allowed, an
        // infringing file.
        Some(7 | 24 | 28 | 35) => DebridError::LinkDead,
        // Slow down, the hoster's limit, too many active downloads,
        // traffic exhausted, too many requests, fair usage.
        Some(5 | 18 | 21 | 23 | 34 | 36) => DebridError::Limit,
        Some(_) => DebridError::Service(message),
        None if status == 401 => DebridError::BadKey,
        None if status == 403 => DebridError::NotPremium,
        None => DebridError::Service(message),
    }
}

/// AllDebrid's errors: `{"status": "error", "error": {"code", "message"}}`.
fn all_debrid_error(status: u16, json: &Value) -> DebridError {
    let error = json.get("error").cloned().unwrap_or(Value::Null);
    let code = text(&error, "code").unwrap_or_default();
    let message = text(&error, "message").unwrap_or_else(|| format!("HTTP {status}"));
    match code.as_str() {
        "AUTH_MISSING_APIKEY"
        | "AUTH_BAD_APIKEY"
        | "AUTH_BLOCKED"
        | "AUTH_USER_BANNED"
        | "AUTH_MISSING_AGENT"
        | "AUTH_BAD_AGENT" => DebridError::BadKey,
        "MUST_BE_PREMIUM" | "FREE_TRIAL_LIMIT_REACHED" | "LINK_HOST_NOT_AVAILABLE_FREE" => DebridError::NotPremium,
        "LINK_HOST_NOT_SUPPORTED" | "LINK_NOT_SUPPORTED" => DebridError::HostNotSupported,
        "LINK_DOWN" | "LINK_IS_MISSING" | "LINK_ERROR" => DebridError::LinkDead,
        "LINK_PASS_PROTECTED" => {
            DebridError::Service("the link is password-protected, which the service cannot unlock".into())
        }
        "LINK_HOST_LIMIT_REACHED" | "LINK_TOO_MANY_DOWNLOADS" | "LINK_HOST_FULL" | "LINK_LIMIT_REACHED" => {
            DebridError::Limit
        }
        "LINK_TEMPORARY_UNAVAILABLE" | "LINK_HOST_UNAVAILABLE" => DebridError::HostUnavailable,
        _ if status == 401 => DebridError::BadKey,
        _ => DebridError::Service(message),
    }
}

/// Premiumize's errors: `{"status": "error", "message": "..."}`, without a
/// code, so the message's words decide.
fn premiumize_error(status: u16, json: &Value) -> DebridError {
    let message = text(json, "message").unwrap_or_else(|| format!("HTTP {status}"));
    let lower = message.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    // "Fair use limit" before "premium": a premium account's limit names
    // its premium.
    if status == 401
        || has(&["not logged in", "invalid api key", "invalid apikey", "unauthorized", "authentication failed"])
    {
        DebridError::BadKey
    } else if has(&["limit", "fair use", "too many"]) {
        DebridError::Limit
    } else if has(&["premium"]) {
        DebridError::NotPremium
    } else if has(&["not supported", "unsupported"]) {
        DebridError::HostNotSupported
    } else if has(&["not found", "offline", "removed", "deleted", "does not exist"]) {
        DebridError::LinkDead
    } else {
        DebridError::Service(message)
    }
}

/// The strings of `json`'s array `key`.
fn strings(json: &Value, key: &str) -> Vec<String> {
    json.get(key)
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

impl Debrid for DebridApi {
    fn hosts(&self, service: DebridService, key: &DebridKey) -> Result<SupportedHosts, DebridError> {
        let hosts = match service {
            DebridService::RealDebrid => {
                let json = self.rd("GET", "hosts/domains", key, None)?;
                json.as_array()
                    .map(|l| l.iter().filter_map(Value::as_str).map(str::to_string).collect())
                    .unwrap_or_default()
            }
            DebridService::AllDebrid => strings(&self.ad("hosts/domains", key, &[])?, "hosts"),
            DebridService::Premiumize => {
                let json = self.pm("GET", "services/list", key, &[])?;
                let mut hosts = strings(&json, "directdl");
                // Other names of the same hosts.
                let aliases: Vec<String> = json
                    .get("aliases")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .filter(|(host, _)| hosts.contains(host))
                    .flat_map(|(_, names)| names.as_array().into_iter().flatten().filter_map(Value::as_str))
                    .map(str::to_string)
                    .collect();
                hosts.extend(aliases);
                hosts
            }
        };
        let hosts = SupportedHosts::new(hosts);
        tracing::info!(service = %service, hosts = hosts.len(), "read the debrid service's hosts");
        Ok(hosts)
    }

    fn unrestrict(&self, service: DebridService, key: &DebridKey, link: &Address) -> Result<Unrestricted, DebridError> {
        match service {
            DebridService::RealDebrid => {
                let json = self.rd("POST", "unrestrict/link", key, Some(vec![("link", link.as_str())]))?;
                Ok(Unrestricted {
                    address: direct(json.get("download").and_then(Value::as_str))?,
                    filename: text(&json, "filename"),
                    size: size(&json, "filesize"),
                })
            }
            DebridService::AllDebrid => {
                let data = self.ad("link/unlock", key, &[("link", link.as_str())])?;
                let link = data.get("link").and_then(Value::as_str).filter(|l| !l.is_empty());
                if link.is_none() && data.get("delayed").is_some() {
                    return Err(DebridError::Service(
                        "the service is still preparing the file; try again later".into(),
                    ));
                }
                Ok(Unrestricted {
                    address: direct(link)?,
                    filename: text(&data, "filename"),
                    size: size(&data, "filesize"),
                })
            }
            DebridService::Premiumize => {
                let json = self.pm("POST", "transfer/directdl", key, &[("src", link.as_str())])?;
                let first = json.get("content").and_then(Value::as_array).and_then(|c| c.first()).cloned();
                match first {
                    Some(file) => Ok(Unrestricted {
                        address: direct(file.get("link").and_then(Value::as_str))?,
                        filename: text(&file, "path").map(|p| p.rsplit('/').next().unwrap_or(&p).to_string()),
                        size: size(&file, "size"),
                    }),
                    None => Ok(Unrestricted {
                        address: direct(json.get("location").and_then(Value::as_str))?,
                        filename: text(&json, "filename"),
                        size: size(&json, "filesize"),
                    }),
                }
            }
        }
    }

    fn account(&self, service: DebridService, key: &DebridKey) -> Result<DebridAccount, DebridError> {
        match service {
            DebridService::RealDebrid => {
                let json = self.rd("GET", "user", key, None)?;
                let premium = text(&json, "type").as_deref() == Some("premium");
                // `premium` is the seconds left.
                let left = json.get("premium").and_then(Value::as_u64).filter(|s| *s > 0);
                Ok(DebridAccount {
                    username: text(&json, "username"),
                    premium,
                    premium_until: left.filter(|_| premium).map(|s| now() + s),
                })
            }
            DebridService::AllDebrid => {
                let user = self.ad("user", key, &[])?.get("user").cloned().unwrap_or(Value::Null);
                let premium = user.get("isPremium").and_then(Value::as_bool).unwrap_or(false);
                Ok(DebridAccount {
                    username: text(&user, "username"),
                    premium,
                    premium_until: user.get("premiumUntil").and_then(Value::as_u64).filter(|t| premium && *t > 0),
                })
            }
            DebridService::Premiumize => {
                let json = self.pm("GET", "account/info", key, &[])?;
                // `premium_until` is a time, or false for a free account.
                let until = json.get("premium_until").and_then(Value::as_u64).filter(|t| *t > now());
                let username = json.get("customer_id").and_then(|c| match c {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                });
                Ok(DebridAccount { username, premium: until.is_some(), premium_until: until })
            }
        }
    }
}

/// Where the debrid account's key is kept: `debrid.bin` beside the history,
/// in the data folder.
pub fn key_path() -> io::Result<PathBuf> {
    Ok(crate::history::history_path()?.with_file_name("debrid.bin"))
}

/// The service a debrid key is stored for, if any.
pub fn stored_service() -> Option<DebridService> {
    KeyFile::new().stored()
}

/// The account's key and the service it is for, sealed together in a file
/// of their own, so a key is only ever sent to its own service.
pub struct KeyFile {
    path: Option<PathBuf>,
}

/// What `KeyFile` seals.
#[derive(serde::Serialize, serde::Deserialize)]
struct Sealed {
    service: DebridService,
    key: String,
}

impl KeyFile {
    /// The key at `key_path`.
    pub fn new() -> Self {
        KeyFile { path: key_path().ok() }
    }

    /// The key at `path`, for tests.
    pub fn at(path: PathBuf) -> Self {
        KeyFile { path: Some(path) }
    }

    /// The service and key stored. A file that does not open, or an older
    /// one without its service, holds none.
    fn read(&self) -> Option<(DebridService, DebridKey)> {
        let sealed = fs::read(self.path.as_ref()?).ok()?;
        let plain = crate::dpapi::unprotect(&sealed).ok()?;
        let read: Sealed = serde_json::from_slice(&plain).ok()?;
        Some((read.service, DebridKey::new(&read.key)?))
    }

    fn path(&self) -> Result<&PathBuf, SettingsError> {
        self.path.as_ref().ok_or_else(|| refused(io::Error::other("no data folder")))
    }
}

impl Default for KeyFile {
    fn default() -> Self {
        Self::new()
    }
}

fn refused(e: io::Error) -> SettingsError {
    SettingsError::Refused(format!("could not store the debrid key: {e}"))
}

impl DebridKeys for KeyFile {
    fn stored(&self) -> Option<DebridService> {
        self.read().map(|(service, _)| service)
    }

    fn key(&self, service: DebridService) -> Option<DebridKey> {
        self.read().filter(|(stored, _)| *stored == service).map(|(_, key)| key)
    }

    fn set_key(&self, service: DebridService, key: &DebridKey) -> Result<(), SettingsError> {
        let path = self.path()?;
        let plain = serde_json::to_vec(&Sealed { service, key: key.expose().to_string() })
            .map_err(|e| refused(io::Error::other(e)))?;
        let sealed = crate::dpapi::protect(&plain).map_err(refused)?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(refused)?;
        }
        crate::settings::write_atomic(path, &sealed).map_err(refused)
    }

    fn remove_key(&self) -> Result<(), SettingsError> {
        match fs::remove_file(self.path()?) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(refused(e)),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use udm_domain::settings::{ProxyMode, Settings};

    /// One request the stand-in took: its method, path with query, the
    /// `Authorization` header and the body.
    #[derive(Debug, Clone, Default)]
    struct Asked {
        method: String,
        path: String,
        auth: Option<String>,
        body: String,
    }

    type Answer = Box<dyn Fn(&Asked) -> (u16, String) + Send + Sync>;

    /// A stand-in for a service's API on loopback, answering each request
    /// with `answer`, and what it was asked.
    fn stand_in(answer: Answer) -> (String, Arc<Mutex<Vec<Asked>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let mut asked = Asked {
                    method: parts.next().unwrap_or_default().to_string(),
                    path: parts.next().unwrap_or_default().to_string(),
                    ..Asked::default()
                };
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    let header = header.trim_end();
                    if header.is_empty() {
                        break;
                    }
                    let (name, value) = header.split_once(':').unwrap();
                    match name.to_ascii_lowercase().as_str() {
                        "content-length" => length = value.trim().parse().unwrap(),
                        "authorization" => asked.auth = Some(value.trim().to_string()),
                        _ => {}
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                asked.body = String::from_utf8(body).unwrap();
                let (status, text) = answer(&asked);
                log.lock().unwrap().push(asked);
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        (url, seen)
    }

    fn api(url: &str) -> DebridApi {
        let settings = Settings { proxy_mode: ProxyMode::None, ..Settings::default() };
        DebridApi::new(Arc::new(LiveSettings::new(settings, None)), Arc::default()).at(Endpoints {
            real_debrid: format!("{url}/rest/1.0"),
            all_debrid: format!("{url}/v4"),
            premiumize: format!("{url}/api"),
        })
    }

    fn key() -> DebridKey {
        DebridKey::new("SECRETKEY").unwrap()
    }

    fn link(text: &str) -> Address {
        Address::parse(text).unwrap()
    }

    #[test]
    fn real_debrid_unlocks_with_its_token() {
        let (url, seen) = stand_in(Box::new(|asked| {
            match asked.path.as_str() {
            "/rest/1.0/hosts/domains" => (200, r#"["rapidgator.net","1fichier.com"]"#.into()),
            "/rest/1.0/unrestrict/link" if asked.body.contains("dead") => {
                (503, r#"{"error":"unavailable_file","error_code":24}"#.into())
            }
            "/rest/1.0/unrestrict/link" => (
                200,
                r#"{"id":"X","filename":"a.zip","filesize":1234,"link":"https://rapidgator.net/f","download":"https://a1.download.real-debrid.com/d/X/a.zip"}"#
                    .into(),
            ),
            "/rest/1.0/user" => (200, r#"{"id":1,"username":"ann","type":"premium","premium":3600}"#.into()),
            _ => (404, r#"{"error":"unknown_ressource","error_code":7}"#.into()),
        }
        }));
        let api = api(&url);
        let hosts = api.hosts(DebridService::RealDebrid, &key()).unwrap();
        assert!(hosts.covers("rapidgator.net") && hosts.covers("1fichier.com"));
        let done = api.unrestrict(DebridService::RealDebrid, &key(), &link("https://rapidgator.net/file/1")).unwrap();
        assert_eq!(done.address.as_str(), "https://a1.download.real-debrid.com/d/X/a.zip");
        assert_eq!((done.filename.as_deref(), done.size), (Some("a.zip"), Some(ByteCount::new(1234))));
        let dead = api.unrestrict(DebridService::RealDebrid, &key(), &link("https://rapidgator.net/dead"));
        assert_eq!(dead, Err(DebridError::LinkDead));
        let account = api.account(DebridService::RealDebrid, &key()).unwrap();
        assert_eq!((account.username.as_deref(), account.premium), (Some("ann"), true));
        assert!(account.premium_until.is_some_and(|t| t > now()));
        let seen = seen.lock().unwrap();
        let post = seen.iter().find(|a| a.path.ends_with("unrestrict/link")).unwrap();
        assert_eq!(post.method, "POST");
        assert_eq!(post.body, "link=https%3A%2F%2Frapidgator.net%2Ffile%2F1");
        assert!(seen.iter().all(|a| a.auth.as_deref() == Some("Bearer SECRETKEY")));
        assert!(seen.iter().all(|a| !a.path.contains("SECRETKEY") && !a.body.contains("SECRETKEY")));
    }

    #[test]
    fn real_debrid_errors_read_by_code() {
        let cases = [
            (401, r#"{"error":"bad_token","error_code":8}"#, DebridError::BadKey),
            (403, r#"{"error":"permission_denied","error_code":9}"#, DebridError::NotPremium),
            (503, r#"{"error":"hoster_unsupported","error_code":16}"#, DebridError::HostNotSupported),
            (503, r#"{"error":"traffic_exhausted","error_code":23}"#, DebridError::Limit),
            (503, r#"{"error":"hoster_in_maintenance","error_code":17}"#, DebridError::HostUnavailable),
            (403, r#"{"error":"ip_not_allowed","error_code":22}"#, DebridError::IpNotAllowed),
            (503, r#"{"error":"some_new_thing","error_code":99}"#, DebridError::Service("some new thing".into())),
            (
                500,
                r#"{"error":"internal_error","error_code":-1}"#,
                DebridError::Service("the service had an internal error".into()),
            ),
            (401, "{}", DebridError::BadKey),
        ];
        for (status, body, want) in cases {
            let json: Value = serde_json::from_str(body).unwrap();
            assert_eq!(real_debrid_error(status, &json), want, "{body}");
        }
    }

    #[test]
    fn alldebrid_unlocks_with_its_agent_and_key() {
        let (url, seen) = stand_in(Box::new(|asked| {
            let path = asked.path.split('?').next().unwrap_or_default();
            match path {
                "/v4/hosts/domains" => (
                    200,
                    r#"{"status":"success","data":{"hosts":["rapidgator.net","uptobox.com"],"streams":["youtube.com"],"redirectors":[]}}"#
                        .into(),
                ),
                "/v4/link/unlock" if asked.path.contains("dead") => (
                    200,
                    r#"{"status":"error","error":{"code":"LINK_DOWN","message":"This link is not available on the file hoster website"}}"#
                        .into(),
                ),
                "/v4/link/unlock" if asked.path.contains("slow") => {
                    (200, r#"{"status":"success","data":{"link":"","delayed":42}}"#.into())
                }
                "/v4/link/unlock" => (
                    200,
                    r#"{"status":"success","data":{"link":"https://x7.debrid.it/dl/abc/a.zip","host":"rapidgator","filename":"a.zip","filesize":99}}"#
                        .into(),
                ),
                "/v4/user" => (
                    200,
                    r#"{"status":"success","data":{"user":{"username":"bob","isPremium":true,"premiumUntil":4102444800}}}"#.into(),
                ),
                _ => (200, r#"{"status":"error","error":{"code":"AUTH_BAD_APIKEY","message":"The auth apikey is invalid"}}"#.into()),
            }
        }));
        let api = api(&url);
        let hosts = api.hosts(DebridService::AllDebrid, &key()).unwrap();
        assert!(hosts.covers("uptobox.com") && !hosts.covers("youtube.com"), "streams are not file hosts");
        let done = api.unrestrict(DebridService::AllDebrid, &key(), &link("https://rapidgator.net/file/2")).unwrap();
        assert_eq!(done.address.as_str(), "https://x7.debrid.it/dl/abc/a.zip");
        assert_eq!(done.size, Some(ByteCount::new(99)));
        let dead = api.unrestrict(DebridService::AllDebrid, &key(), &link("https://rapidgator.net/dead"));
        assert_eq!(dead, Err(DebridError::LinkDead));
        let slow = api.unrestrict(DebridService::AllDebrid, &key(), &link("https://rapidgator.net/slow"));
        assert!(matches!(slow, Err(DebridError::Service(_))));
        let account = api.account(DebridService::AllDebrid, &key()).unwrap();
        assert_eq!(
            account,
            DebridAccount { username: Some("bob".into()), premium: true, premium_until: Some(4102444800) }
        );
        let seen = seen.lock().unwrap();
        let unlock = seen.iter().find(|a| a.path.starts_with("/v4/link/unlock")).unwrap();
        assert_eq!(unlock.path, "/v4/link/unlock?agent=udm&link=https%3A%2F%2Frapidgator.net%2Ffile%2F2");
        assert!(seen.iter().all(|a| a.auth.as_deref() == Some("Bearer SECRETKEY") && !a.path.contains("SECRETKEY")));
    }

    #[test]
    fn alldebrid_errors_read_by_code() {
        let error = |code: &str| serde_json::json!({"status": "error", "error": {"code": code, "message": "m"}});
        assert_eq!(all_debrid_error(200, &error("AUTH_BAD_APIKEY")), DebridError::BadKey);
        assert_eq!(all_debrid_error(200, &error("MUST_BE_PREMIUM")), DebridError::NotPremium);
        assert_eq!(all_debrid_error(200, &error("LINK_HOST_NOT_SUPPORTED")), DebridError::HostNotSupported);
        assert_eq!(all_debrid_error(200, &error("LINK_HOST_LIMIT_REACHED")), DebridError::Limit);
        assert_eq!(all_debrid_error(200, &error("LINK_HOST_UNAVAILABLE")), DebridError::HostUnavailable);
        assert!(
            matches!(all_debrid_error(200, &error("LINK_PASS_PROTECTED")), DebridError::Service(m) if m.contains("password"))
        );
        assert_eq!(all_debrid_error(200, &error("SOMETHING_NEW")), DebridError::Service("m".into()));
    }

    #[test]
    fn premiumize_unlocks_with_its_api_key() {
        let (url, seen) = stand_in(Box::new(|asked| {
            let path = asked.path.split('?').next().unwrap_or_default();
            match path {
                "/api/services/list" => (
                    200,
                    // As documented: no status.
                    r#"{"directdl":["rapidgator.net","nitroflare.com"],"cache":["x.com"],"aliases":{"rapidgator.net":["rg.to"]},"regexpatterns":{},"fairusefactor":{}}"#
                        .into(),
                ),
                "/api/transfer/directdl" if asked.body.contains("dead") => {
                    (200, r#"{"status":"error","message":"File not found on the host."}"#.into())
                }
                "/api/transfer/directdl" => (
                    200,
                    r#"{"status":"success","content":[{"path":"dir/a.zip","size":55,"link":"https://e1.energycdn.com/dl/a.zip"}]}"#.into(),
                ),
                "/api/account/info" => {
                    (200, r#"{"status":"success","customer_id":12345,"premium_until":4102444800,"limit_used":0.1}"#.into())
                }
                _ => (200, r#"{"status":"error","message":"Not logged in."}"#.into()),
            }
        }));
        let api = api(&url);
        let hosts = api.hosts(DebridService::Premiumize, &key()).unwrap();
        assert!(hosts.covers("rg.to") && hosts.covers("nitroflare.com") && !hosts.covers("x.com"));
        let done = api.unrestrict(DebridService::Premiumize, &key(), &link("https://rapidgator.net/file/3")).unwrap();
        assert_eq!(done.address.as_str(), "https://e1.energycdn.com/dl/a.zip");
        assert_eq!(done.filename.as_deref(), Some("a.zip"));
        let dead = api.unrestrict(DebridService::Premiumize, &key(), &link("https://rapidgator.net/dead"));
        assert_eq!(dead, Err(DebridError::LinkDead));
        let account = api.account(DebridService::Premiumize, &key()).unwrap();
        assert_eq!(
            account,
            DebridAccount { username: Some("12345".into()), premium: true, premium_until: Some(4102444800) }
        );
        let seen = seen.lock().unwrap();
        let post = seen.iter().find(|a| a.path == "/api/transfer/directdl").unwrap();
        assert_eq!(post.method, "POST");
        assert_eq!(post.body, "apikey=SECRETKEY&src=https%3A%2F%2Frapidgator.net%2Ffile%2F3");
    }

    #[test]
    fn premiumize_errors_read_by_their_words() {
        let error = |message: &str| serde_json::json!({"status": "error", "message": message});
        assert_eq!(premiumize_error(200, &error("Not logged in.")), DebridError::BadKey);
        assert_eq!(premiumize_error(200, &error("You need a premium membership")), DebridError::NotPremium);
        assert_eq!(premiumize_error(200, &error("This hoster is not supported")), DebridError::HostNotSupported);
        assert_eq!(premiumize_error(200, &error("Fair use limit reached")), DebridError::Limit);
        assert_eq!(premiumize_error(200, &error("Premium fair use limit reached")), DebridError::Limit);
        assert_eq!(premiumize_error(200, &error("Invalid API key.")), DebridError::BadKey);
        assert_eq!(
            premiumize_error(200, &error("Host authorisation pending")),
            DebridError::Service("Host authorisation pending".into())
        );
        assert_eq!(premiumize_error(200, &error("Something else")), DebridError::Service("Something else".into()));
    }

    #[test]
    fn a_service_that_is_not_there_is_a_network_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let got = api(&url).account(DebridService::RealDebrid, &key());
        assert!(matches!(got, Err(DebridError::Network(_))), "{got:?}");
        let got = api(&url).account(DebridService::Premiumize, &key());
        assert!(matches!(&got, Err(DebridError::Network(why)) if !why.contains("SECRETKEY")), "{got:?}");
        let (url, _) = stand_in(Box::new(|_| (502, "<html>bad gateway</html>".into())));
        assert!(matches!(api(&url).account(DebridService::RealDebrid, &key()), Err(DebridError::Network(_))));
        let (url, _) = stand_in(Box::new(|_| (200, "<html>a page</html>".into())));
        assert!(matches!(api(&url).account(DebridService::RealDebrid, &key()), Err(DebridError::Service(_))));
    }

    #[test]
    fn the_key_is_sealed_and_removed() {
        let dir = std::env::temp_dir().join(format!("udm_debrid_key_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let file = KeyFile::at(dir.join("debrid.bin"));
        assert!(file.key(DebridService::RealDebrid).is_none() && file.stored().is_none());
        file.set_key(DebridService::RealDebrid, &key()).unwrap();
        let sealed = fs::read(dir.join("debrid.bin")).unwrap();
        assert!(!sealed.windows(9).any(|w| w == b"SECRETKEY"));
        assert_eq!(file.key(DebridService::RealDebrid), Some(key()));
        assert_eq!(file.stored(), Some(DebridService::RealDebrid));
        // The key is Real-Debrid's: another service never gets it.
        assert_eq!(file.key(DebridService::AllDebrid), None);
        assert_eq!(file.key(DebridService::Premiumize), None);
        file.remove_key().unwrap();
        assert!(file.stored().is_none() && !dir.join("debrid.bin").exists());
        file.remove_key().unwrap();
        // A file sealed with a bare key, without its service, holds none.
        let bare = crate::dpapi::protect(b"SECRETKEY").unwrap();
        crate::settings::write_atomic(&dir.join("debrid.bin"), &bare).unwrap();
        assert!(file.stored().is_none() && file.key(DebridService::RealDebrid).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}
