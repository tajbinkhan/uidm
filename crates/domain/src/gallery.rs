//! Image galleries: the pictures gallery-dl lists for a page (a Wikimedia
//! Commons category, a Flickr album, a DeviantArt gallery, ...), which of
//! them the user ticks, the one folder they are saved in, and how often a
//! page may be read. gallery-dl only lists the pictures; UDM's own
//! engine downloads them. Running gallery-dl is
//! `udm_infrastructure::media::gallerydl`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::address::Address;
use crate::batch::is_web_url;
use crate::download::Download;
use crate::filename::{name_from_url, numbered_name, sanitize_filename};

/// The most pictures of one gallery listed.
pub const MAX_PICTURES: usize = 1000;

/// The most galleries read at once, each by a gallery-dl process of its own.
pub const MAX_READS: usize = 2;

/// How long a page gallery-dl found nothing on is not read again, in
/// seconds: asking again at once would find nothing again.
pub const MISS_KEPT_SECS: u64 = 10 * 60;

/// The pictures of a gallery page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gallery {
    /// The gallery's own name, when gallery-dl gives one.
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub pictures: Vec<Picture>,
}

/// One picture (or other file) of a gallery.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Picture {
    /// The file's own address, which UDM downloads.
    pub url: String,
    /// The name gallery-dl gives the file, without its extension.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Its extension (`jpg`, `png`, `webm`), without the dot.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub extension: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

/// Why a gallery could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum GalleryError {
    /// Only http and https pages are read; the address given.
    #[error("{0} is not an http or https page.")]
    NotWebPage(String),
    /// gallery-dl is nowhere to be found.
    #[error("Image galleries need gallery-dl. Install it in UDM's Options > Tools, or put gallery-dl next to UDM.")]
    Missing,
    /// gallery-dl knows no site like this page's.
    #[error("gallery-dl does not know this site.")]
    Unsupported,
    /// gallery-dl read the page and listed no picture.
    #[error("gallery-dl found no pictures on this page.")]
    NothingFound,
    /// gallery-dl failed, in its own words.
    #[error("gallery-dl: {0}")]
    Failed(String),
    /// gallery-dl did not finish within this many seconds.
    #[error("gallery-dl did not answer within {0} seconds.")]
    TimedOut(u64),
    /// The same page is being read already.
    #[error("This page is being read already.")]
    Busy,
    /// `MAX_READS` galleries are being read already.
    #[error("UDM is reading other galleries. Try again when they are done.")]
    TooMany,
}

impl GalleryError {
    /// Whether the page is not worth reading again soon: gallery-dl read
    /// it and found nothing, or does not know its site.
    fn is_miss(&self) -> bool {
        matches!(self, GalleryError::Unsupported | GalleryError::NothingFound)
    }
}

/// The page to read for a gallery: an http or https address.
pub fn gallery_page(text: &str) -> Result<Address, GalleryError> {
    let text = text.trim();
    if !is_web_url(text) {
        return Err(GalleryError::NotWebPage(text.chars().take(200).collect()));
    }
    Address::parse(text).map_err(|_| GalleryError::NotWebPage(text.chars().take(200).collect()))
}

/// An extension worth keeping: up to 8 letters and digits.
fn clean_extension(ext: &str) -> String {
    let ext = ext.trim().trim_start_matches('.');
    if !ext.is_empty() && ext.len() <= 8 && ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
        ext.to_ascii_lowercase()
    } else {
        String::new()
    }
}

impl Picture {
    /// The file name it is saved under: gallery-dl's name and extension,
    /// else its address's last part, else `picture <n>`, made safe.
    pub fn file_name(&self, n: usize) -> String {
        let ext = clean_extension(&self.extension);
        let stem = self.name.trim();
        let named = if stem.is_empty() {
            None
        } else if ext.is_empty() {
            sanitize_filename(stem)
        } else {
            sanitize_filename(&format!("{stem}.{ext}"))
        };
        let name = named.unwrap_or_else(|| name_from_url(&self.url));
        let name = name.as_str();
        if name == "download" {
            let fallback = if ext.is_empty() { format!("picture {n}") } else { format!("picture {n}.{ext}") };
            return fallback;
        }
        name.to_string()
    }

    /// Its size as `1920 × 1080`, when gallery-dl knows it.
    pub fn dimensions(&self) -> Option<String> {
        Some(format!("{} × {}", self.width?, self.height?))
    }
}

impl Gallery {
    /// The pictures UDM can download: http and https addresses, each
    /// once, at most `MAX_PICTURES`, with a name, extension and title of
    /// a sensible length.
    pub fn cleaned(self) -> Gallery {
        let mut seen = HashSet::new();
        let pictures = self
            .pictures
            .into_iter()
            .filter(|p| is_web_url(&p.url) && Address::parse(&p.url).is_ok())
            .filter(|p| seen.insert(p.url.clone()))
            .take(MAX_PICTURES)
            .map(|p| Picture {
                name: p.name.trim().chars().take(200).collect(),
                extension: clean_extension(&p.extension),
                ..p
            })
            .collect();
        Gallery { title: self.title.trim().chars().take(500).collect(), pictures }
    }

    /// The pictures whose file name holds all of `words` (any case), by
    /// their place in the gallery.
    pub fn matching(&self, words: &str) -> Vec<usize> {
        let words: Vec<String> = words.split_whitespace().map(str::to_lowercase).collect();
        (0..self.pictures.len())
            .filter(|&i| {
                let name = self.pictures[i].file_name(i + 1).to_lowercase();
                words.iter().all(|w| name.contains(w.as_str()))
            })
            .collect()
    }

    /// The downloads for the pictures at `picked`, in the gallery's order,
    /// all saved in `folder` and coming from `page`. Two pictures with the
    /// same name are numbered apart (`a.jpg`, `a (1).jpg`).
    pub fn downloads(&self, picked: &[usize], folder: &Path, page: &str) -> Vec<Download> {
        let mut picked: Vec<usize> = picked.iter().copied().filter(|&i| i < self.pictures.len()).collect();
        picked.sort_unstable();
        picked.dedup();
        let mut taken = HashSet::new();
        let page = Some(page.trim().to_string()).filter(|p| is_web_url(p));
        picked
            .into_iter()
            .filter_map(|i| {
                let p = &self.pictures[i];
                let address = Address::parse(&p.url).ok()?;
                let base = p.file_name(i + 1);
                let name = (0..)
                    .map(|n| numbered_name(&base, n))
                    .find(|name| taken.insert(name.to_lowercase()))
                    .unwrap_or(base);
                Some(Download {
                    url: address.into(),
                    filename: Some(name),
                    referrer: page.clone(),
                    folder: Some(folder.display().to_string()),
                    ..Download::default()
                })
            })
            .collect()
    }
}

/// The folder a gallery is saved in unless the user picks another: a
/// folder named after the gallery in `base` (where its pictures would go
/// by category), or `base` itself for a gallery without a name.
pub fn gallery_folder(base: &Path, title: &str) -> PathBuf {
    let short: String = title.trim().chars().take(80).collect();
    match sanitize_filename(&short) {
        Some(name) => base.join(name.as_str()),
        None => base.to_path_buf(),
    }
}

/// The galleries being read, and the pages read lately that held none:
/// one read of a page at a time, at most `MAX_READS` at once, and a page
/// that held nothing is not read again for `MISS_KEPT_SECS`.
#[derive(Debug, Default)]
pub struct GalleryReads {
    running: HashSet<String>,
    /// Each page that held nothing: when, and what was found.
    misses: HashMap<String, (u64, GalleryError)>,
}

/// The most misses remembered; older ones go first.
const MAX_MISSES: usize = 256;

impl GalleryReads {
    /// Start reading `page` at `now` (seconds since 1970), or say why not.
    pub fn begin(&mut self, page: &Address, now: u64) -> Result<(), GalleryError> {
        let key = page.as_str().to_string();
        self.misses.retain(|_, (at, _)| now.saturating_sub(*at) < MISS_KEPT_SECS);
        if let Some((_, why)) = self.misses.get(&key) {
            return Err(why.clone());
        }
        if self.running.contains(&key) {
            return Err(GalleryError::Busy);
        }
        if self.running.len() >= MAX_READS {
            return Err(GalleryError::TooMany);
        }
        self.running.insert(key);
        Ok(())
    }

    /// Reading `page` ended at `now` with `outcome`.
    pub fn end(&mut self, page: &Address, outcome: &Result<Gallery, GalleryError>, now: u64) {
        let key = page.as_str().to_string();
        self.running.remove(&key);
        if let Err(why) = outcome {
            if why.is_miss() {
                if self.misses.len() >= MAX_MISSES {
                    if let Some(oldest) = self.misses.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| k.clone()) {
                        self.misses.remove(&oldest);
                    }
                }
                self.misses.insert(key, (now, why.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(url: &str, name: &str, ext: &str) -> Picture {
        Picture { url: url.into(), name: name.into(), extension: ext.into(), ..Picture::default() }
    }

    #[test]
    fn only_web_pages_are_read() {
        assert_eq!(
            gallery_page(" https://commons.example/wiki/Category:Cats ").unwrap().as_str(),
            "https://commons.example/wiki/Category:Cats"
        );
        assert_eq!(gallery_page("ftp://a/b"), Err(GalleryError::NotWebPage("ftp://a/b".into())));
        assert!(matches!(gallery_page("javascript:alert(1)"), Err(GalleryError::NotWebPage(_))));
        assert!(matches!(gallery_page("file:///etc/passwd"), Err(GalleryError::NotWebPage(_))));
    }

    #[test]
    fn a_gallery_keeps_each_web_picture_once() {
        let g = Gallery {
            title: "  Cats  ".into(),
            pictures: vec![
                picture("https://img.example/a.jpg", "a", "JPG"),
                picture("ytdl:https://video.example/1", "v", "mp4"),
                picture("text:hello", "t", "txt"),
                picture("https://img.example/a.jpg", "again", "jpg"),
                picture("https://img.example/b", "b", "../../x"),
            ],
        }
        .cleaned();
        assert_eq!(g.title, "Cats");
        let urls: Vec<&str> = g.pictures.iter().map(|p| p.url.as_str()).collect();
        assert_eq!(urls, ["https://img.example/a.jpg", "https://img.example/b"]);
        assert_eq!(g.pictures[0].extension, "jpg");
        assert_eq!(g.pictures[1].extension, "", "an odd extension is dropped");
        let many = Gallery {
            title: String::new(),
            pictures: (0..MAX_PICTURES + 3).map(|i| picture(&format!("https://i/{i}.png"), "", "")).collect(),
        };
        assert_eq!(many.cleaned().pictures.len(), MAX_PICTURES);
    }

    #[test]
    fn pictures_are_named_safely() {
        assert_eq!(picture("https://i/x.jpg", "Cat: on a mat", "jpg").file_name(1), "Cat_ on a mat.jpg");
        assert_eq!(picture("https://i/photo.png?size=big", "", "").file_name(2), "photo.png");
        assert_eq!(picture("https://i/", "", "webp").file_name(3), "picture 3.webp");
        assert_eq!(picture("https://i/", "", "").file_name(4), "picture 4");
        assert_eq!(picture("https://i/a", "../..", "jpg").file_name(5), "_...jpg");
        let sized = Picture { width: Some(800), height: Some(600), ..picture("https://i/a", "a", "jpg") };
        assert_eq!(sized.dimensions().as_deref(), Some("800 × 600"));
        assert_eq!(picture("https://i/a", "a", "jpg").dimensions(), None);
    }

    #[test]
    fn the_filter_matches_every_word_of_the_name() {
        let g = Gallery {
            title: String::new(),
            pictures: vec![
                picture("https://i/1", "Red cat", "jpg"),
                picture("https://i/2", "Blue cat", "png"),
                picture("https://i/3", "Red dog", "jpg"),
            ],
        };
        assert_eq!(g.matching(""), [0, 1, 2]);
        assert_eq!(g.matching("red"), [0, 2]);
        assert_eq!(g.matching("CAT red"), [0]);
        assert_eq!(g.matching("png"), [1]);
    }

    #[test]
    fn ticked_pictures_go_to_one_folder_with_names_apart() {
        let g = Gallery {
            title: "Cats".into(),
            pictures: vec![
                picture("https://i/1/a.jpg", "a", "jpg"),
                picture("https://i/2/a.jpg", "A", "JPG"),
                picture("https://i/3/c.jpg", "c", "jpg"),
            ],
        }
        .cleaned();
        let folder = Path::new("/home/me/Downloads/Cats");
        let items = g.downloads(&[2, 1, 0, 1, 7], folder, "https://commons.example/cats");
        let names: Vec<&str> = items.iter().filter_map(|d| d.filename.as_deref()).collect();
        assert_eq!(names, ["a.jpg", "A (1).jpg", "c.jpg"]);
        assert!(items.iter().all(|d| d.folder.as_deref() == Some(folder.display().to_string().as_str())));
        assert!(items.iter().all(|d| d.referrer.as_deref() == Some("https://commons.example/cats")));
        assert_eq!(items[2].url.as_str(), "https://i/3/c.jpg");
        assert!(items.iter().all(|d| d.media.is_none() && d.start.is_none()));
        assert!(g.downloads(&[0], folder, "about:blank")[0].referrer.is_none());
    }

    #[test]
    fn a_gallery_gets_a_folder_of_its_own() {
        let base = Path::new("/d");
        assert_eq!(gallery_folder(base, "Cats: the best"), base.join("Cats_ the best"));
        assert_eq!(gallery_folder(base, "  "), base.to_path_buf());
        assert_eq!(gallery_folder(base, ".."), base.to_path_buf());
    }

    #[test]
    fn a_page_is_read_once_at_a_time_and_a_miss_waits() {
        let page = |s: &str| Address::parse(s).unwrap();
        let (a, b, c) = (page("https://a/"), page("https://b/"), page("https://c/"));
        let mut reads = GalleryReads::default();
        reads.begin(&a, 0).unwrap();
        assert_eq!(reads.begin(&a, 0), Err(GalleryError::Busy));
        reads.begin(&b, 0).unwrap();
        assert_eq!(reads.begin(&c, 0), Err(GalleryError::TooMany));
        reads.end(&a, &Err(GalleryError::NothingFound), 10);
        assert_eq!(reads.begin(&a, 20), Err(GalleryError::NothingFound), "a miss is kept");
        reads.begin(&c, 20).unwrap();
        reads.end(&c, &Err(GalleryError::TimedOut(55)), 30);
        reads.begin(&c, 31).expect("a timeout is no miss");
        reads.end(&c, &Ok(Gallery::default()), 32);
        reads.end(&b, &Err(GalleryError::Missing), 32);
        reads.begin(&b, 33).expect("gallery-dl may be installed since");
        reads.begin(&a, 10 + MISS_KEPT_SECS).expect("the miss is forgotten after a while");
    }

    #[test]
    fn errors_keep_their_wire_shape() {
        assert_eq!(serde_json::to_value(GalleryError::TimedOut(55)).unwrap(), serde_json::json!({"TimedOut": 55}));
        assert_eq!(serde_json::to_value(GalleryError::Missing).unwrap(), serde_json::json!("Missing"));
        let g = Gallery { title: "T".into(), pictures: vec![picture("https://i/a", "a", "jpg")] };
        assert_eq!(
            serde_json::to_value(&g).unwrap(),
            serde_json::json!({"title": "T", "pictures": [{"url": "https://i/a", "name": "a", "extension": "jpg"}]})
        );
    }
}
