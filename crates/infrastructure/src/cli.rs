//! IDM's command-line switches for `udm-gui`:
//!
//! ```text
//! udm-gui /d URL [/p folder] [/f file_name] [/c checksum] [/q] [/h] [/n] [/a]
//! udm-gui /s
//! ```
//!
//! `/d` downloads URL, `/p` and `/f` set its folder and file name, `/c` gives
//! a checksum to compare the file with when it finishes (UDM's own, not
//! IDM's; `sha256:` and the like or a bare hex digest), `/a` adds
//! it to the main queue without starting it, `/n` asks no questions, and `/q`
//! exits UDM once it finishes. `/s` starts the main queue. `/h` (hang up the
//! dial-up connection) is accepted and does nothing. As in IDM, `/p`, `/f`,
//! `/c`, `/q`, `/h`, `/n` and `/a` need `/d`; without it they are ignored.
//!
//! `/d` takes magnet links too. A magnet link or a `.torrent` file given
//! alone, as a system hands a program the links and files it opens,
//! downloads that torrent as `/d` does.
//!
//! Each switch also has a `-x` and a long form (`--download`, `--path`,
//! `--file`, `--checksum`, `--quit`, `--hang-up`, `--silent`, `--add`, `--start-queue`), and
//! the slash forms take either case.
//!
//! `parse_sent` reads the JSON another program hands to `udm-host send`:
//! one link with what IDM's `SendLinkToIDM2` takes, or a list of links with
//! one referer, as `SendLinksArray` takes them. The README shows its fields.

use std::path::Path;

use serde::Deserialize;

use udm_domain::checksum::{Checksum, ChecksumError};
use udm_domain::download::CommandLine;

use udm_domain::download::{Download, DownloadLogin, PageLink};
use udm_domain::settings::{Duplicate, FileExists};

/// Why a command line or a sent link cannot be used. `udm-host send`
/// prints the message; the window shows it.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// The switch, and what it needs after it.
    #[error("{0} needs {1}")]
    NeedsValue(String, &'static str),
    #[error("Unknown command-line argument {0}")]
    Unknown(String),
    #[error("{0} is not an http, https or ftp address")]
    NotDownloadAddress(String),
    /// The name, and how the caller gives the folder instead.
    #[error("{0} is not a file name; give the folder with {1}")]
    NotFileName(String, &'static str),
    #[error("not valid JSON: {0}")]
    NotJson(#[source] serde_json::Error),
    #[error("expected a JSON object with \"url\" or \"links\"")]
    NotObject,
    #[error("bad link list: {0}")]
    BadList(#[source] serde_json::Error),
    #[error("bad link: {0}")]
    BadLink(#[source] serde_json::Error),
    #[error("a password needs a user")]
    PasswordWithoutUser,
    #[error("the link list is empty")]
    EmptyList,
    #[error("{0} is not an http or https address")]
    NotWebAddress(String),
    #[error("{0} is not a checksum: {1}")]
    BadChecksum(String, #[source] ChecksumError),
    #[error("too many links: {0}, at most {max}", max = udm_domain::batch::MAX_BATCH)]
    TooManyLinks(usize),
    /// The `.torrent` file, and why it cannot be used.
    #[error("{0} is not a torrent UDM can add: {1}")]
    BadTorrent(String, String),
}

/// Read `args` (without the program name). `Ok(None)` means no switch asks
/// for anything, so the window opens as usual. `cwd` resolves a relative
/// `/p` folder.
pub fn parse<I, S>(args: I, cwd: &Path) -> Result<Option<CommandLine>, CliError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut url = None;
    let mut metainfo = None;
    let mut folder = None;
    let mut filename = None;
    let mut checksum = None;
    let (mut quit, mut silent, mut add, mut start_queue) = (false, false, false, false);

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let arg = arg.as_ref();
        let mut value = |what: &'static str| {
            args.next()
                .map(|v| v.as_ref().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| CliError::NeedsValue(arg.to_string(), what))
        };
        match switch(arg) {
            Some('d') => url = Some(value("an address")?),
            Some('p') => folder = Some(value("a folder")?),
            Some('f') => filename = Some(value("a file name")?),
            Some('c') => checksum = Some(value("a checksum")?),
            Some('q') => quit = true,
            Some('h') => {}
            Some('n') => silent = true,
            Some('a') => add = true,
            Some('s') => start_queue = true,
            Some('m' | 't') => {}
            None if url.is_none() && udm_domain::torrent::starts_like_magnet(arg) => url = Some(arg.to_string()),
            None if url.is_none() && is_torrent_path(arg) => {
                let (link, bytes) = torrent_file(arg, cwd)?;
                url = Some(link);
                metainfo = Some(bytes);
            }
            _ => return Err(CliError::Unknown(arg.to_string())),
        }
    }

    let download = match url {
        None => None,
        Some(url) => {
            let mut d = download(url, folder, filename, silent, add, cwd, "/p")?;
            d.checksum = checksum.map(|c| parse_checksum(&c)).transpose()?;
            Some(d)
        }
    };
    let quit_after = quit && download.is_some();
    if download.is_none() && !start_queue {
        return Ok(None);
    }
    Ok(Some(CommandLine { download, start_queue, quit_after, metainfo }))
}

/// Whether `arg` names a `.torrent` file rather than a switch.
fn is_torrent_path(arg: &str) -> bool {
    Path::new(arg).extension().is_some_and(|e| e.eq_ignore_ascii_case("torrent"))
}

/// The `.torrent` file at `arg` (under `cwd` when relative): its torrent's
/// magnet link and the file's bytes, which go to UDM, which keeps them.
fn torrent_file(arg: &str, cwd: &Path) -> Result<(String, Vec<u8>), CliError> {
    use std::io::Read;
    let bad = |why: String| CliError::BadTorrent(arg.to_string(), why);
    let max = udm_domain::torrent::MAX_SENT_METAINFO_BYTES as u64;
    let file = std::fs::File::open(cwd.join(arg)).map_err(|e| bad(e.to_string()))?;
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes).map_err(|e| bad(e.to_string()))?;
    // It travels to UDM in one message, which has a size limit.
    if bytes.len() as u64 > max {
        let size = udm_domain::bytes::ByteCount::new(max).megabytes();
        return Err(bad(format!("it is larger than {size}; add it from its web address instead")));
    }
    let meta = crate::torrent::metainfo::Metainfo::read(&bytes).map_err(|e| bad(e.to_string()))?;
    Ok((meta.magnet().to_uri(), bytes))
}

/// The checksum a caller gave, which must be one UDM can check.
fn parse_checksum(text: &str) -> Result<Checksum, CliError> {
    Checksum::parse(text).map_err(|e| CliError::BadChecksum(text.to_string(), e))
}

/// A download of `url` into `folder` (relative to `cwd`) as `filename`.
/// `silent` asks nothing (IDM's `/n`) and `add` only adds it to the main
/// queue (`/a`). `folder_hint` names how the caller gives the folder, for
/// the error about a path given as the name.
fn download(
    url: String,
    folder: Option<String>,
    filename: Option<String>,
    silent: bool,
    add: bool,
    cwd: &Path,
    folder_hint: &'static str,
) -> Result<Download, CliError> {
    if !is_download_url(&url) {
        return Err(CliError::NotDownloadAddress(url));
    }
    let folder = folder.map(|f| {
        let f = Path::new(&f);
        if f.is_absolute() {
            f.to_path_buf()
        } else {
            cwd.join(f)
        }
    });
    if let Some(name) = &filename {
        // An empty name is left to the usual naming, as before.
        if !name.is_empty() && udm_domain::filename::FileName::parse(name).is_err() {
            return Err(CliError::NotFileName(name.clone(), folder_hint));
        }
    }
    Ok(Download {
        url: url.into(),
        folder: folder.map(|f| f.display().to_string()),
        filename,
        start: add.then_some(false),
        ask: silent.then_some(false),
        duplicate: silent.then_some(Duplicate::NewName),
        file_exists: silent.then_some(FileExists::Rename),
        ..Download::default()
    })
}

/// What another program sends with `udm-host send`: one link to add, or
/// links for the link picker.
#[derive(Debug)]
// It lives only while it is sent, as `protocol::Incoming` does.
#[allow(clippy::large_enum_variant)]
pub enum Sent {
    /// One link, added as the browser extension adds one.
    One(Download),
    /// Links to list in the link picker, all from the page `referer`, which
    /// is empty when none was given.
    Many { referer: String, links: Vec<PageLink> },
}

/// IDM's `SendLinkToIDM` flag that adds a link without asking anything.
const FLAG_SILENT: u32 = 1;
/// IDM's `SendLinkToIDM` flag that adds a link to the queue only.
const FLAG_ADD_ONLY: u32 = 2;

/// One link, with what IDM's `SendLinkToIDM2` takes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OneLink {
    url: String,
    #[serde(default, alias = "referrer")]
    referer: Option<String>,
    #[serde(default)]
    cookies: Option<String>,
    /// A form body, sent with a POST.
    #[serde(default)]
    post_data: Option<String>,
    /// The form body's type; a form's usual one when missing.
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    user_agent: Option<String>,
    /// A checksum to compare the file with when it finishes.
    #[serde(default)]
    checksum: Option<String>,
    /// Add it with no dialog.
    #[serde(default)]
    silent: bool,
    /// Add it to the main queue without starting it.
    #[serde(default)]
    add_only: bool,
    /// IDM's flags: 1 is `silent`, 2 is `add_only`.
    #[serde(default)]
    flags: u32,
}

/// Links for the picker, with what IDM's `SendLinksArray` takes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkList {
    #[serde(default, alias = "referrer")]
    referer: Option<String>,
    links: Vec<ListedLink>,
}

/// A link in a list: its address alone, or an object.
#[derive(Deserialize)]
#[serde(untagged)]
enum ListedLink {
    Url(String),
    Link(LinkObject),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkObject {
    url: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    cookies: Option<String>,
}

/// The form body's type when the sender gives none.
const FORM_TYPE: &str = "application/x-www-form-urlencoded";

/// Read what a program sent to `udm-host send`: a JSON object with `url`
/// for one link, or with `links` for the picker. `cwd` resolves a relative
/// folder. An empty string counts as a missing field, as IDM's COM calls
/// pass empty strings for what they leave out.
pub fn parse_sent(json: &str, cwd: &Path) -> Result<Sent, CliError> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(CliError::NotJson)?;
    if !value.is_object() {
        return Err(CliError::NotObject);
    }
    if value.get("links").is_some() {
        let list: LinkList = serde_json::from_value(value).map_err(CliError::BadList)?;
        return many(list);
    }
    let one: OneLink = serde_json::from_value(value).map_err(CliError::BadLink)?;
    one_link(one, cwd)
}

fn given(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.trim().is_empty())
}

fn one_link(l: OneLink, cwd: &Path) -> Result<Sent, CliError> {
    let silent = l.silent || l.flags & FLAG_SILENT != 0;
    let add = l.add_only || l.flags & FLAG_ADD_ONLY != 0;
    let mut d = download(l.url.trim().to_string(), given(l.folder), given(l.filename), silent, add, cwd, "\"folder\"")?;
    d.referrer = given(l.referer);
    d.cookies = given(l.cookies);
    d.user_agent = given(l.user_agent);
    d.checksum = given(l.checksum).map(|c| parse_checksum(&c)).transpose()?;
    if let Some(body) = l.post_data {
        use base64::Engine as _;
        d.method = Some("POST".into());
        d.body = Some(base64::engine::general_purpose::STANDARD.encode(body.as_bytes()));
        let kind = given(l.content_type).unwrap_or_else(|| FORM_TYPE.to_string());
        d.headers.insert("Content-Type".into(), kind);
    }
    match (given(l.user), l.password) {
        (Some(username), password) => {
            d.login = Some(DownloadLogin { username, password: Some(password.unwrap_or_default()) })
        }
        (None, Some(p)) if !p.is_empty() => return Err(CliError::PasswordWithoutUser),
        (None, _) => {}
    }
    Ok(Sent::One(d))
}

fn many(list: LinkList) -> Result<Sent, CliError> {
    if list.links.is_empty() {
        return Err(CliError::EmptyList);
    }
    let links = list
        .links
        .into_iter()
        .map(|l| {
            let link = match l {
                ListedLink::Url(url) => PageLink { url, ..PageLink::default() },
                ListedLink::Link(o) => PageLink { url: o.url, text: o.text, cookies: given(o.cookies), image: false },
            };
            let url = link.url.trim().to_string();
            if !udm_domain::batch::is_web_url(&url) || url::Url::parse(&url).is_err() {
                return Err(CliError::NotWebAddress(url));
            }
            Ok(PageLink { url, ..link })
        })
        .collect::<Result<Vec<_>, CliError>>()?;
    if links.len() > udm_domain::batch::MAX_BATCH {
        return Err(CliError::TooManyLinks(links.len()));
    }
    Ok(Sent::Many { referer: given(list.referer).unwrap_or_default(), links })
}

/// The flag that starts the window in the tray, with no window until one is
/// wanted; the window starts a new copy of itself with it when it closes to
/// the tray on Linux.
pub const IN_TRAY: &str = "--in-tray";

/// The letter of an IDM switch: `/d`, `/D`, `-d` and `--download` are all
/// `d`. `--minimized`, the autostart flag, is `m`, and `--in-tray`, which
/// the window starts itself with when it closes to the tray, is `t`.
fn switch(arg: &str) -> Option<char> {
    let long = match arg {
        "--download" => 'd',
        "--path" => 'p',
        "--file" => 'f',
        "--checksum" => 'c',
        "--quit" => 'q',
        "--hang-up" => 'h',
        "--silent" => 'n',
        "--add" => 'a',
        "--start-queue" => 's',
        "--minimized" => 'm',
        IN_TRAY => 't',
        _ => {
            let mut chars = arg.chars();
            let (Some('/' | '-'), Some(c), None) = (chars.next(), chars.next(), chars.next()) else {
                return None;
            };
            let c = c.to_ascii_lowercase();
            return "dpfcqhnas".contains(c).then_some(c);
        }
    };
    Some(long)
}

fn is_download_url(s: &str) -> bool {
    if udm_domain::torrent::is_magnet(s) {
        return true;
    }
    let Ok(u) = url::Url::parse(s) else {
        return false;
    };
    matches!(u.scheme(), "http" | "https" | "ftp" | "ftps") && u.host_str().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cwd() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    fn run(args: &[&str]) -> Result<Option<CommandLine>, CliError> {
        parse(args, &cwd())
    }

    #[test]
    fn a_magnet_link_or_torrent_file_alone_is_downloaded() {
        let magnet = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=x";
        let c = run(&[magnet]).unwrap().unwrap();
        assert_eq!((c.download.unwrap().url.as_str(), c.metainfo), (magnet, None));
        let dir = std::env::temp_dir().join(format!("udm_gui_torrent_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut info = b"d6:lengthi10e4:name5:a.iso12:piece lengthi16384e6:pieces20:".to_vec();
        info.extend_from_slice(&[1; 20]);
        info.push(b'e');
        let bytes = crate::torrent::metainfo::build(&info, &[]);
        std::fs::write(dir.join("a.torrent"), &bytes).unwrap();
        let c = parse(["a.torrent", "/n"], &dir).unwrap().unwrap();
        let d = c.download.unwrap();
        assert!(d.url.as_str().starts_with("magnet:?xt=urn:btih:") && d.ask == Some(false));
        assert_eq!(c.metainfo, Some(bytes));
        std::fs::write(dir.join("big.torrent"), vec![b'd'; udm_domain::torrent::MAX_SENT_METAINFO_BYTES + 1]).unwrap();
        assert!(
            matches!(parse(["big.torrent"], &dir), Err(CliError::BadTorrent(_, why)) if why.contains("web address"))
        );
        std::fs::write(dir.join("b.torrent"), b"<html>").unwrap();
        assert!(matches!(parse(["b.torrent"], &dir), Err(CliError::BadTorrent(..))));
        assert!(matches!(run(&["notes.txt"]), Err(CliError::Unknown(_))), "only torrents are taken alone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_switches_open_the_window_as_usual() {
        assert!(run(&[]).unwrap().is_none());
        assert!(run(&["--minimized"]).unwrap().is_none());
        assert!(run(&[IN_TRAY]).unwrap().is_none());
    }

    #[test]
    fn d_adds_a_download_with_its_folder_and_name() {
        let folder = cwd().join("dl");
        let c = run(&["/d", "https://example.com/a.zip", "/p", &folder.display().to_string(), "/f", "b.zip"])
            .unwrap()
            .unwrap();
        let d = c.download.unwrap();
        assert_eq!(d.url, "https://example.com/a.zip");
        assert_eq!(d.folder.as_deref(), Some(folder.display().to_string().as_str()));
        assert_eq!(d.filename.as_deref(), Some("b.zip"));
        assert_eq!(d.start, None, "it starts");
        assert_eq!(d.ask, None, "the setting decides on the dialog");
        assert!(!c.start_queue && !c.quit_after);
    }

    #[test]
    fn a_relative_folder_is_under_the_current_one() {
        let c = run(&["/d", "https://example.com/a", "/p", "sub"]).unwrap().unwrap();
        assert_eq!(c.download.unwrap().folder, Some(cwd().join("sub").display().to_string()));
    }

    #[test]
    fn n_asks_nothing_and_a_only_adds() {
        let d = run(&["/n", "/a", "/d", "http://example.com/a"]).unwrap().unwrap().download.unwrap();
        assert_eq!(d.ask, Some(false));
        assert_eq!(d.duplicate, Some(Duplicate::NewName));
        assert_eq!(d.file_exists, Some(FileExists::Rename));
        assert_eq!(d.start, Some(false));
    }

    #[test]
    fn switches_take_any_case_and_every_form() {
        for d in ["/d", "/D", "-d", "--download"] {
            let c = run(&[d, "ftp://example.com/a", "/Q", "--silent"]).unwrap().unwrap();
            assert!(c.quit_after);
            assert_eq!(c.download.unwrap().ask, Some(false));
        }
    }

    #[test]
    fn s_starts_the_queue_alone_or_with_a_download() {
        let c = run(&["/s"]).unwrap().unwrap();
        assert!(c.start_queue && c.download.is_none());
        let c = run(&["/d", "https://example.com/a", "/s"]).unwrap().unwrap();
        assert!(c.start_queue && c.download.is_some());
    }

    #[test]
    fn switches_that_need_d_are_ignored_without_it() {
        assert!(run(&["/q", "/n", "/a", "/h", "/f", "x"]).unwrap().is_none());
        assert!(!run(&["/q", "/s"]).unwrap().unwrap().quit_after);
    }

    #[test]
    fn c_gives_a_checksum_to_check_when_it_finishes() {
        let hex = "d41d8cd98f00b204e9800998ecf8427e";
        for c in ["/c", "/C", "-c", "--checksum"] {
            let d = run(&["/d", "https://example.com/a", c, hex]).unwrap().unwrap().download.unwrap();
            assert_eq!(d.checksum.unwrap().to_string(), format!("md5:{hex}"));
        }
        assert!(matches!(run(&["/d", "https://example.com/a", "/c", "nope"]), Err(CliError::BadChecksum(..))));
        assert!(run(&["/c", hex]).unwrap().is_none(), "it needs /d");
    }

    #[test]
    fn a_sent_link_can_carry_a_checksum() {
        let json = r#"{"url": "https://example.com/a", "checksum": "sha1:da39a3ee5e6b4b0d3255bfef95601890afd80709"}"#;
        let Sent::One(d) = parse_sent(json, &cwd()).unwrap() else { panic!("one link") };
        assert_eq!(d.checksum.unwrap().algorithm(), udm_domain::checksum::Algorithm::Sha1);
        let json = r#"{"url": "https://example.com/a", "checksum": "abc"}"#;
        assert!(matches!(parse_sent(json, &cwd()), Err(CliError::BadChecksum(..))));
    }

    #[test]
    fn h_is_accepted_and_does_nothing() {
        let c = run(&["/d", "https://example.com/a", "/h"]).unwrap().unwrap();
        assert!(!c.quit_after && !c.start_queue);
    }

    #[test]
    fn bad_input_is_refused() {
        assert!(run(&["/d"]).is_err(), "no address");
        assert!(run(&["/d", "file:///c:/x"]).is_err(), "not a web address");
        assert!(run(&["/d", "example.com/a"]).is_err(), "no scheme");
        assert!(run(&["/d", "https://example.com/a", "/f", "..\\x"]).is_err(), "a path as the name");
        assert!(run(&["/x"]).is_err(), "unknown switch");
        assert!(run(&["https://example.com/a"]).is_err(), "a bare address");
    }

    fn sent(json: &str) -> Result<Sent, CliError> {
        parse_sent(json, &cwd())
    }

    fn one(json: &str) -> Download {
        match sent(json).unwrap() {
            Sent::One(d) => d,
            other => panic!("expected one link, got {other:?}"),
        }
    }

    #[test]
    fn one_link_carries_everything_send_link_to_idm_takes() {
        use base64::Engine as _;
        let folder = cwd().join("dl");
        let json = serde_json::json!({
            "url": "https://example.com/get",
            "referer": "https://example.com/page",
            "cookies": "a=1; b=2",
            "post_data": "id=7&x=y",
            "user": "me",
            "password": "secret",
            "folder": folder.display().to_string(),
            "filename": "file.zip",
            "user_agent": "Tool/1.0",
        });
        let d = one(&json.to_string());
        assert_eq!(d.url, "https://example.com/get");
        assert_eq!(d.referrer.as_deref(), Some("https://example.com/page"));
        assert_eq!(d.cookies.as_deref(), Some("a=1; b=2"));
        assert_eq!(d.method.as_deref(), Some("POST"));
        let body = base64::engine::general_purpose::STANDARD.decode(d.body.unwrap()).unwrap();
        assert_eq!(body, b"id=7&x=y");
        assert_eq!(d.headers.get("Content-Type").map(String::as_str), Some(FORM_TYPE));
        let login = d.login.unwrap();
        assert_eq!((login.username.as_str(), login.password.as_deref()), ("me", Some("secret")));
        assert_eq!(d.folder.as_deref(), Some(folder.display().to_string().as_str()));
        assert_eq!(d.filename.as_deref(), Some("file.zip"));
        assert_eq!(d.user_agent.as_deref(), Some("Tool/1.0"));
        // No flags: the settings decide on the dialog, and it starts.
        assert_eq!((d.ask, d.start), (None, None));
    }

    #[test]
    fn a_bare_link_is_a_get_with_nothing_else() {
        let d = one(r#"{"url":"https://example.com/a.zip","referrer":"","cookies":"","user":"","folder":""}"#);
        assert_eq!(d.method, None);
        assert!(d.headers.is_empty());
        assert_eq!((d.referrer, d.cookies, d.login.is_none(), d.folder), (None, None, true, None));
    }

    #[test]
    fn the_flags_skip_the_dialog_or_only_add() {
        for json in [r#"{"url":"https://e.com/a","silent":true}"#, r#"{"url":"https://e.com/a","flags":1}"#] {
            let d = one(json);
            assert_eq!(d.ask, Some(false));
            assert_eq!(d.duplicate, Some(Duplicate::NewName));
            assert_eq!(d.file_exists, Some(FileExists::Rename));
            assert_eq!(d.start, None);
        }
        for json in [r#"{"url":"https://e.com/a","add_only":true}"#, r#"{"url":"https://e.com/a","flags":2}"#] {
            let d = one(json);
            assert_eq!((d.start, d.ask), (Some(false), None));
        }
        let d = one(r#"{"url":"https://e.com/a","flags":3}"#);
        assert_eq!((d.start, d.ask), (Some(false), Some(false)));
    }

    #[test]
    fn a_post_may_name_its_body_type() {
        let d = one(r#"{"url":"https://e.com/a","post_data":"{}","content_type":"application/json"}"#);
        assert_eq!(d.headers.get("Content-Type").map(String::as_str), Some("application/json"));
        // An empty body is still a POST.
        assert_eq!(one(r#"{"url":"https://e.com/a","post_data":""}"#).method.as_deref(), Some("POST"));
    }

    #[test]
    fn a_relative_folder_is_under_the_senders_folder() {
        assert_eq!(
            one(r#"{"url":"https://e.com/a","folder":"sub"}"#).folder,
            Some(cwd().join("sub").display().to_string())
        );
    }

    #[test]
    fn a_list_opens_the_picker_with_one_referer() {
        let json = r#"{"referer":"https://e.com/page","links":[
            "https://e.com/a.zip",
            {"url":"https://e.com/b.zip","text":"B","cookies":"c=1"}
        ]}"#;
        let Sent::Many { referer, links } = sent(json).unwrap() else { panic!("expected a list") };
        assert_eq!(referer, "https://e.com/page");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0], PageLink { url: "https://e.com/a.zip".into(), ..PageLink::default() });
        assert_eq!(
            links[1],
            PageLink { url: "https://e.com/b.zip".into(), text: "B".into(), cookies: Some("c=1".into()), image: false }
        );
        // The referer may be left out.
        let Sent::Many { referer, .. } = sent(r#"{"links":["http://e.com/a"]}"#).unwrap() else { panic!() };
        assert_eq!(referer, "");
    }

    #[test]
    fn bad_sends_are_refused() {
        assert!(sent("not json").is_err());
        assert!(sent(r#"["https://e.com/a"]"#).is_err(), "not an object");
        assert!(sent(r#"{"referer":"https://e.com/"}"#).is_err(), "no url");
        assert!(sent(r#"{"url":"file:///c:/x"}"#).is_err(), "not a web address");
        assert!(sent(r#"{"url":"https://e.com/a","filename":"..\\x"}"#).is_err(), "a path as the name");
        assert!(sent(r#"{"url":"https://e.com/a","password":"p"}"#).is_err(), "a password with no user");
        assert!(sent(r#"{"url":"https://e.com/a","refferer":"x"}"#).is_err(), "a misspelled field");
        assert!(sent(r#"{"links":[]}"#).is_err(), "an empty list");
        assert!(sent(r#"{"links":["ftp://e.com/a"]}"#).is_err(), "the picker takes web links only");
        assert!(sent(r#"{"links":[{"url":"https://e.com/a","size":1}]}"#).is_err(), "an unknown link field");
        assert!(sent(r#"{"links":["https://e.com/a"],"silent":true}"#).is_err(), "flags are for one link");
    }
}
