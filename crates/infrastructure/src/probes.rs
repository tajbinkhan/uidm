//! `ServerProbes`, the `Probes` adapter: it asks servers about downloads
//! with the engine, building each request the way `Transfers` would for a
//! download of it (the settings' proxy and logins, the browser's cookies).

use std::sync::Arc;
use udm_domain::bytes::ByteCount;

use udm_domain::address::Address;
use udm_domain::download::{Download, ZipEntry};
use udm_domain::gallery::{Gallery, GalleryError};
use udm_domain::media::MediaInfo;
use udm_domain::ports::{FileFacts, LinkFacts, ProbeError, Probes};

use crate::engine;
use crate::transfers::Transfers;

/// Asks servers through the engine.
pub struct ServerProbes {
    transfers: Arc<Transfers>,
}

impl ServerProbes {
    pub fn new(transfers: Arc<Transfers>) -> Self {
        ServerProbes { transfers }
    }
}

fn probe_error(e: impl ToString) -> ProbeError {
    ProbeError(e.to_string())
}

impl Probes for ServerProbes {
    fn link(&self, download: &Download) -> Result<LinkFacts, ProbeError> {
        // Not through the debrid account: the link picker probes every link
        // on a page, and unlocking each would send them all to the service
        // (and fill the account's history) before the user picked any.
        let req = self.transfers.request_for(download.clone());
        let found = engine::inspect(&req.url, None, &req.headers, &req.net).map_err(probe_error)?;
        Ok(LinkFacts { total: found.total, content_type: found.content_type })
    }

    fn file(&self, download: &Download) -> Result<FileFacts, ProbeError> {
        // The file a debrid account unlocks the link into, when it does.
        let req = self.transfers.resolved_request(download.clone()).map_err(probe_error)?;
        // Probing a POST download would send a GET (or repeat the POST),
        // so its dialog proposes a name from the request alone. What the
        // browser's response said is all there is to show then. A video
        // page or a stream is no file either: its address is a web page or
        // a playlist, and the video is made from it only when it runs.
        let found = if crate::app::post_body(download).is_some() || download.media.is_some() {
            let mut found = engine::Inspection::unprobed(download.url.as_str(), download.filename.as_deref());
            found.total = download.size;
            found.content_type = download.content_type.clone();
            found
        } else {
            engine::inspect(&req.url, req.filename.as_deref(), &req.headers, &req.net).map_err(probe_error)?
        };
        let path = req.folders.for_name(found.filename.as_str()).join(&found.filename);
        Ok(FileFacts { filename: found.filename, path, total: found.total, resumable: found.resumable })
    }

    fn media(&self, download: &Download) -> MediaInfo {
        let req = self.transfers.request_for(download.clone());
        let page = download.media.as_ref().is_some_and(|m| m.kind == Some(udm_domain::media::MediaKind::Page));
        engine::probe_media(&req.url, &req.headers, &req.net, req.ffmpeg.as_deref(), page)
    }

    fn zip(&self, download: &Download, total: Option<ByteCount>) -> Result<Vec<ZipEntry>, ProbeError> {
        let req = self.transfers.resolved_request(download.clone()).map_err(probe_error)?;
        let total = match total {
            Some(t) => t,
            None => engine::inspect(&req.url, None, &req.headers, &req.net)
                .map_err(probe_error)?
                .total
                .ok_or_else(|| probe_error("The server does not say how big the file is."))?,
        };
        crate::zip::list(total.get(), |a, b| {
            engine::fetch_range_bytes(&req.url, &req.headers, &req.net, a, b)
                .map_err(|e| crate::zip::ZipError::Unread(format!("The server would not send part of the file: {e}")))
        })
        .map_err(probe_error)
    }

    fn gallery(&self, page: &Address, user_agent: Option<&str>) -> Result<Gallery, GalleryError> {
        let program = crate::media::gallerydl::find().ok_or(GalleryError::Missing)?;
        // The proxy a download of the page would use; no cookies.
        let req = self.transfers.request_for(Download { url: page.clone().into(), ..Download::default() });
        let proxy = req.net.proxy.as_ref().and_then(|p| p.proxy_for(page.as_str()));
        // Bound to the chosen network adapter through gallery-dl's own
        // option; not run at all while it is down.
        let source = req.net.route.source_address().map_err(|down| GalleryError::Failed(down.to_string()))?;
        crate::media::gallerydl::probe(
            &program,
            page.as_str(),
            proxy.as_deref(),
            source,
            user_agent,
            crate::media::gallerydl::PROBE_TIMEOUT,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::engine::testserver::{self, TestServer};
    use crate::live_settings::LiveSettings;
    use crate::store::{DownloadStore, Ended, StoreEvents, StoreFiles};
    use udm_domain::settings::Settings;

    struct Quiet;

    impl StoreEvents for Quiet {
        fn announce(&self, _: &crate::protocol::Outgoing) {}
        fn ended(&self, _: Ended) {}
        fn wake(&self) {}
    }

    /// Probes saving to `name`'s folder under the temp folder.
    fn probes(name: &str) -> (ServerProbes, PathBuf) {
        let dir = std::env::temp_dir().join(format!("udm_probes_{}_{name}", std::process::id()));
        let events: Arc<dyn StoreEvents> = Arc::new(Quiet);
        let (store, _, next_id) = DownloadStore::open(StoreFiles::default(), Arc::clone(&events));
        let settings = Settings { download_dir: Some(dir.display().to_string()), ..Settings::default() };
        let live = Arc::new(LiveSettings::new(settings, None));
        let transfers = Arc::new(Transfers::new(Arc::new(store), events, live, Arc::default(), next_id));
        (ServerProbes::new(transfers), dir)
    }

    fn download(url: &str) -> Download {
        Download { url: url.into(), ..Download::default() }
    }

    #[test]
    fn a_link_says_its_size_and_a_dead_one_why_not() {
        let (probes, _) = probes("link");
        let server = TestServer::start(testserver::body(1000));
        let facts = probes.link(&download(&server.url)).unwrap();
        assert_eq!(facts.total, Some(ByteCount::new(1000)));
        assert!(probes.link(&download("http://127.0.0.1:1/gone.bin")).is_err());
    }

    #[test]
    fn the_dialog_proposes_the_servers_file_in_its_category_folder() {
        let (probes, dir) = probes("file");
        let server = TestServer::start(testserver::body(500));
        let facts = probes.file(&download(&server.url)).unwrap();
        assert_eq!(facts.filename, "file.bin");
        assert_eq!(facts.total, Some(ByteCount::new(500)));
        assert!(facts.path.starts_with(&dir) && facts.path.ends_with("file.bin"), "{:?}", facts.path);
    }

    #[test]
    fn a_form_post_is_not_sent_again_to_name_it() {
        let (probes, _) = probes("post");
        let server = TestServer::start(testserver::body(10));
        let post = Download {
            method: Some("POST".into()),
            body: Some("YT0x".into()),
            filename: Some("report.pdf".into()),
            size: Some(ByteCount::new(77)),
            ..download(&server.url)
        };
        let facts = probes.file(&post).unwrap();
        assert_eq!((facts.filename.as_str(), facts.total), ("report.pdf", Some(ByteCount::new(77))));
        assert_eq!(server.served.load(std::sync::atomic::Ordering::Relaxed), 0, "the server was not asked");
    }

    #[test]
    fn a_video_page_or_stream_is_named_without_asking_its_server() {
        use udm_domain::media::{MediaChoice, MediaKind};
        let (probes, dir) = probes("media");
        let server = TestServer::start(testserver::body(10));
        for kind in [MediaKind::Page, MediaKind::Hls] {
            let video = Download {
                filename: Some("Clip.mp4".into()),
                media: Some(MediaChoice { kind: Some(kind), video: Some("298".into()), ..MediaChoice::default() }),
                ..download(&server.url)
            };
            let facts = probes.file(&video).unwrap();
            assert_eq!((facts.filename.as_str(), facts.total), ("Clip.mp4", None), "{kind:?}");
            assert!(facts.path.starts_with(&dir) && facts.path.ends_with("Clip.mp4"), "{:?}", facts.path);
        }
        assert_eq!(server.served.load(std::sync::atomic::Ordering::Relaxed), 0, "the server was not asked");
        // A page that cannot be reached still gets its dialog.
        let gone = Download {
            filename: Some("Clip.mp4".into()),
            media: Some(MediaChoice { kind: Some(MediaKind::Page), ..MediaChoice::default() }),
            ..download("http://127.0.0.1:1/watch?v=x")
        };
        assert_eq!(probes.file(&gone).unwrap().filename, "Clip.mp4");
    }
}
