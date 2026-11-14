//! Installing yt-dlp and ffmpeg for video pages, and gallery-dl for image
//! galleries. `ForgeTools` reads a project's latest release from GitHub's
//! or Codeberg's API and downloads its files (`ToolReleases`); `ToolDir` is
//! UDM's own `tools` folder
//! (`paths::tools_dir`), which puts a checked download in place, lets it run
//! and remembers the release it came from (`ToolFolder`). The checking is the
//! `InstallTool` use case's: a download is only handed to `ToolDir` once its
//! SHA-256 matches the checksum file its release publishes.
//!
//! What is downloaded, and from where:
//! - yt-dlp: `https://github.com/yt-dlp/yt-dlp/releases/latest`, the
//!   program for this system (`yt-dlp.exe`, `yt-dlp_macos`, `yt-dlp_linux`,
//!   ...) and `SHA2-256SUMS`.
//! - ffmpeg on Windows and Linux: `https://github.com/yt-dlp/FFmpeg-Builds/
//!   releases/latest`, `ffmpeg-master-latest-<system>-gpl.zip` or `.tar.xz`
//!   and `checksums.sha256`. The archive is unpacked with the system's own
//!   `tar` (System32's on Windows 10 and later) and only `ffmpeg` and
//!   `ffprobe` are kept.
//! - ffmpeg on macOS: nothing. No build there publishes a checksum to check
//!   it against, so Options names Homebrew instead.
//! - gallery-dl on Windows (x64, x86) and Linux (x86-64):
//!   `https://codeberg.org/mikf/gallery-dl/releases/latest`, `gallery-dl.exe`,
//!   `gallery-dl_x86.exe` or `gallery-dl.bin`, and `SHA256SUMS`. Its project
//!   moved there from GitHub, whose releases no longer carry files. Nothing
//!   on macOS or ARM, where it builds no program; Options names pip or
//!   Homebrew instead.
//!
//! Both checksums come from the same release as the file, over HTTPS from
//! the forge, so they catch a damaged or swapped download, not a release its
//! project's account was made to publish. The GPG signatures beside them
//! (yt-dlp's `SHA2-256SUMS.sig`, gallery-dl's `SHA256SUMS.sig` and `.sig`
//! files) are not checked. Codeberg lists no digest of its own for a file,
//! so a gallery-dl download is checked against `SHA256SUMS` alone.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use udm_domain::bytes::ByteCount;
use udm_domain::download::Download;
use udm_domain::ports::{ReleaseFile, ToolFolder, ToolRelease, ToolReleases};
use udm_domain::sync::LockExt;
use udm_domain::tools::{Cpu, Forge, InstalledTool, Packing, Platform, System, Tool, ToolError};

use crate::connect::{config_for, NetState};
use crate::engine;
use crate::live_settings::LiveSettings;

/// GitHub's API for a repository's releases.
const API: &str = "https://api.github.com/repos/";
/// Where GitHub serves release files from; a file anywhere else is not used.
const DOWNLOADS: &str = "https://github.com/";
/// Codeberg's (Forgejo's) API for a repository's releases.
const CODEBERG_API: &str = "https://codeberg.org/api/v1/repos/";
/// Where Codeberg serves release files from.
const CODEBERG_DOWNLOADS: &str = "https://codeberg.org/";
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The system this copy of UDM is built for.
pub fn platform() -> Platform {
    let system = if cfg!(windows) {
        System::Windows
    } else if cfg!(target_os = "macos") {
        System::MacOs
    } else if cfg!(target_os = "linux") {
        System::Linux
    } else {
        System::Other
    };
    let cpu = if cfg!(target_arch = "x86_64") {
        Cpu::X86_64
    } else if cfg!(target_arch = "aarch64") {
        Cpu::Aarch64
    } else if cfg!(target_arch = "x86") {
        Cpu::X86
    } else {
        Cpu::Other
    };
    Platform { system, cpu }
}

/// A forge's release API and where it serves release files from.
struct Site {
    api: String,
    downloads: String,
}

/// Tools' releases on GitHub and Codeberg, reached through the settings'
/// proxy.
pub struct ForgeTools {
    settings: Arc<LiveSettings>,
    retries: Arc<NetState>,
    github: Site,
    codeberg: Site,
    /// Where downloads wait to be checked.
    folder: Option<PathBuf>,
}

impl ForgeTools {
    pub fn new(settings: Arc<LiveSettings>, retries: Arc<NetState>) -> Self {
        let folder = crate::paths::tools_dir().map(|d| d.join(".downloads"));
        ForgeTools {
            settings,
            retries,
            github: Site { api: API.into(), downloads: DOWNLOADS.into() },
            codeberg: Site { api: CODEBERG_API.into(), downloads: CODEBERG_DOWNLOADS.into() },
            folder,
        }
    }

    /// Both forges' API at `api`, files only from addresses starting with
    /// `downloads`, downloads kept in `folder`: for tests.
    pub fn at(settings: Arc<LiveSettings>, api: &str, downloads: &str, folder: PathBuf) -> Self {
        let site = || Site { api: api.into(), downloads: downloads.into() };
        ForgeTools { settings, retries: Arc::default(), github: site(), codeberg: site(), folder: Some(folder) }
    }

    fn site(&self, forge: Forge) -> &Site {
        match forge {
            Forge::GitHub => &self.github,
            Forge::Codeberg => &self.codeberg,
        }
    }

    fn net(&self, url: &str) -> crate::net::NetConfig {
        let settings = self.settings.get();
        config_for(&Download { url: url.into(), ..Download::default() }, &settings, &self.retries)
    }

    fn headers(json: bool) -> Vec<(String, String)> {
        let mut h = vec![("User-Agent".to_string(), format!("UDM/{VERSION}"))];
        if json {
            h.push(("Accept".to_string(), "application/vnd.github+json".to_string()));
        }
        h
    }
}

/// The release in GitHub's or Codeberg's answer for `repository`'s latest
/// release (both answer in GitHub's shape): its files served from
/// `downloads` + the repository's releases, with GitHub's digests. A release whose tag never changes (FFmpeg-Builds'
/// `latest`) is known by the day it was published.
pub fn parse_release(json: &[u8], repository: &str, downloads: &str) -> Result<ToolRelease, ToolError> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|e| ToolError::Answer(e.to_string()))?;
    let tag = v["tag_name"].as_str().ok_or_else(|| ToolError::Answer("no release".into()))?.trim();
    let version = match (tag, v["published_at"].as_str()) {
        ("latest", Some(at)) => at.chars().take(10).collect(),
        _ => tag.to_string(),
    };
    let prefix = format!("{downloads}{repository}/releases/download/");
    let files = v["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let name = a["name"].as_str()?;
            let url = a["browser_download_url"].as_str()?;
            let sha256 = a["digest"]
                .as_str()
                .and_then(|d| d.strip_prefix("sha256:"))
                .filter(|h| udm_domain::tools::is_sha256(h))
                .map(str::to_ascii_lowercase);
            let usable = url.starts_with(&prefix) && !name.contains(['/', '\\', ':']) && !name.starts_with('.');
            usable.then(|| ReleaseFile { name: name.into(), url: url.into(), sha256 })
        })
        .collect();
    Ok(ToolRelease { version, files })
}

impl ToolReleases for ForgeTools {
    fn latest(&self, forge: Forge, repository: &str) -> Result<ToolRelease, ToolError> {
        let site = self.site(forge);
        let url = format!("{}{repository}/releases/latest", site.api);
        let body = engine::fetch(&url, &Self::headers(forge == Forge::GitHub), &self.net(&url), ByteCount::mib(4))
            .map_err(|e| ToolError::Unreachable(e.to_string()))?
            .body;
        parse_release(&body, repository, &site.downloads)
    }

    fn text(&self, file: &ReleaseFile) -> Result<String, ToolError> {
        let body = engine::fetch(&file.url, &Self::headers(false), &self.net(&file.url), ByteCount::mib(1))
            .map_err(|e| ToolError::Download(e.to_string()))?
            .body;
        String::from_utf8(body).map_err(|_| ToolError::NoChecksum(file.name.clone()))
    }

    fn download(&self, file: &ReleaseFile, limit: ByteCount) -> Result<PathBuf, ToolError> {
        let folder = self.folder.clone().ok_or_else(|| no_folder(Path::new("tools")))?;
        fs::create_dir_all(&folder).map_err(|e| save_error(&folder, &e))?;
        // A name of its own, never the program's: nothing runs it here.
        let path = folder.join(format!("{}.download", file.name));
        tracing::info!(url = %file.url, "downloading a tool");
        engine::fetch_to_file(&file.url, &Self::headers(false), &self.net(&file.url), limit, &path)
            .map_err(|e| ToolError::Download(e.to_string()))?;
        Ok(path)
    }
}

fn save_error(path: &Path, e: &io::Error) -> ToolError {
    ToolError::Save { path: path.display().to_string(), reason: e.to_string() }
}

fn no_folder(path: &Path) -> ToolError {
    ToolError::Save { path: path.display().to_string(), reason: "UDM's data folder cannot be found".into() }
}

/// UDM's `tools` folder, and `tools.json` in it naming the release each
/// installed tool came from.
pub struct ToolDir {
    dir: Option<PathBuf>,
    settings: Arc<LiveSettings>,
    /// Keeps two changes of `tools.json` apart.
    manifest: Mutex<()>,
}

const MANIFEST: &str = "tools.json";

impl ToolDir {
    pub fn new(settings: Arc<LiveSettings>) -> Self {
        Self::at(crate::paths::tools_dir(), settings)
    }

    pub fn at(dir: Option<PathBuf>, settings: Arc<LiveSettings>) -> Self {
        ToolDir { dir, settings, manifest: Mutex::new(()) }
    }

    fn read(&self) -> BTreeMap<Tool, InstalledTool> {
        let Some(dir) = &self.dir else { return BTreeMap::new() };
        fs::read(dir.join(MANIFEST)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Change `tools.json` with `f`, writing it atomically.
    fn change(&self, f: impl FnOnce(&mut BTreeMap<Tool, InstalledTool>)) -> Result<(), ToolError> {
        let _held = self.manifest.lock_or_recover();
        let dir = self.dir.clone().ok_or_else(|| no_folder(Path::new(MANIFEST)))?;
        let mut tools = self.read();
        f(&mut tools);
        let path = dir.join(MANIFEST);
        let text = serde_json::to_vec_pretty(&tools)
            .map_err(|e| ToolError::Save { path: path.display().to_string(), reason: e.to_string() })?;
        let temp = dir.join(format!("{MANIFEST}.tmp"));
        fs::write(&temp, text).and_then(|()| fs::rename(&temp, &path)).map_err(|e| save_error(&path, &e))
    }

    /// The program `name` in the folder.
    fn program(&self, name: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(crate::platform::exe_name(name)))
    }
}

/// Let `path` run, for its owner and everyone else (as a package manager
/// installs programs). Windows runs any `.exe`.
fn make_executable(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Put `from` at `to`, replacing what is there. On Windows a running
/// program cannot be replaced, which fails here.
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    if cfg!(windows) && to.exists() {
        fs::remove_file(to)?;
    }
    fs::rename(from, to)
}

/// The system's `tar`, which reads zip and tar.xz: System32's own copy on
/// Windows (10 and later), not one found on the PATH.
fn tar() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()))
            .join("System32")
            .join("tar.exe")
    } else {
        PathBuf::from("tar")
    }
}

/// Unpack `archive` into the empty folder `into` with `tar`.
pub fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    let mut cmd = Command::new(tar());
    crate::platform::no_window(&mut cmd);
    let out = cmd
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("could not run tar: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&out.stderr);
    Err(why.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("tar failed").trim().to_string())
}

/// The file called `name` in `dir` or the folders under it, at most three
/// deep (an FFmpeg-Builds archive has `<build>/bin/ffmpeg`).
fn find_file(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_file() && entry.file_name() == name {
            return Some(entry.path());
        }
        if kind.is_dir() {
            dirs.push(entry.path());
        }
    }
    if depth == 0 {
        return None;
    }
    dirs.into_iter().find_map(|d| find_file(&d, name, depth - 1))
}

impl ToolFolder for ToolDir {
    fn installed(&self, tool: Tool) -> Option<InstalledTool> {
        let program = self.program(tool.name())?;
        program.is_file().then(|| self.read().remove(&tool)).flatten()
    }

    fn install(
        &self,
        tool: Tool,
        file: &Path,
        packing: Packing,
        version: &str,
        now: u64,
    ) -> Result<PathBuf, ToolError> {
        let dir = self.dir.clone().ok_or_else(|| no_folder(file))?;
        fs::create_dir_all(&dir).map_err(|e| save_error(&dir, &e))?;
        let program = dir.join(crate::platform::exe_name(tool.name()));
        let placed = match packing {
            Packing::Program => {
                let staged = dir.join(format!(".{}.new", tool.name()));
                fs::rename(file, &staged)
                    .and_then(|()| make_executable(&staged))
                    .and_then(|()| replace(&staged, &program))
                    .map_err(|e| {
                        let _ = fs::remove_file(&staged);
                        save_error(&program, &e)
                    })
            }
            Packing::Archive => {
                let unpacked = dir.join(format!(".{}.unpacked", tool.name()));
                let _ = fs::remove_dir_all(&unpacked);
                let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let result = fs::create_dir_all(&unpacked)
                    .map_err(|e| save_error(&unpacked, &e))
                    .and_then(|()| unpack(file, &unpacked).map_err(|why| ToolError::Unpack(name.clone(), why)))
                    .and_then(|()| {
                        // ffprobe comes with ffmpeg, and yt-dlp uses it too.
                        for wanted in [tool.name(), "ffprobe"] {
                            let exe = crate::platform::exe_name(wanted);
                            let Some(found) = find_file(&unpacked, &exe, 3) else {
                                if wanted == tool.name() {
                                    return Err(ToolError::Unpack(name.clone(), format!("{exe} is not in it")));
                                }
                                continue;
                            };
                            let to = dir.join(&exe);
                            make_executable(&found)
                                .and_then(|()| replace(&found, &to))
                                .map_err(|e| save_error(&to, &e))?;
                        }
                        Ok(())
                    });
                let _ = fs::remove_dir_all(&unpacked);
                let _ = fs::remove_file(file);
                result
            }
        };
        if let Err(e) = placed {
            let _ = fs::remove_file(file);
            return Err(e);
        }
        let installed = InstalledTool { version: version.to_string(), checked: now };
        self.change(|tools| {
            tools.insert(tool, installed);
        })?;
        Ok(program)
    }

    fn discard(&self, file: &Path) {
        let _ = fs::remove_file(file);
    }

    fn note_checked(&self, tool: Tool, now: u64) {
        let changed = self.change(|tools| {
            if let Some(t) = tools.get_mut(&tool) {
                t.checked = now;
            }
        });
        if let Err(e) = changed {
            tracing::warn!(error = %e, "could not note the tools' check");
        }
    }

    fn found(&self, tool: Tool) -> Option<PathBuf> {
        let settings = self.settings.get();
        let ffmpeg = crate::settings::ffmpeg(&settings);
        match tool {
            Tool::Ffmpeg => ffmpeg,
            Tool::YtDlp => crate::media::ytdlp::find(ffmpeg.as_deref()),
            Tool::GalleryDl => crate::media::gallerydl::find(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testserver::{self, TestServer};
    use std::collections::HashMap;
    use udm_application::tools::InstallTool;
    use udm_domain::settings::Settings;

    fn live() -> Arc<LiveSettings> {
        Arc::new(LiveSettings::new(Settings::default(), None))
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("udm_tools_{}_{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sha(bytes: &[u8]) -> String {
        use sha2::Digest as _;
        udm_domain::tools::hex(&sha2::Sha256::digest(bytes))
    }

    /// A fake GitHub on the local test server: the API's answer for
    /// `yt-dlp/yt-dlp`'s latest release, the program for this system and
    /// the checksum file listing `listed` for it.
    fn release_site(program: &[u8], listed: &str) -> TestServer {
        let name = udm_domain::tools::source(Tool::YtDlp, platform()).map_or("yt-dlp_linux", |s| s.asset).to_string();
        let program = program.to_vec();
        let listed = listed.to_string();
        TestServer::start_files_linking(move |base| {
            let dl = "/yt-dlp/yt-dlp/releases/download/2026.10.26";
            let file = |n: &str| serde_json::json!({"name": n, "browser_download_url": format!("{base}{dl}/{n}")});
            let answer = serde_json::json!({"tag_name": "2026.10.26", "assets": [file(&name), file("SHA2-256SUMS")]});
            HashMap::from([
                ("/yt-dlp/yt-dlp/releases/latest".to_string(), answer.to_string().into_bytes()),
                (format!("{dl}/{name}"), program),
                (format!("{dl}/SHA2-256SUMS"), format!("{listed}  {name}\n").into_bytes()),
            ])
        })
    }

    fn origin(server: &TestServer) -> String {
        format!("{}/", server.url)
    }

    #[test]
    fn a_release_lists_only_its_own_files() {
        let json = br#"{"tag_name":"latest","published_at":"2026-11-09T12:00:00Z","assets":[
            {"name":"checksums.sha256","browser_download_url":"https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/checksums.sha256"},
            {"name":"evil","browser_download_url":"https://evil.example/yt-dlp/FFmpeg-Builds/releases/download/latest/evil"},
            {"name":"ffmpeg.zip","browser_download_url":"https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/ffmpeg.zip","digest":"sha256:ABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABAB"}
        ]}"#;
        let r = parse_release(json, "yt-dlp/FFmpeg-Builds", DOWNLOADS).unwrap();
        assert_eq!(r.version, "2026-11-09");
        let names: Vec<&str> = r.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["checksums.sha256", "ffmpeg.zip"]);
        assert_eq!(r.files[1].sha256.as_deref(), Some("ab".repeat(32).as_str()));
        assert!(parse_release(b"{}", "a/b", DOWNLOADS).is_err());
        assert_eq!(parse_release(br#"{"tag_name":"v1"}"#, "a/b", DOWNLOADS).unwrap().version, "v1");
    }

    #[test]
    fn a_checked_program_is_installed_and_runs() {
        if udm_domain::tools::source(Tool::YtDlp, platform()).is_none() {
            return;
        }
        let program = b"#!/bin/sh\necho fake yt-dlp\n";
        let server = release_site(program, &sha(program));
        let dir = scratch("good");
        let releases = ForgeTools::at(live(), &origin(&server), &origin(&server), dir.join(".downloads"));
        let folder = ToolDir::at(Some(dir.clone()), live());
        let (version, path) = InstallTool::new(&releases, &folder, &crate::hash::DiskHashes, platform())
            .run(Tool::YtDlp, false, 7)
            .unwrap();
        assert_eq!(version, "2026.10.26");
        assert_eq!(fs::read(&path).unwrap(), program);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o111, 0o111);
        }
        assert_eq!(folder.installed(Tool::YtDlp), Some(InstalledTool { version, checked: 7 }));
        folder.note_checked(Tool::YtDlp, 9);
        assert_eq!(folder.installed(Tool::YtDlp).unwrap().checked, 9);
        assert!(fs::read_dir(dir.join(".downloads")).unwrap().next().is_none(), "the download moved");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_program_that_does_not_match_is_never_installed() {
        if udm_domain::tools::source(Tool::YtDlp, platform()).is_none() {
            return;
        }
        let server = release_site(b"tampered", &sha(b"the real one"));
        let dir = scratch("bad");
        let releases = ForgeTools::at(live(), &origin(&server), &origin(&server), dir.join(".downloads"));
        let folder = ToolDir::at(Some(dir.clone()), live());
        let err = InstallTool::new(&releases, &folder, &crate::hash::DiskHashes, platform())
            .run(Tool::YtDlp, false, 7)
            .unwrap_err();
        assert!(matches!(err, ToolError::WrongChecksum(_)), "{err}");
        assert!(!dir.join(crate::platform::exe_name("yt-dlp")).exists());
        assert!(fs::read_dir(dir.join(".downloads")).unwrap().next().is_none(), "the download was deleted");
        assert_eq!(folder.installed(Tool::YtDlp), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// gallery-dl from a fake Codeberg: its API answers in GitHub's shape
    /// without digests, and `SHA256SUMS` lists every program.
    #[test]
    fn gallery_dl_is_installed_from_codeberg_when_it_matches() {
        let Some(source) = udm_domain::tools::source(Tool::GalleryDl, platform()) else { return };
        let program = b"#!/bin/sh\necho fake gallery-dl\n".to_vec();
        let (name, listed) = (source.asset.to_string(), sha(&program));
        let server = TestServer::start_files_linking(move |base| {
            let dl = "/mikf/gallery-dl/releases/download/v1.32.19";
            let file = |n: &str| serde_json::json!({"name": n, "browser_download_url": format!("{base}{dl}/{n}")});
            let answer = serde_json::json!({"tag_name": "v1.32.19", "assets": [file(&name), file("SHA256SUMS"), file("SHA256SUMS.sig")]});
            let sums = format!("{listed}  {name}\n{}  gallery_dl-1.32.19.tar.gz\n", "0".repeat(64));
            HashMap::from([
                ("/mikf/gallery-dl/releases/latest".to_string(), answer.to_string().into_bytes()),
                (format!("{dl}/{name}"), program.clone()),
                (format!("{dl}/SHA256SUMS"), sums.into_bytes()),
            ])
        });
        let dir = scratch("gallerydl");
        let releases = ForgeTools::at(live(), &origin(&server), &origin(&server), dir.join(".downloads"));
        let folder = ToolDir::at(Some(dir.clone()), live());
        let (version, path) = InstallTool::new(&releases, &folder, &crate::hash::DiskHashes, platform())
            .run(Tool::GalleryDl, false, 5)
            .unwrap();
        assert_eq!(version, "v1.32.19");
        assert_eq!(path, dir.join(crate::platform::exe_name("gallery-dl")));
        assert_eq!(fs::read(&path).unwrap(), b"#!/bin/sh\necho fake gallery-dl\n");
        assert_eq!(folder.installed(Tool::GalleryDl).unwrap().version, "v1.32.19");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_from_elsewhere_are_not_downloaded() {
        let server = TestServer::start(testserver::body(10));
        let dir = scratch("elsewhere");
        let releases = ForgeTools::at(live(), &server.url, "https://github.com/", dir.join(".downloads"));
        // The test server answers anything with ten bytes, which is no JSON.
        assert!(matches!(releases.latest(Forge::GitHub, "yt-dlp/yt-dlp"), Err(ToolError::Answer(_))));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// An archive like FFmpeg-Builds' (`<build>/bin/ffmpeg`), made with the
    /// system's tar, unpacks to just the programs.
    #[test]
    fn an_archive_leaves_only_its_programs() {
        let dir = scratch("archive");
        let build = dir.join("src").join("ffmpeg-build").join("bin");
        fs::create_dir_all(&build).unwrap();
        fs::write(build.join(crate::platform::exe_name("ffmpeg")), b"ffmpeg").unwrap();
        fs::write(build.join(crate::platform::exe_name("ffprobe")), b"ffprobe").unwrap();
        fs::write(build.join("README.txt"), b"x").unwrap();
        let archive = dir.join("ffmpeg.tar.gz");
        let made =
            Command::new(tar()).arg("-czf").arg(&archive).arg("-C").arg(dir.join("src")).arg("ffmpeg-build").status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("no tar; skipped");
            return;
        }
        let tools = dir.join("tools");
        let folder = ToolDir::at(Some(tools.clone()), live());
        let path = folder.install(Tool::Ffmpeg, &archive, Packing::Archive, "2026-11-09", 3).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"ffmpeg");
        assert_eq!(fs::read(tools.join(crate::platform::exe_name("ffprobe"))).unwrap(), b"ffprobe");
        assert!(!tools.join("README.txt").exists() && !archive.exists());
        assert_eq!(folder.installed(Tool::Ffmpeg).unwrap().version, "2026-11-09");
        let broken = dir.join("broken.tar.gz");
        fs::write(&broken, b"not an archive").unwrap();
        assert!(matches!(folder.install(Tool::Ffmpeg, &broken, Packing::Archive, "x", 1), Err(ToolError::Unpack(..))));
        fs::remove_dir_all(&dir).unwrap();
    }
}
