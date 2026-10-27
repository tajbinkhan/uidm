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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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
}

/// Pages a crawl should never open, so it does not sign itself out.
pub const LOGOUT_PATTERNS: [&str; 5] = ["*logout*", "*logoff*", "*log-out*", "*signout*", "*sign-out*"];

pub const DEPTH_RANGE: (u32, u32) = (0, 10);
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
        if !in_range(self.depth as u64, (DEPTH_RANGE.0 as u64, DEPTH_RANGE.1 as u64)) {
            return Err(format!("The depth must be between {} and {}.", DEPTH_RANGE.0, DEPTH_RANGE.1));
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
/// that may be pages, and those that may be files.
pub fn links(html: &str, page: &Url) -> (Vec<Url>, Vec<Url>) {
    let doc = Html::parse_document(html);
    let base = Selector::parse("base[href]")
        .ok()
        .and_then(|s| doc.select(&s).next().and_then(|b| b.value().attr("href")).and_then(|h| page.join(h).ok()))
        .filter(|b| matches!(b.scheme(), "http" | "https"))
        .unwrap_or_else(|| page.clone());
    let pick = |selector: &str, attr: &str| -> Vec<Url> {
        let Ok(sel) = Selector::parse(selector) else { return Vec::new() };
        doc.select(&sel)
            .filter_map(|e| e.value().attr(attr))
            .filter_map(|href| base.join(href.trim()).ok())
            .filter(|u| matches!(u.scheme(), "http" | "https"))
            .map(|mut u| {
                u.set_fragment(None);
                u
            })
            .collect()
    };
    let mut pages = pick("a[href]", "href");
    pages.extend(pick("area[href]", "href"));
    pages.extend(pick("iframe[src], frame[src]", "src"));
    let mut files = pages.clone();
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

/// How a crawl ended.
pub struct Outcome {
    pub files: Vec<GrabFile>,
    pub pages: u64,
    /// Why it stopped before reading everything in reach, if it did.
    pub stopped: Option<String>,
}

/// Crawl from `req.start`. `fetch` reads a page (at most `MAX_PAGE_BYTES`);
/// `size` asks a file's size, for the size filters and duplicates; `wait`
/// sleeps between requests (tests pass a no-op); `progress` hears pages
/// read, bytes read and files found after each page. `cancel` stops the
/// crawl between requests.
pub fn crawl(
    req: &GrabRequest,
    mut fetch: impl FnMut(&str) -> Result<Page, String>,
    mut size: impl FnMut(&str) -> Option<u64>,
    mut wait: impl FnMut(Duration),
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64, usize),
) -> Outcome {
    let mut out = Outcome {
        files: Vec::new(),
        pages: 0,
        stopped: None,
    };
    let Ok(mut start) = Url::parse(req.start.trim()) else {
        out.stopped = Some("The start page is not a web address.".into());
        return out;
    };
    start.set_fragment(None);
    let start_host = start.host_str().unwrap_or("").to_ascii_lowercase();
    // The domain whose subdomains count as the same site: the start host
    // without a leading www.
    let domain = start_host.strip_prefix("www.").unwrap_or(&start_host).to_string();
    // The folder the crawl may not go above.
    let start_dir = start.path().rsplit_once('/').map_or("/".to_string(), |(dir, _)| format!("{dir}/"));
    // The files kept so far, by name (as indexes into `out.files`), to spot
    // duplicates, and those whose size was asked.
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    let mut asked: HashSet<usize> = HashSet::new();
    let types: HashSet<String> = req.types.iter().map(|t| t.trim().trim_start_matches('.').to_ascii_lowercase()).collect();
    let delay = Duration::from_millis(req.delay_ms);

    let mut robots: HashMap<String, Robots> = HashMap::new();
    let mut queue: VecDeque<(Url, u32)> = VecDeque::from([(start.clone(), 0)]);
    let mut seen_pages: HashSet<String> = HashSet::from([start.to_string()]);
    let mut seen_files: HashSet<String> = HashSet::new();
    let mut bytes = 0u64;
    let mut first = true;
    let mut skipped_by_robots = 0u64;

    while let Some((url, depth)) = queue.pop_front() {
        if cancel.load(Ordering::Relaxed) {
            out.stopped = Some("Stopped.".into());
            break;
        }
        if out.pages >= req.max_pages {
            out.stopped = Some(format!("Stopped at the budget of {} pages.", req.max_pages));
            break;
        }
        if bytes >= req.max_bytes {
            out.stopped = Some(format!("Stopped at the budget of {} MB.", req.max_bytes / (1024 * 1024)));
            break;
        }
        let origin = url.origin().ascii_serialization();
        if !robots.contains_key(&origin) {
            if !first {
                wait(delay);
            }
            first = false;
            let rules = match fetch(&format!("{origin}/robots.txt")) {
                Ok(page) => Robots::parse(&String::from_utf8_lossy(&page.body), USER_AGENT),
                Err(_) => Robots::default(),
            };
            robots.insert(origin.clone(), rules);
        }
        let rules = &robots[&origin];
        if !rules.allows(&url) {
            skipped_by_robots += 1;
            continue;
        }
        let pause = rules.crawl_delay.map_or(delay, |d| d.max(delay));
        if !first {
            wait(pause);
        }
        first = false;
        if cancel.load(Ordering::Relaxed) {
            out.stopped = Some("Stopped.".into());
            break;
        }
        let page = match fetch(url.as_str()) {
            Ok(page) => page,
            Err(_) => continue,
        };
        out.pages += 1;
        bytes += page.body.len() as u64;
        let html = page
            .content_type
            .as_deref()
            .is_none_or(|t| t.to_ascii_lowercase().contains("html"));
        if html {
            let here = Url::parse(&page.url).unwrap_or(url.clone());
            let text = String::from_utf8_lossy(&page.body);
            let (pages, files) = links(&text, &here);
            for file in files {
                let Some(ext) = url_extension(&file) else { continue };
                if !types.contains(&ext) || !passes(file.as_str(), &req.files_only, &req.files_skip) {
                    continue;
                }
                if !seen_files.insert(file.to_string()) {
                    continue;
                }
                let name = file
                    .path_segments()
                    .and_then(|mut s| s.next_back())
                    .map(|n| percent_encoding::percent_decode_str(n).decode_utf8_lossy().into_owned())
                    .unwrap_or_default();
                // Ask the size only when a filter needs it, or to compare
                // with files of the same name kept earlier (whose sizes are
                // asked then too).
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
                });
            }
            if depth < req.depth {
                for next in pages {
                    let host = next.host_str().unwrap_or("").to_ascii_lowercase();
                    let exact = host == start_host;
                    let sub = req.subdomains && (host == domain || host.ends_with(&format!(".{domain}")));
                    let below = !req.no_parent || !exact || next.path().starts_with(&start_dir);
                    let page_like = url_extension(&next).is_none_or(|e| PAGE_EXTENSIONS.contains(&e.as_str()));
                    let wanted = passes(next.as_str(), &req.explore_only, &req.explore_skip);
                    if (exact || sub || !req.same_site) && below && page_like && wanted && seen_pages.insert(next.to_string()) {
                        queue.push_back((next, depth + 1));
                    }
                }
            }
        }
        progress(out.pages, bytes, out.files.len());
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
        assert!(files.iter().any(|f| f.as_str() == "https://cdn.s/pic.png"));

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
        let fetched = RefCell::new(Vec::new());
        let waits = RefCell::new(0);
        let out = crawl(
            &request(1),
            |u| {
                fetched.borrow_mut().push(u.to_string());
                site(u)
            },
            |_| None,
            |_| *waits.borrow_mut() += 1,
            &AtomicBool::new(false),
            |_, _, _| {},
        );
        let names: Vec<&str> = out.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["top.pdf", "one.pdf", "two.pdf", "three x.pdf"]);
        assert_eq!(out.files[1].page, "https://s/a/");
        assert_eq!(out.pages, 3, "/, /a/, /b/");
        let fetched = fetched.into_inner();
        assert!(!fetched.iter().any(|u| u.contains("secret")), "robots.txt is obeyed");
        assert!(!fetched.iter().any(|u| u.contains("elsewhere")), "same site only");
        assert!(!fetched.iter().any(|u| u.contains("deep")), "depth 1");
        assert!(!fetched.iter().any(|u| u.ends_with(".pdf")), "files are listed, not fetched");
        assert_eq!(fetched.iter().filter(|u| u.ends_with("robots.txt")).count(), 1);
        assert_eq!(*waits.borrow(), fetched.len() - 1, "a wait before every request but the first");
        assert!(out.stopped.unwrap().contains("robots.txt"));

        let deeper = crawl(&request(2), site, |_| None, |_| {}, &AtomicBool::new(false), |_, _, _| {});
        assert!(deeper.files.iter().any(|f| f.name == "four.pdf"));
        let anywhere = crawl(
            &GrabRequest { same_site: false, ..request(1) },
            site,
            |_| None,
            |_| {},
            &AtomicBool::new(false),
            |_, _, _| {},
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
            |_, _, _| {},
        );
        assert_eq!(out.pages, 2);
        assert!(out.stopped.unwrap().contains("2 pages"));

        let cancel = AtomicBool::new(false);
        let out = crawl(&request(3), site, |_| None, |_| {}, &cancel, |pages, _, _| {
            if pages == 1 {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert_eq!(out.pages, 1);
        assert_eq!(out.stopped.as_deref(), Some("Stopped."));
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
        let out = crawl(&skip_b, site, |_| None, |_| {}, &none, |_, _, _| {});
        assert!(!out.files.iter().any(|f| f.name == "two.pdf"), "/b/ was not explored");
        let only_one = GrabRequest { files_only: vec!["one".into()], ..request(1) };
        let names: Vec<String> = crawl(&only_one, site, |_| None, |_| {}, &none, |_, _, _| {}).files.into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["one.pdf"]);
        let out = crawl(&GrabRequest { start: "https://s/a/".into(), no_parent: true, ..request(2) }, site, |_| None, |_| {}, &none, |_, _, _| {});
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
        let names: Vec<String> = crawl(&req, site, sizes, |_| {}, &none, |_, _, _| {}).files.into_iter().map(|f| f.name).collect();
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
        let out = crawl(&request(0), twice, size, |_| {}, &none, |_, _, _| {});
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
        }, |_| None, |_| {}, &AtomicBool::new(false), |_, _, _| {});
        assert!(out.pages >= 2);
        assert!(on_start_site(&request(1), "https://s/a/"));
        assert!(!on_start_site(&request(1), "https://cdn.s/a"));
        assert!(on_start_site(&GrabRequest { subdomains: true, ..request(1) }, "https://cdn.s/a"));
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
