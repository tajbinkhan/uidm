//! The site grabber: crawl a site from a start page and list the files it
//! links to, such as every PDF on a documentation site.
//!
//! The crawl is polite and bounded: it reads `robots.txt` and skips what it
//! disallows, waits between requests (longer if `Crawl-delay` asks), follows
//! links only so many levels deep, and stops at a page and size budget.
//! Pages are untrusted input: they are parsed with a real HTML parser
//! (html5ever), and only http and https links are followed or listed.
//!
//! The crawler takes its fetch function as an argument, so it is tested
//! without a network; the daemon passes one that uses the engine's client
//! (proxy, logins).

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use url::Url;

/// What to crawl and how far.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrabRequest {
    pub start: String,
    /// How many links deep to follow from the start page; 0 reads only it.
    pub depth: u32,
    /// Follow links only on the start page's host.
    pub same_site: bool,
    /// File types to keep, such as `pdf`.
    pub types: Vec<String>,
    /// Stop after reading this many pages.
    pub max_pages: u64,
    /// Stop after reading this many bytes of pages.
    pub max_bytes: u64,
    /// Wait at least this long between requests, in milliseconds.
    pub delay_ms: u64,
    /// Explore only pages whose address matches one of these (see
    /// `pattern_matches`); empty explores every page in reach.
    #[serde(default)]
    pub explore_only: Vec<String>,
    /// Never explore pages whose address matches one of these.
    #[serde(default)]
    pub explore_skip: Vec<String>,
    /// Keep only files whose address matches one of these.
    #[serde(default)]
    pub files_only: Vec<String>,
    /// Never keep files whose address matches one of these.
    #[serde(default)]
    pub files_skip: Vec<String>,
    /// With `same_site`, also explore the start domain's subdomains.
    #[serde(default)]
    pub subdomains: bool,
    /// Do not explore above the start page's folder on its site.
    #[serde(default)]
    pub no_parent: bool,
    /// Keep only files of at least this many bytes. Sizes are asked with a
    /// request per file; a file whose size is not known is kept.
    #[serde(default)]
    pub min_size: Option<u64>,
    #[serde(default)]
    pub max_size: Option<u64>,
    /// List a file once when the same name and size turn up at another
    /// address.
    #[serde(default)]
    pub hide_duplicates: bool,
    /// Send the browser's cookies for the start site with its pages, so
    /// pages behind a sign-in form can be read. The browser extension
    /// supplies them when the crawl starts.
    #[serde(default)]
    pub browser_cookies: bool,
    /// Save for offline browsing: files under folders that mirror their
    /// addresses, and maybe the pages too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror: Option<Mirror>,
    /// Read each page as the browser shows it, after its scripts ran, so
    /// links that scripts add are found too. Slower: the browser extension
    /// opens each page in a background tab.
    #[serde(default)]
    pub render_scripts: bool,
    /// When pages are rendered, leave out the pages their scripts open in
    /// new windows; otherwise those are links to follow too.
    #[serde(default = "yes")]
    pub ignore_popups: bool,
    /// How many pages are read at once, 1 to 10.
    #[serde(default = "one")]
    pub explore_at_once: u32,
    /// How many of the found files the queue they go to downloads at once,
    /// 1 to 10.
    #[serde(default = "three")]
    pub download_at_once: u32,
    /// Keep each file's link text, and give it to the download as its
    /// description.
    #[serde(default)]
    pub link_text: bool,
    /// File types never kept, even when `types` takes them (as `*` does).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_types: Vec<String>,
    /// Keep only files on the start site (and, with `subdomains`, its
    /// subdomains).
    #[serde(default)]
    pub files_this_site: bool,
    /// Where the found files are saved, when the crawl keeps no mirror.
    #[serde(default)]
    pub save_to: SaveTo,
    /// Add each file to this queue as soon as it is found, and start the
    /// queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_found: Option<u64>,
    /// Follow links on the start site however deep they go; the page and
    /// size budgets still stop the crawl.
    #[serde(default)]
    pub whole_site: bool,
    /// Without `same_site`, how many links in a row to follow on other
    /// sites. `None` counts them in `depth` with the rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_depth: Option<u32>,
    /// The user name for the start site, when the project keeps its own
    /// login; the password is in Credential Manager (`project_login_target`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_user: Option<String>,
}

fn yes() -> bool {
    true
}

fn one() -> u32 {
    1
}

fn three() -> u32 {
    3
}

/// Where a crawl's files are saved, when it keeps no mirror.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "to", content = "name")]
pub enum SaveTo {
    /// Each file to the folder of its own category.
    #[default]
    Categories,
    /// Every file to the folder of the category with this name.
    Category(String),
    /// Every file to this folder, a full path, without subfolders.
    Folder(String),
}

/// The Credential Manager entry for the grabber login `user` on the site
/// of `start`, so two projects on one site keep their own accounts.
pub fn project_login_target(start: &str, user: &str) -> String {
    format!("{}/{}", host_login_target(start), user.trim())
}

/// The entry older versions kept one grabber password per site under.
fn host_login_target(start: &str) -> String {
    let host = Url::parse(start.trim()).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default();
    format!("UIDM/grabber/{host}")
}

/// The saved password of the grabber login `user` on the site of `start`,
/// or the site's one saved by an older version.
pub fn project_password(start: &str, user: &str) -> Option<String> {
    crate::net::load_credential(&project_login_target(start, user)).or_else(|| crate::net::load_credential(&host_login_target(start)))
}

/// Where and how a crawl is saved for offline browsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mirror {
    /// The folder the site's folders are made under, a full path.
    pub folder: String,
    /// Save the pages read too, with their links to saved files and pages
    /// changed to the local copies.
    #[serde(default)]
    pub pages: bool,
    /// Replace files already there; otherwise they are skipped.
    #[serde(default)]
    pub overwrite: bool,
}

/// Pages a crawl should never open, so it does not sign itself out.
pub const LOGOUT_PATTERNS: [&str; 5] = ["*logout*", "*logoff*", "*log-out*", "*signout*", "*sign-out*"];

pub const DEPTH_RANGE: (u32, u32) = (0, 10);
/// Pages read at once, and downloads run at once.
pub const AT_ONCE_RANGE: (u32, u32) = (1, 10);
pub const MAX_PAGES_RANGE: (u64, u64) = (1, 10_000);
pub const MAX_MB_RANGE: (u64, u64) = (1, 2_000);
pub const DELAY_MS_RANGE: (u64, u64) = (200, 60_000);
/// A `Crawl-delay` longer than this is capped, so one site cannot stall a
/// crawl for hours.
const MAX_CRAWL_DELAY: Duration = Duration::from_secs(30);
/// Most bytes read from one page.
pub const MAX_PAGE_BYTES: u64 = 5 * 1024 * 1024;
/// The name the crawler gives in `robots.txt` matching.
pub const USER_AGENT: &str = "UIDM";

impl Default for GrabRequest {
    fn default() -> Self {
        GrabRequest {
            start: String::new(),
            depth: 2,
            same_site: true,
            types: vec!["pdf".into()],
            max_pages: 200,
            max_bytes: 50 * 1024 * 1024,
            delay_ms: 1000,
            explore_only: Vec::new(),
            explore_skip: LOGOUT_PATTERNS.iter().map(|p| p.to_string()).collect(),
            files_only: Vec::new(),
            files_skip: Vec::new(),
            subdomains: false,
            no_parent: false,
            min_size: None,
            max_size: None,
            hide_duplicates: true,
            browser_cookies: false,
            mirror: None,
            render_scripts: false,
            ignore_popups: true,
            explore_at_once: 1,
            download_at_once: 3,
            link_text: false,
            skip_types: Vec::new(),
            files_this_site: false,
            save_to: SaveTo::Categories,
            download_found: None,
            whole_site: false,
            other_depth: None,
            login_user: None,
        }
    }
}

impl GrabRequest {
    pub fn validate(&self) -> Result<(), String> {
        let start = Url::parse(self.start.trim()).map_err(|_| "The start page must be a full web address.".to_string())?;
        if !matches!(start.scheme(), "http" | "https") {
            return Err("The start page must start with http:// or https://.".into());
        }
        let in_range = |v: u64, (lo, hi): (u64, u64)| (lo..=hi).contains(&v);
        let depth_range = (DEPTH_RANGE.0 as u64, DEPTH_RANGE.1 as u64);
        if !in_range(self.depth as u64, depth_range) || self.other_depth.is_some_and(|d| !in_range(d as u64, depth_range)) {
            return Err(format!("The depth must be between {} and {}.", DEPTH_RANGE.0, DEPTH_RANGE.1));
        }
        let at_once = (AT_ONCE_RANGE.0 as u64, AT_ONCE_RANGE.1 as u64);
        if !in_range(self.explore_at_once as u64, at_once) || !in_range(self.download_at_once as u64, at_once) {
            return Err(format!("Files at once must be between {} and {}.", AT_ONCE_RANGE.0, AT_ONCE_RANGE.1));
        }
        match &self.save_to {
            SaveTo::Category(name) if name.trim().is_empty() => return Err("Choose the category to save the files in.".into()),
            SaveTo::Folder(folder) if !Path::new(folder.trim()).is_absolute() => {
                return Err(format!("The folder to save the files in must be a full path: {folder}"));
            }
            _ => {}
        }
        if !in_range(self.max_pages, MAX_PAGES_RANGE) {
            return Err(format!("The page budget must be between {} and {}.", MAX_PAGES_RANGE.0, MAX_PAGES_RANGE.1));
        }
        let mb = self.max_bytes / (1024 * 1024);
        if !in_range(mb, MAX_MB_RANGE) {
            return Err(format!("The size budget must be between {} and {} MB.", MAX_MB_RANGE.0, MAX_MB_RANGE.1));
        }
        if !in_range(self.delay_ms, DELAY_MS_RANGE) {
            return Err(format!(
                "The wait between pages must be between {} and {} ms.",
                DELAY_MS_RANGE.0, DELAY_MS_RANGE.1
            ));
        }
        if self.types.is_empty() {
            return Err("Choose at least one file type to keep.".into());
        }
        if let Some(m) = self.mirror.as_ref().filter(|m| !Path::new(m.folder.trim()).is_absolute()) {
            return Err(format!("The folder to keep the site in must be a full path: {}", m.folder));
        }
        if let (Some(lo), Some(hi)) = (self.min_size, self.max_size) {
            if lo > hi {
                return Err("The smallest file size is larger than the largest.".into());
            }
        }
        Ok(())
    }
}

/// Whether `url` matches `pattern`, ignoring case: `*` matches any run of
/// characters, a pattern without a scheme also matches without one, and a
/// pattern without `*` matches anywhere in the address (`/docs/` keeps
/// every address with /docs/ in it).
pub fn pattern_matches(pattern: &str, url: &str) -> bool {
    let p = pattern.trim().to_ascii_lowercase();
    let u = url.trim().to_ascii_lowercase();
    if p.is_empty() {
        return false;
    }
    if !p.contains('*') {
        return u.contains(&p);
    }
    let bare = u.split_once("://").map_or(u.as_str(), |(_, rest)| rest);
    glob(&p, &u) || (!p.contains("://") && glob(&p, bare))
}

/// `*` matches any run of characters; the rest must match exactly.
fn glob(pattern: &str, text: &str) -> bool {
    let pieces: Vec<&str> = pattern.split('*').collect();
    let Some(mut rest) = text.strip_prefix(pieces[0]) else {
        return false;
    };
    let last = pieces.len() - 1;
    for (i, piece) in pieces.iter().enumerate().skip(1) {
        if i == last {
            return rest.ends_with(piece);
        }
        match rest.find(piece) {
            Some(at) => rest = &rest[at + piece.len()..],
            None => return false,
        }
    }
    rest.is_empty()
}

/// One path part made safe for a Windows file name.
fn safe_part(part: &str) -> String {
    let decoded = percent_encoding::percent_decode_str(part).decode_utf8_lossy();
    let clean: String = decoded
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    let clean = clean.trim_end_matches(['.', ' ']).to_string();
    if clean.is_empty() || clean == "." || clean == ".." {
        "_".to_string()
    } else {
        clean
    }
}

/// Where `url` is kept under the mirror folder `root`: the host, then its
/// path. A folder address gets `index.html`; a page gets `.html` added when
/// its name does not end in it, so a browser opens it as a page; a query
/// adds a short hash to the name, so `?page=2` does not overwrite `?page=1`.
pub fn mirror_path(root: &Path, url: &Url, page: bool) -> PathBuf {
    let mut path = root.join(safe_part(url.host_str().unwrap_or("site")));
    let segments: Vec<&str> = url.path_segments().map(|s| s.collect()).unwrap_or_default();
    let (dirs, last) = match segments.split_last() {
        Some((last, dirs)) => (dirs, *last),
        None => (&[][..], ""),
    };
    for d in dirs.iter().filter(|d| !d.is_empty()) {
        path.push(safe_part(d));
    }
    let mut name = if last.is_empty() { "index.html".to_string() } else { safe_part(last) };
    if let Some(q) = url.query() {
        let mut hash: u32 = 0x811c_9dc5;
        for b in q.bytes() {
            hash = (hash ^ b as u32).wrapping_mul(0x0100_0193);
        }
        name = match name.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem}_{hash:08x}.{ext}"),
            None => format!("{name}_{hash:08x}"),
        };
    }
    let lower = name.to_ascii_lowercase();
    if page && !lower.ends_with(".html") && !lower.ends_with(".htm") {
        name.push_str(".html");
    }
    path.join(name)
}

/// The path from the file `from` to the file `to`, with `/`, for a link.
fn relative_link(from: &Path, to: &Path) -> String {
    let from: Vec<_> = from.parent().map(|p| p.components().collect()).unwrap_or_default();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_string(); from.len() - common];
    parts.extend(to[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    let link = parts.join("/");
    // Spaces and the like as a browser expects them in a link.
    link.replace('%', "%25").replace(' ', "%20").replace('#', "%23")
}

/// `html`, read from `page` and kept at `here`, with the links that `local`
/// says are kept locally (given the target's address, it answers whether
/// it is a page and so where it is kept) pointing at their copies.
/// Other links stay as they are.
pub fn rewrite_links(html: &str, page: &Url, here: &Path, root: &Path, local: impl Fn(&Url) -> Option<bool>) -> String {
    let doc = Html::parse_document(html);
    let mut replacements: Vec<(String, String)> = Vec::new();
    for (sel, attr) in [("a[href]", "href"), ("area[href]", "href"), ("img[src]", "src"), ("source[src]", "src"), ("iframe[src]", "src"), ("frame[src]", "src"), ("link[href]", "href")] {
        let Ok(s) = Selector::parse(sel) else { continue };
        for e in doc.select(&s) {
            let Some(value) = e.value().attr(attr) else { continue };
            let Ok(mut target) = page.join(value.trim()) else { continue };
            let fragment = target.fragment().map(|f| format!("#{f}")).unwrap_or_default();
            target.set_fragment(None);
            if let Some(is_page) = local(&target) {
                let link = relative_link(here, &mirror_path(root, &target, is_page));
                replacements.push((value.to_string(), format!("{link}{fragment}")));
            }
        }
    }
    let mut out = html.to_string();
    for (from, to) in replacements {
        for quote in ['"', '\''] {
            out = out.replace(&format!("={quote}{from}{quote}"), &format!("={quote}{to}{quote}"));
        }
    }
    out
}

/// The file types a request keeps and skips, as typed: lower case, without
/// dots or a leading `*.`; `*` keeps every type that is not a page.
struct Types {
    keep: HashSet<String>,
    skip: HashSet<String>,
}

impl Types {
    fn of(req: &GrabRequest) -> Types {
        let clean = |list: &[String]| -> HashSet<String> {
            list.iter()
                .map(|t| t.trim().trim_start_matches("*.").trim_start_matches('.').to_ascii_lowercase())
                .filter(|t| !t.is_empty())
                .collect()
        };
        Types {
            keep: clean(&req.types),
            skip: clean(&req.skip_types),
        }
    }

    fn keeps(&self, ext: &str) -> bool {
        !self.skip.contains(ext) && (self.keep.contains(ext) || (self.keep.contains("*") && !PAGE_EXTENSIONS.contains(&ext)))
    }
}

/// Whether a crawl that saves pages keeps `url` locally, and if so whether
/// as a page: pages on the start site, and files of the kept types.
pub fn kept_locally(req: &GrabRequest, url: &Url) -> Option<bool> {
    let ext = url_extension(url);
    if ext.as_ref().is_some_and(|e| Types::of(req).keeps(e)) {
        return Some(false);
    }
    let page_like = ext.is_none_or(|e| PAGE_EXTENSIONS.contains(&e.as_str()));
    (page_like && on_start_site(req, url.as_str())).then_some(true)
}

/// The download for found file `f`: waiting, with the page as referrer,
/// its link text as description when the crawl keeps those, and in the
/// folder the request saves to: under the mirror folder when the crawl
/// keeps one, else one category's folder (from `settings`) or one folder
/// for every file. `None` when the mirror skips existing files and this
/// one is there.
pub fn download_for(req: &GrabRequest, f: &GrabFile, settings: &crate::settings::Settings) -> Option<crate::protocol::Download> {
    let mut d = crate::protocol::Download {
        url: f.url.clone(),
        referrer: Some(f.page.clone()),
        start: Some(false),
        description: f.text.clone().filter(|t| req.link_text && !t.is_empty()),
        ..crate::protocol::Download::default()
    };
    match &req.save_to {
        SaveTo::Categories => {}
        SaveTo::Category(name) => {
            let name = name.trim();
            d.folder = Some(settings.category_folder(name).display().to_string());
            d.category = Some(name.to_string());
        }
        SaveTo::Folder(folder) => d.folder = Some(folder.trim().to_string()),
    }
    if let (Some(m), Ok(url)) = (&req.mirror, Url::parse(&f.url)) {
        let path = mirror_path(Path::new(m.folder.trim()), &url, false);
        if path.exists() && !m.overwrite {
            return None;
        }
        d.filename = path.file_name().map(|n| n.to_string_lossy().into_owned());
        d.folder = path.parent().map(|p| p.display().to_string());
        d.file_exists = Some(if m.overwrite {
            crate::settings::FileExists::Overwrite
        } else {
            crate::settings::FileExists::Rename
        });
    }
    // The project's own login goes to the start site's files.
    if let Some(user) = req.login_user.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        if on_start_site(req, &f.url) {
            d.login = Some(crate::protocol::DownloadLogin {
                username: user.to_string(),
                password: project_password(&req.start, user),
            });
        }
    }
    Some(d)
}

/// Whether a page at `url` belongs to the start site of `req`, which is
/// what browser cookies are sent to: the start host, and with `subdomains`
/// the start domain's other hosts.
pub fn on_start_site(req: &GrabRequest, url: &str) -> bool {
    let host = |u: &str| Url::parse(u).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase));
    let (Some(start), Some(here)) = (host(&req.start), host(url)) else {
        return false;
    };
    let domain = start.strip_prefix("www.").unwrap_or(&start);
    here == start || (req.subdomains && (here == domain || here.ends_with(&format!(".{domain}"))))
}

/// Whether `url` passes an include list (empty passes all) and an exclude
/// list.
fn passes(url: &str, only: &[String], skip: &[String]) -> bool {
    (only.is_empty() || only.iter().any(|p| pattern_matches(p, url))) && !skip.iter().any(|p| pattern_matches(p, url))
}

/// A file the crawl found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrabFile {
    pub url: String,
    /// Its file name, from the address.
    pub name: String,
    /// The page that links to it.
    pub page: String,
    /// Its size, when it was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// The text of the link to it, when the crawl keeps link texts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// A fetched page.
pub struct Page {
    /// The address after redirects; relative links resolve against it.
    pub url: String,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// The rules `robots.txt` sets for the crawler.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Robots {
    /// `(allow, path prefix)`; the longest matching prefix wins, `allow` on a
    /// tie.
    rules: Vec<(bool, String)>,
    pub crawl_delay: Option<Duration>,
}

impl Robots {
    /// Read `robots.txt`, keeping the group for `agent` if it has one, or
    /// else the `*` group.
    pub fn parse(text: &str, agent: &str) -> Robots {
        let agent = agent.to_ascii_lowercase();
        // Groups: the agents named, then their rules.
        let mut groups: Vec<(Vec<String>, Robots)> = Vec::new();
        let mut in_agents = false;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once(':') else { continue };
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            match key.as_str() {
                "user-agent" => {
                    if !in_agents {
                        groups.push((Vec::new(), Robots::default()));
                    }
                    in_agents = true;
                    if let Some(g) = groups.last_mut() {
                        g.0.push(value.to_ascii_lowercase());
                    }
                }
                "allow" | "disallow" => {
                    in_agents = false;
                    if let Some(g) = groups.last_mut() {
                        // An empty Disallow allows everything.
                        if !value.is_empty() {
                            g.1.rules.push((key == "allow", value.to_string()));
                        }
                    }
                }
                "crawl-delay" => {
                    in_agents = false;
                    if let (Some(g), Ok(secs)) = (groups.last_mut(), value.parse::<f64>()) {
                        if secs.is_finite() && secs >= 0.0 {
                            g.1.crawl_delay = Some(Duration::from_secs_f64(secs).min(MAX_CRAWL_DELAY));
                        }
                    }
                }
                _ => {}
            }
        }
        let named = groups.iter().find(|(agents, _)| agents.iter().any(|a| a != "*" && agent.contains(a.as_str())));
        let any = groups.iter().find(|(agents, _)| agents.iter().any(|a| a == "*"));
        named.or(any).map(|(_, r)| r.clone()).unwrap_or_default()
    }

    /// Whether the path (with its query) of `url` may be crawled.
    pub fn allows(&self, url: &Url) -> bool {
        let mut path = url.path().to_string();
        if let Some(q) = url.query() {
            path.push('?');
            path.push_str(q);
        }
        let mut best: Option<(usize, bool)> = None;
        for (allow, prefix) in &self.rules {
            if robots_match(prefix, &path) {
                let len = prefix.len();
                if best.is_none_or(|(l, a)| len > l || (len == l && *allow && !a)) {
                    best = Some((len, *allow));
                }
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }
}

/// A robots.txt path pattern: a prefix, with `*` for any run of characters
/// and `$` for the end.
fn robots_match(pattern: &str, path: &str) -> bool {
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    let pieces: Vec<&str> = pattern.split('*').collect();
    let Some(mut rest) = path.strip_prefix(pieces[0]) else {
        return false;
    };
    for piece in &pieces[1..] {
        match rest.find(piece) {
            Some(at) => rest = &rest[at + piece.len()..],
            None => return false,
        }
    }
    match (anchored, pieces.len()) {
        (false, _) => true,
        (true, 1) => rest.is_empty(),
        (true, _) => path.ends_with(pieces[pieces.len() - 1]),
    }
}

/// The lower-case extension of a URL's last path segment.
fn url_extension(url: &Url) -> Option<String> {
    let last = url.path_segments()?.next_back()?;
    let (_, ext) = last.rsplit_once('.')?;
    (!ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// Extensions of pages worth reading for more links; a link with no
/// extension (`/docs/`) is read too.
const PAGE_EXTENSIONS: &[&str] = &["html", "htm", "xhtml", "php", "asp", "aspx", "jsp", "shtml", "cgi"];

/// The links on a page, resolved against it (or its `<base href>`): those
/// that may be pages, and those that may be files with their link text
/// (the element's text, else its `alt` or `title`).
pub fn links(html: &str, page: &Url) -> (Vec<Url>, Vec<(Url, String)>) {
    let doc = Html::parse_document(html);
    let base = Selector::parse("base[href]")
        .ok()
        .and_then(|s| doc.select(&s).next().and_then(|b| b.value().attr("href")).and_then(|h| page.join(h).ok()))
        .filter(|b| matches!(b.scheme(), "http" | "https"))
        .unwrap_or_else(|| page.clone());
    let pick = |selector: &str, attr: &str| -> Vec<(Url, String)> {
        let Ok(sel) = Selector::parse(selector) else { return Vec::new() };
        doc.select(&sel)
            .filter_map(|e| {
                let mut u = base.join(e.value().attr(attr)?.trim()).ok()?;
                u.set_fragment(None);
                let text: String = e.text().collect::<Vec<_>>().join(" ");
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                let text = if text.is_empty() {
                    e.value().attr("alt").or(e.value().attr("title")).unwrap_or("").trim().to_string()
                } else {
                    text
                };
                Some((u, text.chars().take(200).collect()))
            })
            .filter(|(u, _)| matches!(u.scheme(), "http" | "https"))
            .collect()
    };
    let mut files = pick("a[href]", "href");
    files.extend(pick("area[href]", "href"));
    files.extend(pick("iframe[src], frame[src]", "src"));
    let pages = files.iter().map(|(u, _)| u.clone()).collect();
    for (sel, attr) in [
        ("img[src]", "src"),
        ("source[src]", "src"),
        ("video[src]", "src"),
        ("audio[src]", "src"),
        ("embed[src]", "src"),
        ("object[data]", "data"),
    ] {
        files.extend(pick(sel, attr));
    }
    (pages, files)
}

/// A saved grabber project: its settings, and what its last crawl found
/// with the boxes ticked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub request: GrabRequest,
    #[serde(default)]
    pub files: Vec<GrabFile>,
    /// Ticked files, one flag per entry of `files`.
    #[serde(default)]
    pub checked: Vec<bool>,
    /// When the daemon runs it by itself: once, or on chosen days, with an
    /// optional stop time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<crate::queue::Schedule>,
    /// What a scheduled run does.
    #[serde(default)]
    pub run: RunAction,
    /// Files from older project files said `ExploreAndDownload` this way;
    /// `Project::load` moves it into `run`.
    #[serde(default, skip_serializing)]
    pub download_new: bool,
    /// The queue a scheduled run downloads in.
    #[serde(default = "main_queue")]
    pub queue: u64,
    /// What happens once a scheduled run is done: after the crawl, or once
    /// the queue has downloaded what the run added.
    #[serde(default, skip_serializing_if = "is_nothing")]
    pub when_done: crate::queue::WhenDone,
    /// A file to open once a scheduled run is done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_when_done: Option<String>,
    /// Every file address earlier runs found, so a run adds only new ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seen: Vec<String>,
    /// The day of the scheduled run last started, so it starts only once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_scheduled_day: Option<i64>,
    /// When the schedule last started it, in minutes since 1970 local time,
    /// for starting it again every so often.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_scheduled_start: Option<i64>,
}

fn main_queue() -> u64 {
    crate::queue::MAIN_QUEUE
}

fn is_nothing(w: &crate::queue::WhenDone) -> bool {
    *w == crate::queue::WhenDone::Nothing
}

/// What a project's scheduled run does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunAction {
    /// Crawl and keep what it found, for a look later.
    #[default]
    Explore,
    /// Crawl, add the files no earlier run found to the queue, and start it.
    ExploreAndDownload,
    /// Do not crawl: add the files ticked in the project to the queue and
    /// start it.
    DownloadChecked,
}

impl RunAction {
    pub const ALL: [RunAction; 3] = [RunAction::Explore, RunAction::ExploreAndDownload, RunAction::DownloadChecked];

    pub fn label(self) -> &'static str {
        match self {
            RunAction::Explore => "Explore only",
            RunAction::ExploreAndDownload => "Explore, then download the new files",
            RunAction::DownloadChecked => "Download the ticked files, without exploring",
        }
    }
}

/// `%APPDATA%\UIDM\grabber`, next to the settings file.
pub fn projects_dir() -> io::Result<PathBuf> {
    let settings = crate::settings::settings_path()?;
    Ok(settings.with_file_name("grabber"))
}

const TEMPLATE_FILE: &str = "template.json";

/// The file a project named `name` is kept in.
fn project_file(dir: &Path, name: &str) -> PathBuf {
    let safe: String = name
        .trim()
        .chars()
        .map(|c| if c.is_alphanumeric() || " -_.()".contains(c) { c } else { '_' })
        .collect();
    dir.join(format!("{}.project.json", safe.trim_matches('.')))
}

impl Project {
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        if self.name.trim().is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "A project needs a name."));
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        crate::settings::write_atomic(&project_file(dir, &self.name), &bytes)
    }

    /// Take a run's findings as the project's list, and return those no
    /// earlier run found.
    pub fn record_run(&mut self, found: Vec<GrabFile>) -> Vec<GrabFile> {
        let seen: HashSet<String> = self.seen.iter().cloned().collect();
        let new: Vec<GrabFile> = found.iter().filter(|f| !seen.contains(&f.url)).cloned().collect();
        self.seen.extend(new.iter().map(|f| f.url.clone()));
        self.checked = found.iter().map(|f| !seen.contains(&f.url)).collect();
        self.files = found;
        new
    }

    pub fn load(dir: &Path, name: &str) -> io::Result<Project> {
        let bytes = fs::read(project_file(dir, name))?;
        let mut p: Project = serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if std::mem::take(&mut p.download_new) && p.run == RunAction::Explore {
            p.run = RunAction::ExploreAndDownload;
        }
        Ok(p)
    }

    /// The files ticked in the project.
    pub fn checked_files(&self) -> Vec<GrabFile> {
        self.files.iter().zip(&self.checked).filter(|(_, c)| **c).map(|(f, _)| f.clone()).collect()
    }
}

/// The saved projects' names, most recently saved first.
pub fn recent_projects(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(SystemTime, String)> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".project.json"))
        .filter_map(|e| {
            let modified = e.metadata().and_then(|m| m.modified()).ok()?;
            let project: Project = serde_json::from_slice(&fs::read(e.path()).ok()?).ok()?;
            Some((modified, project.name))
        })
        .collect();
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    found.into_iter().map(|(_, n)| n).collect()
}

/// The settings new projects start from: the saved template, or the
/// defaults.
pub fn template(dir: &Path) -> GrabRequest {
    fs::read(dir.join(TEMPLATE_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<GrabRequest>(&b).ok())
        .map(|r| GrabRequest { start: String::new(), ..r })
        .unwrap_or_default()
}

pub fn save_template(dir: &Path, req: &GrabRequest) -> io::Result<()> {
    let req = GrabRequest {
        start: String::new(),
        ..req.clone()
    };
    let bytes = serde_json::to_vec_pretty(&req).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    crate::settings::write_atomic(&dir.join(TEMPLATE_FILE), &bytes)
}

/// A named template: the settings a new project can start from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Template {
    pub name: String,
    pub request: GrabRequest,
    /// Shipped with UIDM, so it cannot be deleted.
    #[serde(skip)]
    pub built_in: bool,
}

/// The templates UIDM ships, files of one kind from a site: each one's
/// name, file types and depth.
pub const BUILT_IN_TEMPLATES: [(&str, &[&str], u32); 5] = [
    ("Pictures from a site", &["jpg", "jpeg", "png", "gif", "webp", "bmp"], 2),
    ("Audio files from a site", &["mp3", "m4a", "ogg", "flac", "wav", "aac", "opus"], 2),
    ("Video files from a site", &["mp4", "mkv", "webm", "mov", "avi", "wmv"], 2),
    ("Documents from a site", &["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "txt"], 3),
    ("Archives and programs from a site", &["zip", "rar", "7z", "gz", "exe", "msi"], 2),
];

/// `BUILT_IN_TEMPLATES` as templates.
pub fn built_in_templates() -> Vec<Template> {
    BUILT_IN_TEMPLATES
        .iter()
        .map(|(name, types, depth)| Template {
            name: name.to_string(),
            request: GrabRequest {
                types: types.iter().map(|t| t.to_string()).collect(),
                depth: *depth,
                ..GrabRequest::default()
            },
            built_in: true,
        })
        .collect()
}

/// A named filter for the files a crawl keeps: a name and a mask such as
/// `*.jpg;*.png`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    pub name: String,
    pub mask: String,
}

impl Filter {
    /// The file types the mask names, as `GrabRequest::types` takes them;
    /// `*` or `*.*` keeps every type.
    pub fn types(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for part in self.mask.split([';', ',', ' ']).map(str::trim).filter(|p| !p.is_empty()) {
            let t = part.trim_start_matches('*').trim_start_matches('.').to_ascii_lowercase();
            let t = if t.is_empty() || t == "*" { "*".to_string() } else { t };
            if !out.contains(&t) {
                out.push(t);
            }
        }
        out
    }
}

/// The filters a new installation starts with.
pub fn default_filters() -> Vec<Filter> {
    let f = |name: &str, mask: &str| Filter {
        name: name.to_string(),
        mask: mask.to_string(),
    };
    vec![
        f("All files", "*.*"),
        f("Documents", "*.pdf;*.doc;*.docx;*.xls;*.xlsx;*.ppt;*.pptx;*.odt;*.txt"),
        f("Pictures", "*.jpg;*.jpeg;*.png;*.gif;*.webp;*.bmp"),
        f("Audio", "*.mp3;*.m4a;*.ogg;*.flac;*.wav;*.aac"),
        f("Video", "*.mp4;*.mkv;*.webm;*.mov;*.avi;*.wmv"),
        f("Archives", "*.zip;*.rar;*.7z;*.gz;*.tar"),
        f("Programs", "*.exe;*.msi;*.apk"),
    ]
}

const FILTERS_FILE: &str = "filters.json";

/// The saved filters, or the defaults before any were saved.
pub fn load_filters(dir: &Path) -> Vec<Filter> {
    fs::read(dir.join(FILTERS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(default_filters)
}

pub fn save_filters(dir: &Path, filters: &[Filter]) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(filters).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    crate::settings::write_atomic(&dir.join(FILTERS_FILE), &bytes)
}

/// The folder the named templates are kept in.
fn templates_dir(dir: &Path) -> PathBuf {
    dir.join("templates")
}

/// The built-in templates, then the user's by name.
pub fn templates(dir: &Path) -> Vec<Template> {
    let mut saved: Vec<Template> = fs::read_dir(templates_dir(dir))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
                .filter_map(|e| serde_json::from_slice::<Template>(&fs::read(e.path()).ok()?).ok())
                .collect()
        })
        .unwrap_or_default();
    saved.sort_by_key(|t| t.name.to_lowercase());
    let mut all = built_in_templates();
    all.extend(saved);
    all
}

/// The file the template named `name` is kept in.
fn template_file(dir: &Path, name: &str) -> PathBuf {
    let file = project_file(&templates_dir(dir), name);
    file.with_file_name(file.file_name().unwrap_or_default().to_string_lossy().replace(".project.json", ".json"))
}

/// Save `req` as the template named `name`, without its start page.
pub fn save_named_template(dir: &Path, name: &str, req: &GrabRequest) -> io::Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "A template needs a name."));
    }
    if built_in_templates().iter().any(|t| t.name.eq_ignore_ascii_case(name)) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "A template that comes with UIDM has that name."));
    }
    let t = Template {
        name: name.to_string(),
        request: GrabRequest {
            start: String::new(),
            login_user: None,
            ..req.clone()
        },
        built_in: false,
    };
    let bytes = serde_json::to_vec_pretty(&t).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    crate::settings::write_atomic(&template_file(dir, name), &bytes)
}

/// Delete the user's template named `name`.
pub fn delete_template(dir: &Path, name: &str) -> io::Result<()> {
    fs::remove_file(template_file(dir, name))
}

/// How a crawl went, so far or in the end.
#[derive(Debug, Default)]
pub struct Outcome {
    /// The files kept, in the order they were found.
    pub files: Vec<GrabFile>,
    /// Pages read.
    pub pages: u64,
    /// Bytes of pages read.
    pub bytes: u64,
    /// Links to files looked at, kept or not.
    pub explored: u64,
    /// Why it stopped before reading everything in reach, if it did.
    pub stopped: Option<String>,
}

/// A page waiting to be read: its address, how many links from the start
/// page it is, and how many of those in a row were on other sites.
struct Queued {
    url: Url,
    depth: u32,
    off_site: u32,
}

/// Crawl from `req.start`. `fetch` reads a page (at most `MAX_PAGE_BYTES`),
/// up to `req.explore_at_once` pages at a time on their own threads; `size`
/// asks a file's size, for the size filters and duplicates; `wait` sleeps
/// between requests (tests pass a no-op); `progress` hears how the crawl
/// goes after each page. `cancel` stops the crawl between requests.
pub fn crawl(
    req: &GrabRequest,
    fetch: impl Fn(&str) -> Result<Page, String> + Sync,
    mut size: impl FnMut(&str) -> Option<u64>,
    mut wait: impl FnMut(Duration),
    cancel: &AtomicBool,
    mut progress: impl FnMut(&Outcome),
) -> Outcome {
    let mut out = Outcome::default();
    let Ok(mut start) = Url::parse(req.start.trim()) else {
        out.stopped = Some("The start page is not a web address.".into());
        return out;
    };
    start.set_fragment(None);
    let start_host = start.host_str().unwrap_or("").to_ascii_lowercase();
    // The domain whose subdomains count as the same site: the start host
    // without a leading www.
    let domain = start_host.strip_prefix("www.").unwrap_or(&start_host).to_string();
    let on_site = |u: &Url| {
        let host = u.host_str().unwrap_or("").to_ascii_lowercase();
        host == start_host || (req.subdomains && (host == domain || host.ends_with(&format!(".{domain}"))))
    };
    // The folder the crawl may not go above.
    let start_dir = start.path().rsplit_once('/').map_or("/".to_string(), |(dir, _)| format!("{dir}/"));
    // The files kept so far, by name (as indexes into `out.files`), to spot
    // duplicates, and those whose size was asked.
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    let mut asked: HashSet<usize> = HashSet::new();
    let types = Types::of(req);
    let delay = Duration::from_millis(req.delay_ms);
    let at_once = req.explore_at_once.clamp(AT_ONCE_RANGE.0, AT_ONCE_RANGE.1) as usize;

    let mut robots: HashMap<String, Robots> = HashMap::new();
    let mut queue: VecDeque<Queued> = VecDeque::from([Queued {
        url: start.clone(),
        depth: 0,
        off_site: 0,
    }]);
    let mut seen_pages: HashSet<String> = HashSet::from([start.to_string()]);
    let mut seen_files: HashSet<String> = HashSet::new();
    let mut first = true;
    let mut skipped_by_robots = 0u64;

    'crawl: while !queue.is_empty() {
        if cancel.load(Ordering::Relaxed) {
            out.stopped = Some("Stopped.".into());
            break;
        }
        if out.pages >= req.max_pages {
            out.stopped = Some(format!("Stopped at the budget of {} pages.", req.max_pages));
            break;
        }
        if out.bytes >= req.max_bytes {
            out.stopped = Some(format!("Stopped at the budget of {} MB.", req.max_bytes / (1024 * 1024)));
            break;
        }
        // The next pages robots.txt lets the crawl read, as many as are
        // read at once, within the page budget.
        let room = (req.max_pages - out.pages) as usize;
        let mut batch: Vec<Queued> = Vec::new();
        let mut pause = delay;
        while batch.len() < at_once.min(room) {
            let Some(next) = queue.pop_front() else { break };
            let origin = next.url.origin().ascii_serialization();
            if !robots.contains_key(&origin) {
                if !first {
                    wait(delay);
                }
                first = false;
                if cancel.load(Ordering::Relaxed) {
                    out.stopped = Some("Stopped.".into());
                    break 'crawl;
                }
                let rules = match fetch(&format!("{origin}/robots.txt")) {
                    Ok(page) => Robots::parse(&String::from_utf8_lossy(&page.body), USER_AGENT),
                    Err(_) => Robots::default(),
                };
                robots.insert(origin.clone(), rules);
            }
            let rules = &robots[&origin];
            if !rules.allows(&next.url) {
                skipped_by_robots += 1;
                continue;
            }
            pause = rules.crawl_delay.map_or(pause, |d| d.max(pause));
            batch.push(next);
        }
        if batch.is_empty() {
            continue;
        }
        if !first {
            wait(pause);
        }
        first = false;
        if cancel.load(Ordering::Relaxed) {
            out.stopped = Some("Stopped.".into());
            break;
        }
        let fetched: Vec<Result<Page, String>> = if batch.len() == 1 {
            vec![fetch(batch[0].url.as_str())]
        } else {
            std::thread::scope(|s| {
                let fetch = &fetch;
                let reading: Vec<_> = batch.iter().map(|q| s.spawn(move || fetch(q.url.as_str()))).collect();
                reading
                    .into_iter()
                    .map(|r| r.join().unwrap_or_else(|_| Err("the page could not be read".into())))
                    .collect()
            })
        };

        for (Queued { url, depth, off_site }, page) in batch.into_iter().zip(fetched) {
            let Ok(page) = page else { continue };
            out.pages += 1;
            out.bytes += page.body.len() as u64;
            let html = page
                .content_type
                .as_deref()
                .is_none_or(|t| t.to_ascii_lowercase().contains("html"));
            if html {
                let here = Url::parse(&page.url).unwrap_or(url.clone());
                let text = String::from_utf8_lossy(&page.body);
                let (pages, files) = links(&text, &here);
                for (file, link_text) in files {
                    let Some(ext) = url_extension(&file) else { continue };
                    if PAGE_EXTENSIONS.contains(&ext.as_str()) && !types.keep.contains(&ext) {
                        continue;
                    }
                    if !seen_files.insert(file.to_string()) {
                        continue;
                    }
                    out.explored += 1;
                    if !types.keeps(&ext)
                        || !passes(file.as_str(), &req.files_only, &req.files_skip)
                        || (req.files_this_site && !on_site(&file))
                    {
                        continue;
                    }
                    let name = file
                        .path_segments()
                        .and_then(|mut s| s.next_back())
                        .map(|n| percent_encoding::percent_decode_str(n).decode_utf8_lossy().into_owned())
                        .unwrap_or_default();
                    // Ask the size only when a filter needs it, or to compare
                    // with files of the same name kept earlier (whose sizes
                    // are asked then too).
                    let twins = by_name.get(&name).cloned().unwrap_or_default();
                    let compare = req.hide_duplicates && !twins.is_empty();
                    let need = req.min_size.is_some() || req.max_size.is_some() || compare;
                    if need && cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    if compare {
                        for &i in &twins {
                            if out.files[i].size.is_none() && asked.insert(i) {
                                wait(delay);
                                out.files[i].size = size(&out.files[i].url);
                            }
                        }
                    }
                    let known = if need {
                        wait(delay);
                        size(file.as_str())
                    } else {
                        None
                    };
                    if let Some(n) = known {
                        if req.min_size.is_some_and(|lo| n < lo) || req.max_size.is_some_and(|hi| n > hi) {
                            continue;
                        }
                        if compare && twins.iter().any(|&i| out.files[i].size == Some(n)) {
                            continue;
                        }
                    }
                    let at = out.files.len();
                    if need {
                        asked.insert(at);
                    }
                    by_name.entry(name.clone()).or_default().push(at);
                    out.files.push(GrabFile {
                        url: file.to_string(),
                        name,
                        page: here.to_string(),
                        size: known,
                        text: (req.link_text && !link_text.is_empty()).then_some(link_text),
                    });
                }
                for next in pages {
                    let here_site = on_site(&next);
                    let exact = next.host_str().unwrap_or("").eq_ignore_ascii_case(&start_host);
                    // How far it may go: on the start site, `depth` links (or
                    // any number for the whole site); elsewhere, `other_depth`
                    // links in a row, or `depth` in all without it.
                    let in_reach = if here_site {
                        req.whole_site || depth < req.depth
                    } else if req.same_site {
                        false
                    } else {
                        match req.other_depth {
                            Some(n) => off_site < n,
                            None => depth < req.depth,
                        }
                    };
                    let below = !req.no_parent || !exact || next.path().starts_with(&start_dir);
                    let page_like = url_extension(&next).is_none_or(|e| PAGE_EXTENSIONS.contains(&e.as_str()));
                    let wanted = passes(next.as_str(), &req.explore_only, &req.explore_skip);
                    if in_reach && below && page_like && wanted && seen_pages.insert(next.to_string()) {
                        queue.push_back(Queued {
                            url: next,
                            depth: depth.saturating_add(1),
                            off_site: if here_site { 0 } else { off_site + 1 },
                        });
                    }
                }
            }
            progress(&out);
        }
    }
    if out.stopped.is_none() && skipped_by_robots > 0 {
        let pages = match skipped_by_robots {
            1 => "1 page was".to_string(),
            n => format!("{n} pages were"),
        };
        out.stopped = Some(format!("{pages} skipped because robots.txt asks crawlers to stay out."));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn robots_rules() {
        let text = "
# comment
User-agent: Googlebot
Disallow: /

User-agent: *
Disallow: /private/
Allow: /private/open.html
Disallow: /*.cgi$
Disallow: /tmp*/x
Crawl-delay: 2.5
";
        let r = Robots::parse(text, USER_AGENT);
        let ok = |p: &str| r.allows(&Url::parse(&format!("https://s{p}")).unwrap());
        assert!(ok("/docs/a.pdf"));
        assert!(!ok("/private/secret.html"));
        assert!(ok("/private/open.html"));
        assert!(!ok("/run.cgi"));
        assert!(ok("/run.cgi?x=1"));
        assert!(!ok("/tmp1/x/y"));
        assert_eq!(r.crawl_delay, Some(Duration::from_millis(2500)));

        let named = Robots::parse("User-agent: UIDM\nDisallow: /\n\nUser-agent: *\nDisallow:\n", USER_AGENT);
        assert!(!named.allows(&Url::parse("https://s/a").unwrap()));
        let empty = Robots::parse("User-agent: *\nDisallow:\n", USER_AGENT);
        assert!(empty.allows(&Url::parse("https://s/a").unwrap()));
        let huge = Robots::parse("User-agent: *\nCrawl-delay: 99999\n", USER_AGENT);
        assert_eq!(huge.crawl_delay, Some(MAX_CRAWL_DELAY));
    }

    #[test]
    fn links_are_resolved_and_only_web() {
        let page = Url::parse("https://s/docs/index.html").unwrap();
        let html = r#"<html><head><title>x</title></head><body>
            <a href="guide.pdf">Guide</a>
            <a href="/files/b.PDF#page=2">B</a>
            <a href="javascript:alert(1)">no</a>
            <a href="mailto:a@b">no</a>
            <a href="data:text/html,x">no</a>
            <a href="sub/">Sub</a>
            <img src="//cdn.s/pic.png">
            <script>var a = '<a href="hidden.pdf">';</script>
            <a href = "  spaced.pdf  ">
        </body></html>"#;
        let (pages, files) = links(html, &page);
        let pages: Vec<String> = pages.iter().map(Url::to_string).collect();
        assert!(pages.contains(&"https://s/docs/guide.pdf".to_string()));
        assert!(pages.contains(&"https://s/files/b.PDF".to_string()));
        assert!(pages.contains(&"https://s/docs/sub/".to_string()));
        assert!(pages.contains(&"https://s/docs/spaced.pdf".to_string()));
        assert!(!pages.iter().any(|p| p.contains("hidden")), "script text is not markup");
        assert!(pages.iter().all(|p| p.starts_with("https://")));
        assert!(files.iter().any(|(f, _)| f.as_str() == "https://cdn.s/pic.png"));
        let text = |url: &str| files.iter().find(|(f, _)| f.as_str() == url).map(|(_, t)| t.as_str());
        assert_eq!(text("https://s/docs/guide.pdf"), Some("Guide"));
        let titled = links(r#"<a href="a.pdf" title="The A file"></a><img src="p.png" alt="A picture">"#, &page).1;
        assert_eq!(titled[0].1, "The A file");
        assert_eq!(titled[1].1, "A picture");

        let based = links(r#"<base href="https://other/x/"><a href="y.pdf">"#, &page).0;
        assert_eq!(based[0].as_str(), "https://other/x/y.pdf");
    }

    /// A small site: `/` links to two sections, each to files and deeper
    /// pages.
    fn site(url: &str) -> Result<Page, String> {
        let html = |body: &str| {
            Ok(Page {
                url: url.to_string(),
                content_type: Some("text/html; charset=utf-8".into()),
                body: body.as_bytes().to_vec(),
            })
        };
        match url {
            "https://s/robots.txt" => Ok(Page {
                url: url.into(),
                content_type: Some("text/plain".into()),
                body: b"User-agent: *\nDisallow: /secret/\n".to_vec(),
            }),
            "https://s/" => html(r#"<a href="/a/">A</a><a href="/b/">B</a><a href="/secret/">S</a><a href="https://elsewhere/">E</a><a href="/top.pdf">t</a>"#),
            "https://s/a/" => html(r#"<a href="one.pdf">1</a><a href="one.zip">z</a><a href="deep/">d</a><a href="/">home</a>"#),
            "https://s/b/" => html(r#"<a href="two.pdf">2</a><a href="https://cdn.s/three%20x.pdf">3</a>"#),
            "https://s/a/deep/" => html(r#"<a href="four.pdf">4</a>"#),
            "https://s/secret/" => html(r#"<a href="hidden.pdf">h</a>"#),
            "https://elsewhere/" => html(r#"<a href="far.pdf">f</a>"#),
            _ => Err("404".into()),
        }
    }

    fn request(depth: u32) -> GrabRequest {
        GrabRequest {
            start: "https://s/".into(),
            depth,
            ..GrabRequest::default()
        }
    }

    #[test]
    fn crawls_within_depth_robots_and_site() {
        let fetched = std::sync::Mutex::new(Vec::new());
        let waits = RefCell::new(0);
        let out = crawl(
            &request(1),
            |u| {
                fetched.lock().unwrap().push(u.to_string());
                site(u)
            },
            |_| None,
            |_| *waits.borrow_mut() += 1,
            &AtomicBool::new(false),
            |_| {},
        );
        let names: Vec<&str> = out.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["top.pdf", "one.pdf", "two.pdf", "three x.pdf"]);
        assert_eq!(out.files[1].page, "https://s/a/");
        assert_eq!(out.pages, 3, "/, /a/, /b/");
        let fetched = fetched.into_inner().unwrap();
        assert!(!fetched.iter().any(|u| u.contains("secret")), "robots.txt is obeyed");
        assert!(!fetched.iter().any(|u| u.contains("elsewhere")), "same site only");
        assert!(!fetched.iter().any(|u| u.contains("deep")), "depth 1");
        assert!(!fetched.iter().any(|u| u.ends_with(".pdf")), "files are listed, not fetched");
        assert_eq!(fetched.iter().filter(|u| u.ends_with("robots.txt")).count(), 1);
        assert_eq!(*waits.borrow(), fetched.len() - 1, "a wait before every request but the first");
        assert!(out.stopped.unwrap().contains("robots.txt"));

        let deeper = crawl(&request(2), site, |_| None, |_| {}, &AtomicBool::new(false), |_| {});
        assert!(deeper.files.iter().any(|f| f.name == "four.pdf"));
        let anywhere = crawl(
            &GrabRequest { same_site: false, ..request(1) },
            site,
            |_| None,
            |_| {},
            &AtomicBool::new(false),
            |_| {},
        );
        assert!(anywhere.files.iter().any(|f| f.name == "far.pdf"));
    }

    #[test]
    fn budgets_and_cancel_stop_a_crawl() {
        let out = crawl(
            &GrabRequest { max_pages: 2, ..request(3) },
            site,
            |_| None,
            |_| {},
            &AtomicBool::new(false),
            |_| {},
        );
        assert_eq!(out.pages, 2);
        assert!(out.stopped.unwrap().contains("2 pages"));

        let cancel = AtomicBool::new(false);
        let out = crawl(&request(3), site, |_| None, |_| {}, &cancel, |o| {
            if o.pages == 1 {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert_eq!(out.pages, 1);
        assert_eq!(out.stopped.as_deref(), Some("Stopped."));
    }

    #[test]
    fn pages_can_be_read_several_at_once() {
        let none = AtomicBool::new(false);
        let one = crawl(&request(2), site, |_| None, |_| {}, &none, |_| {});
        let waits = RefCell::new(0);
        let many = crawl(
            &GrabRequest { explore_at_once: 3, ..request(2) },
            site,
            |_| None,
            |_| *waits.borrow_mut() += 1,
            &none,
            |_| {},
        );
        let names = |o: &Outcome| {
            let mut n: Vec<String> = o.files.iter().map(|f| f.name.clone()).collect();
            n.sort();
            n
        };
        assert_eq!(names(&one), names(&many));
        assert_eq!(one.pages, many.pages);
        // robots.txt, then /, then /a/ and /b/ together, then /a/deep/.
        assert_eq!(*waits.borrow(), 3, "one wait per batch of pages");
        let budget = crawl(&GrabRequest { explore_at_once: 5, max_pages: 2, ..request(2) }, site, |_| None, |_| {}, &none, |_| {});
        assert_eq!(budget.pages, 2, "a batch stays within the page budget");
    }

    #[test]
    fn types_skipped_types_link_texts_and_this_site_only() {
        let none = AtomicBool::new(false);
        let every = GrabRequest { types: vec!["*".into()], skip_types: vec!["zip".into()], link_text: true, ..request(1) };
        let out = crawl(&every, site, |_| None, |_| {}, &none, |_| {});
        let names: Vec<&str> = out.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["top.pdf", "one.pdf", "two.pdf", "three x.pdf"], "every type but zip, and no pages");
        assert_eq!(out.files[1].text.as_deref(), Some("1"));
        assert!(out.explored >= 5, "the zip was looked at: {}", out.explored);
        let here = GrabRequest { files_this_site: true, ..request(1) };
        let out = crawl(&here, site, |_| None, |_| {}, &none, |_| {});
        assert!(!out.files.iter().any(|f| f.url.contains("cdn.s")), "cdn.s is another site");
        assert!(out.files.iter().all(|f| f.text.is_none()), "no texts unless asked");
    }

    #[test]
    fn the_whole_site_and_levels_on_other_sites() {
        let none = AtomicBool::new(false);
        let whole = crawl(&GrabRequest { whole_site: true, ..request(0) }, site, |_| None, |_| {}, &none, |_| {});
        assert!(whole.files.iter().any(|f| f.name == "four.pdf"), "depth 0, but the whole site");
        assert!(!whole.files.iter().any(|f| f.name == "far.pdf"), "and only the site");
        let other = GrabRequest { same_site: false, other_depth: Some(1), ..request(0) };
        let out = crawl(&other, site, |_| None, |_| {}, &none, |_| {});
        assert!(out.files.iter().any(|f| f.name == "far.pdf"), "one level on another site");
        assert!(!out.files.iter().any(|f| f.name == "one.pdf"), "depth 0 on the start site");
        let none_other = GrabRequest { same_site: false, other_depth: Some(0), ..request(1) };
        let out = crawl(&none_other, site, |_| None, |_| {}, &none, |_| {});
        assert!(!out.files.iter().any(|f| f.name == "far.pdf"));
    }

    #[test]
    fn found_files_go_to_one_category_or_one_folder() {
        let file = GrabFile {
            url: "https://s/a/one.pdf".into(),
            name: "one.pdf".into(),
            page: "https://s/a/".into(),
            size: None,
            text: Some("The manual".into()),
        };
        let settings = crate::settings::Settings {
            download_dir: Some(std::env::temp_dir().join("uidm-grab-dl").display().to_string()),
            ..crate::settings::Settings::default()
        };
        let d = download_for(&request(1), &file, &settings).unwrap();
        assert_eq!((d.category, d.folder, d.description), (None, None, None));
        let req = GrabRequest { save_to: SaveTo::Category("Music".into()), link_text: true, ..request(1) };
        let d = download_for(&req, &file, &settings).unwrap();
        assert_eq!((d.category.as_deref(), d.description.as_deref()), (Some("Music"), Some("The manual")));
        let music = settings.category_folder("Music").display().to_string();
        assert_eq!(d.folder.as_deref(), Some(music.as_str()), "a pdf saved to Music goes to Music's folder");
        let req = GrabRequest { save_to: SaveTo::Folder(r"D:\grabbed".into()), ..request(1) };
        assert_eq!(download_for(&req, &file, &settings).unwrap().folder.as_deref(), Some(r"D:\grabbed"));
        assert!(GrabRequest { save_to: SaveTo::Folder("rel".into()), ..request(1) }.validate().is_err());
        assert!(GrabRequest { save_to: SaveTo::Category(" ".into()), ..request(1) }.validate().is_err());
        assert!(GrabRequest { explore_at_once: 11, ..request(1) }.validate().is_err());
        let json = serde_json::to_string(&SaveTo::Folder("D:\\x".into())).unwrap();
        assert_eq!(json, r#"{"to":"folder","name":"D:\\x"}"#);
        let old: GrabRequest = serde_json::from_str(r#"{"start":"https://s/","depth":1,"same_site":true,"types":["pdf"],"max_pages":5,"max_bytes":1048576,"delay_ms":1000}"#).unwrap();
        assert_eq!((old.explore_at_once, old.download_at_once, old.ignore_popups, old.save_to), (1, 3, true, SaveTo::Categories));
    }

    #[test]
    fn patterns_match_addresses() {
        assert!(pattern_matches("*/docs/*", "https://s/docs/a.pdf"));
        assert!(pattern_matches("/docs/", "https://s/docs/a.pdf"), "no * matches anywhere");
        assert!(!pattern_matches("*/docs/*", "https://s/blog/a.pdf"));
        assert!(pattern_matches("s/*.PDF", "https://s/x/a.pdf"), "no scheme, any case");
        assert!(pattern_matches("https://s/*", "https://s/a"));
        assert!(!pattern_matches("http://s/*", "https://s/a"));
        assert!(!pattern_matches("", "https://s/a"));
    }

    #[test]
    fn filters_narrow_what_is_explored_and_kept() {
        let none = AtomicBool::new(false);
        let skip_b = GrabRequest { explore_skip: vec!["*/b/*".into()], ..request(1) };
        let out = crawl(&skip_b, site, |_| None, |_| {}, &none, |_| {});
        assert!(!out.files.iter().any(|f| f.name == "two.pdf"), "/b/ was not explored");
        let only_one = GrabRequest { files_only: vec!["one".into()], ..request(1) };
        let names: Vec<String> = crawl(&only_one, site, |_| None, |_| {}, &none, |_| {}).files.into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["one.pdf"]);
        let out = crawl(&GrabRequest { start: "https://s/a/".into(), no_parent: true, ..request(2) }, site, |_| None, |_| {}, &none, |_| {});
        assert!(out.files.iter().any(|f| f.name == "four.pdf") && !out.files.iter().any(|f| f.name == "two.pdf"), "stays below /a/");
    }

    #[test]
    fn sizes_filter_and_hide_duplicates() {
        let none = AtomicBool::new(false);
        let sizes = |u: &str| -> Option<u64> {
            match u.rsplit('/').next()? {
                "one.pdf" => Some(500),
                "two.pdf" => Some(5000),
                "top.pdf" => Some(50),
                _ => None,
            }
        };
        let req = GrabRequest { min_size: Some(100), max_size: Some(1000), ..request(1) };
        let names: Vec<String> = crawl(&req, site, sizes, |_| {}, &none, |_| {}).files.into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["one.pdf", "three x.pdf"], "unknown sizes stay");

        // The same name and size at two addresses shows once.
        let twice = |url: &str| -> Result<Page, String> {
            let body = match url {
                "https://s/" => r#"<a href="/x/a.pdf">1</a><a href="/y/a.pdf">2</a><a href="/z/a.pdf">3</a>"#,
                _ => return Err("404".into()),
            };
            Ok(Page { url: url.into(), content_type: Some("text/html".into()), body: body.as_bytes().to_vec() })
        };
        let size = |u: &str| Some(if u.contains("/z/") { 9 } else { 7 });
        let out = crawl(&request(0), twice, size, |_| {}, &none, |_| {});
        let urls: Vec<&str> = out.files.iter().map(|f| f.url.as_str()).collect();
        assert_eq!(urls, ["https://s/x/a.pdf", "https://s/z/a.pdf"]);
    }

    #[test]
    fn logout_pages_are_skipped_by_default() {
        let out = crawl(&request(1), |u| {
            if u == "https://s/" {
                Ok(Page {
                    url: u.into(),
                    content_type: Some("text/html".into()),
                    body: br#"<a href="/account/logout">out</a><a href="/a/">a</a>"#.to_vec(),
                })
            } else if u.contains("logout") {
                panic!("opened the logout page");
            } else {
                site(u)
            }
        }, |_| None, |_| {}, &AtomicBool::new(false), |_| {});
        assert!(out.pages >= 2);
        assert!(on_start_site(&request(1), "https://s/a/"));
        assert!(!on_start_site(&request(1), "https://cdn.s/a"));
        assert!(on_start_site(&GrabRequest { subdomains: true, ..request(1) }, "https://cdn.s/a"));
    }

    #[test]
    fn projects_and_the_template_are_saved_and_loaded() {
        let dir = std::env::temp_dir().join(format!("uidm_grab_projects_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(recent_projects(&dir).is_empty());
        assert_eq!(template(&dir), GrabRequest::default());
        let p = Project {
            name: "Docs: manuals".into(),
            request: GrabRequest { depth: 3, ..request(1) },
            files: vec![GrabFile { url: "https://s/a.pdf".into(), name: "a.pdf".into(), page: "https://s/".into(), size: None, text: None }],
            checked: vec![false],
            schedule: None,
            run: RunAction::Explore,
            download_new: false,
            when_done: crate::queue::WhenDone::Nothing,
            open_when_done: None,
            queue: crate::queue::MAIN_QUEUE,
            seen: Vec::new(),
            last_scheduled_day: None,
            last_scheduled_start: None,
        };
        p.save(&dir).unwrap();
        assert_eq!(Project::load(&dir, "Docs: manuals").unwrap(), p);
        std::thread::sleep(Duration::from_millis(20));
        Project { name: "Second".into(), ..p.clone() }.save(&dir).unwrap();
        assert_eq!(recent_projects(&dir), ["Second", "Docs: manuals"], "most recent first");
        save_template(&dir, &GrabRequest { max_pages: 7, ..request(1) }).unwrap();
        let t = template(&dir);
        assert_eq!((t.max_pages, t.start.as_str()), (7, ""), "a template has no start page");
        assert!(Project { name: " ".into(), ..p }.save(&dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn named_templates_are_saved_listed_and_deleted() {
        let dir = std::env::temp_dir().join(format!("uidm_grab_templates_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let names = |dir: &Path| templates(dir).into_iter().map(|t| t.name).collect::<Vec<_>>();
        let built_in = built_in_templates().len();
        assert_eq!(templates(&dir).len(), built_in);
        assert!(templates(&dir).iter().all(|t| t.built_in && t.request.validate().is_err()), "no start page");
        save_named_template(&dir, "My PDFs", &GrabRequest { depth: 4, ..request(1) }).unwrap();
        save_named_template(&dir, "a: zips", &request(1)).unwrap();
        assert_eq!(names(&dir)[built_in..], ["a: zips", "My PDFs"]);
        let mine = templates(&dir).into_iter().find(|t| t.name == "My PDFs").unwrap();
        assert_eq!((mine.request.depth, mine.request.start.as_str(), mine.built_in), (4, "", false));
        assert!(save_named_template(&dir, "pictures from a site", &request(1)).is_err());
        assert!(save_named_template(&dir, " ", &request(1)).is_err());
        delete_template(&dir, "My PDFs").unwrap();
        assert_eq!(names(&dir).len(), built_in + 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn filters_turn_masks_into_types_and_are_kept() {
        let f = |mask: &str| Filter { name: "x".into(), mask: mask.into() }.types();
        assert_eq!(f("*.JPG; *.png,gif"), ["jpg", "png", "gif"]);
        assert_eq!(f("*.*"), ["*"]);
        assert_eq!(f("*"), ["*"]);
        assert!(f(" ; ").is_empty());
        let dir = std::env::temp_dir().join(format!("uidm_grab_filters_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(load_filters(&dir), default_filters());
        let mine = vec![Filter { name: "Manuals".into(), mask: "*.pdf".into() }];
        save_filters(&dir, &mine).unwrap();
        assert_eq!(load_filters(&dir), mine);
        // Every default keeps something a crawl accepts.
        assert!(default_filters().iter().all(|f| GrabRequest { types: f.types(), ..request(1) }.validate().is_ok()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn older_projects_that_added_new_files_now_download_them() {
        let dir = std::env::temp_dir().join(format!("uidm_grab_old_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let json = r#"{"name":"old","request":{"start":"https://s/","depth":1,"same_site":true,"types":["pdf"],"max_pages":5,"max_bytes":1048576,"delay_ms":1000},"download_new":true}"#;
        fs::write(project_file(&dir, "old"), json).unwrap();
        let p = Project::load(&dir, "old").unwrap();
        assert_eq!(p.run, RunAction::ExploreAndDownload);
        assert!(!serde_json::to_string(&p).unwrap().contains("download_new"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_run_adds_only_what_earlier_runs_did_not_find() {
        let file = |u: &str| GrabFile { url: u.into(), name: u.rsplit('/').next().unwrap().into(), page: "https://s/".into(), size: None, text: None };
        let mut p: Project = serde_json::from_str(r#"{"name":"p","request":{"start":"https://s/","depth":1,"same_site":true,"types":["pdf"],"max_pages":5,"max_bytes":1048576,"delay_ms":1000}}"#).unwrap();
        assert_eq!(p.queue, crate::queue::MAIN_QUEUE, "an older project file loads");
        let first = p.record_run(vec![file("https://s/a.pdf"), file("https://s/b.pdf")]);
        assert_eq!(first.len(), 2);
        let second = p.record_run(vec![file("https://s/b.pdf"), file("https://s/c.pdf")]);
        assert_eq!(second.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["c.pdf"]);
        assert_eq!(p.checked, [false, true], "only the new one is ticked");
        assert_eq!(p.seen.len(), 3);
    }

    #[test]
    fn mirror_paths_follow_addresses() {
        let root = Path::new(r"C:\site");
        let at = |u: &str, page: bool| mirror_path(root, &Url::parse(u).unwrap(), page);
        assert_eq!(at("https://s.com/docs/a%20b.pdf", false), root.join("s.com").join("docs").join("a b.pdf"));
        assert_eq!(at("https://s.com/docs/", true), root.join("s.com").join("docs").join("index.html"));
        assert_eq!(at("https://s.com/", true), root.join("s.com").join("index.html"));
        assert_eq!(at("https://s.com/list.php", true), root.join("s.com").join("list.php.html"));
        assert_ne!(at("https://s.com/list.php?page=1", true), at("https://s.com/list.php?page=2", true));
        assert_eq!(at("https://s.com/a/../b:c.pdf", false), root.join("s.com").join("b_c.pdf"));
    }

    #[test]
    fn saved_pages_link_to_the_local_copies() {
        let root = Path::new(r"C:\site");
        let page = Url::parse("https://s/docs/index.html").unwrap();
        let here = mirror_path(root, &page, true);
        let req = GrabRequest { start: "https://s/".into(), ..GrabRequest::default() };
        let html = r##"<a href="guide.pdf">g</a> <a href='/top/'>t</a> <a href="https://other/x.pdf">o</a> <a href="#part">p</a> <img src="pic.png">"##;
        let out = rewrite_links(html, &page, &here, root, |u| kept_locally(&req, u));
        assert!(out.contains(r#"href="guide.pdf""#), "same folder");
        assert!(out.contains("href='../top/index.html'"));
        assert!(out.contains(r#"href="../../other/x.pdf""#), "a kept file on another site");
        assert!(out.contains(r#"src="pic.png""#), "not a kept type: untouched");
        assert!(out.contains(r##"href="index.html#part""##));

        let file = GrabFile { url: "https://s/docs/guide.pdf".into(), name: "guide.pdf".into(), page: page.to_string(), size: None, text: None };
        let settings = crate::settings::Settings::default();
        let dir = std::env::temp_dir().join(format!("uidm_mirror_{}", std::process::id()));
        let mirrored = GrabRequest { mirror: Some(Mirror { folder: dir.display().to_string(), pages: true, overwrite: false }), ..req.clone() };
        let d = download_for(&mirrored, &file, &settings).unwrap();
        assert_eq!(d.folder.as_deref(), Some(dir.join("s").join("docs").display().to_string().as_str()));
        assert_eq!(d.filename.as_deref(), Some("guide.pdf"));
        std::fs::create_dir_all(dir.join("s").join("docs")).unwrap();
        std::fs::write(dir.join("s").join("docs").join("guide.pdf"), b"x").unwrap();
        assert!(download_for(&mirrored, &file, &settings).is_none(), "an existing file is skipped");
        let replace = GrabRequest { mirror: Some(Mirror { overwrite: true, ..mirrored.mirror.clone().unwrap() }), ..req.clone() };
        assert!(download_for(&replace, &file, &settings).is_some());
        assert_eq!(download_for(&req, &file, &settings).unwrap().folder, None, "no mirror: the category decides");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn requests_are_checked() {
        assert!(request(2).validate().is_ok());
        assert!(GrabRequest { start: "ftp://s/".into(), ..request(2) }.validate().is_err());
        assert!(GrabRequest { start: "s/".into(), ..request(2) }.validate().is_err());
        assert!(request(11).validate().is_err());
        assert!(GrabRequest { max_pages: 0, ..request(1) }.validate().is_err());
        assert!(GrabRequest { delay_ms: 0, ..request(1) }.validate().is_err());
        assert!(GrabRequest { types: vec![], ..request(1) }.validate().is_err());
    }
}
