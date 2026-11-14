//! `Crawler`, the `SiteCrawler` adapter: one crawl with the engine, the
//! settings' proxy and logins (and the project's own login for its start
//! site), the browser's cookies and rendered pages when the request asks,
//! and short waits so a stop is noticed quickly. Each request goes out the
//! way the settings say when it is made (the network adapter and resolver),
//! so a change reaches a crawl that is running.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use tracing::{info, warn};
use udm_domain::download::Download;
use udm_domain::grab::{GrabRequest, Outcome, Page};
use udm_domain::ports::{Browser, SiteCrawler};

use crate::engine;
use crate::grab;
use crate::live_settings::LiveSettings;
use crate::net::Login;
use crate::transfers::Transfers;

/// How the site grabber names itself to servers.
const GRABBER_AGENT: &str = "Mozilla/5.0 (compatible; UDM site grabber; obeys robots.txt)";

/// Crawls with the engine.
pub struct Crawler {
    transfers: Arc<Transfers>,
    settings: Arc<LiveSettings>,
    browser: Arc<dyn Browser>,
}

impl Crawler {
    pub fn new(transfers: Arc<Transfers>, settings: Arc<LiveSettings>, browser: Arc<dyn Browser>) -> Self {
        Crawler { transfers, settings, browser }
    }
}

/// Keep `page` under the request's mirror folder, when it keeps pages.
fn mirror(req: &GrabRequest, page: &Page) {
    if let Some(m) = req.mirror.as_ref().filter(|m| m.pages) {
        if let Err(e) = grab::save_page(req, m, page) {
            warn!(error = %e, "could not save");
        }
    }
}

impl SiteCrawler for Crawler {
    fn crawl(
        &self,
        req: &GrabRequest,
        login: Option<&str>,
        stop: &AtomicBool,
        progress: &mut dyn FnMut(&Outcome),
    ) -> Outcome {
        let settings = self.settings.lock().clone();
        let start = Download { url: req.start.as_str().into(), ..Download::default() };
        let mut net = self.transfers.net_for(&start, &settings);
        if let (Some(user), Ok(url)) =
            (req.login_user.as_deref().map(str::trim).filter(|u| !u.is_empty()), url::Url::parse(&req.start))
        {
            if let Some(host) = url.host_str() {
                net.logins.push(Login {
                    site: host.to_string(),
                    username: user.to_string(),
                    password: grab::project_password(&req.start, user).unwrap_or_default(),
                    // Given for this project's start site.
                    plain: true,
                });
            }
        }
        let headers = [("User-Agent".to_string(), GRABBER_AGENT.to_string())];
        let cookies = if req.browser_cookies { self.browser.cookies(&req.start) } else { None };
        // Each site's cookies go to that site's pages only.
        let page_headers = |url: &str| {
            let mut h = headers.to_vec();
            if let Some(c) = udm_domain::grab::crawl_cookies(req, url, cookies.as_deref(), login) {
                h.push(("Cookie".to_string(), c));
            }
            h
        };
        // Rendering needs the browser extension connected.
        let render = AtomicBool::new(req.render_scripts && self.browser.connected());
        let fetch = |url: &str| {
            // A page as the browser shows it, when asked and the browser
            // can; otherwise as the server sends it.
            let rendering = render.load(Ordering::Relaxed) && !url.ends_with("/robots.txt");
            let rendered = rendering.then(|| self.browser.render(url)).flatten();
            // A browser that does not answer once is not asked again.
            if rendering && rendered.is_none() {
                info!(url = %url, "the browser did not render the page; reading pages as sent");
                render.store(false, Ordering::Relaxed);
            }
            if let Some(mut shown) = rendered {
                // Pages the page's scripts opened in new windows are links
                // like the rest, unless the crawl ignores them.
                if !req.ignore_popups {
                    for p in shown.popups.iter().filter(|p| p.starts_with("http://") || p.starts_with("https://")) {
                        shown.html.push_str(&format!("<a href=\"{}\"></a>", p.replace('"', "%22")));
                    }
                }
                let mut body = shown.html.into_bytes();
                body.truncate(usize::try_from(udm_domain::grab::MAX_PAGE_BYTES.get()).unwrap_or(usize::MAX));
                let page = Page { url: shown.url, content_type: Some("text/html".into()), body };
                mirror(req, &page);
                return Some(page);
            }
            let net = net.with_route(self.transfers.live_route());
            let page = engine::fetch(url, &page_headers(url), &net, udm_domain::grab::MAX_PAGE_BYTES)
                .map(|f| Page { url: f.url, content_type: f.content_type, body: f.body })
                .ok()?;
            mirror(req, &page);
            Some(page)
        };
        // Wait in short steps, so stopping is quick.
        let wait = |d: Duration| {
            let until = Instant::now() + d;
            while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100).min(until - Instant::now()));
            }
        };
        // A file's size, for the size filters and duplicates.
        let size = |url: &str| {
            let net = net.with_route(self.transfers.live_route());
            engine::inspect(url, None, &page_headers(url), &net).ok().and_then(|i| i.total)
        };
        grab::crawl(req, fetch, size, wait, stop, progress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testserver::TestServer;
    use crate::store::{DownloadStore, Ended, StoreEvents, StoreFiles};
    use udm_domain::grab::RenderedPage;
    use udm_domain::network::{AdapterChoice, AdapterId};
    use udm_domain::settings::Settings;

    struct Quiet;

    impl StoreEvents for Quiet {
        fn announce(&self, _: &crate::protocol::Outgoing) {}
        fn ended(&self, _: Ended) {}
        fn wake(&self) {}
    }

    struct NoBrowser;

    impl Browser for NoBrowser {
        fn connected(&self) -> bool {
            false
        }
        fn cookies(&self, _: &str) -> Option<String> {
            None
        }
        fn render(&self, _: &str) -> Option<RenderedPage> {
            None
        }
        fn answered_cookies(&self, _: u64, _: Option<String>) {}
        fn answered_render(&self, _: u64, _: Option<RenderedPage>) {}
    }

    #[test]
    fn a_running_crawl_follows_a_change_of_the_network_adapter() {
        let page = b"<a href=\"/1.html\">1</a> <a href=\"/2.html\">2</a> <a href=\"/3.html\">3</a>".to_vec();
        let server = TestServer::start(page);
        let events: Arc<dyn StoreEvents> = Arc::new(Quiet);
        let (store, _, next_id) = DownloadStore::open(StoreFiles::default(), Arc::clone(&events));
        let live = Arc::new(LiveSettings::new(Settings::default(), None));
        let transfers = Arc::new(Transfers::new(Arc::new(store), events, Arc::clone(&live), Arc::default(), next_id));
        let crawler = Crawler::new(transfers, Arc::clone(&live), Arc::new(NoBrowser));
        let req = GrabRequest { start: format!("{}/", server.url), depth: 1, ..GrabRequest::default() };
        let mut changed_at = None;
        let out = crawler.crawl(&req, None, &AtomicBool::new(false), &mut |o: &Outcome| {
            if o.pages >= 1 && changed_at.is_none() {
                // Bound now to an adapter that is not there: nothing more
                // may be fetched.
                live.lock().network_adapter = Some(AdapterChoice {
                    id: AdapterId::parse("udmnone0").unwrap(),
                    name: "Gone VPN".into(),
                    subnets: Vec::new(),
                });
                changed_at = Some(server.connections.load(Ordering::SeqCst));
            }
        });
        let changed_at = changed_at.expect("the first page was read");
        assert_eq!(out.pages, 1, "no page after the change: {out:?}");
        assert_eq!(server.connections.load(Ordering::SeqCst), changed_at, "no connection after the change");
    }
}
