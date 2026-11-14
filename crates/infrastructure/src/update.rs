//! Checking GitHub for a newer UDM release, and the file attached to it
//! that installs it here. The window shows a notice with a link to the
//! release page; Update now has the daemon download that file, check it
//! against the release's signed checksums, and install it
//! (`update_in_place`). `GitHub` is the adapter for `ReleaseSource`.
//!
//! The release workflow lists every file's SHA-256 in `SHA256SUMS` and
//! signs the list with minisign (`installer/sign-checksums.js`). UDM pins
//! the public key, `release-key.pub`, and installs nothing the signed list
//! does not name with the same checksum, so a file swapped on GitHub, or a
//! release made without the key, is refused.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use udm_domain::bytes::ByteCount;

use tracing::info;
use udm_domain::download::Download;
use udm_domain::ports::ReleaseSource;
use udm_domain::update::UpdateError;

use crate::connect::{config_for, NetState};
use crate::engine;
use crate::live_settings::LiveSettings;

use udm_domain::tools::listed_sha256;
use udm_domain::update::{Install, Installer, Release, SetupAsset, SignedChecksums, CHECKSUMS, CHECKSUMS_SIGNATURE};

/// The public half of UDM's release key, in minisign's format.
pub const RELEASE_KEY: &str = include_str!("release-key.pub");

/// The largest checksum list or signature UDM reads.
const MAX_CHECKSUMS_BYTES: ByteCount = ByteCount::kib(64);

/// The releases of UDM's repository, newest first by GitHub's reckoning.
pub const LATEST_RELEASE: &str = "https://api.github.com/repos/tajbinkhan/UDM/releases/latest";

/// Where release files are served from; a Setup anywhere else is not used.
const DOWNLOADS: &str = "https://github.com/tajbinkhan/UDM/releases/download/";
/// The largest Setup UDM downloads.
pub const MAX_SETUP_BYTES: ByteCount = ByteCount::mib(256);

/// Where a release serves the asset called `name`, if it is served from the
/// repository's releases.
fn asset_url<'a>(assets: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    assets.as_array()?.iter().find_map(|a| {
        let url = a["browser_download_url"].as_str()?;
        (a["name"].as_str()? == name && url.starts_with(DOWNLOADS)).then_some(url)
    })
}

/// The `installer` asset among a release's `assets`, for a machine whose
/// processor is `arch`, if there is one UDM can use: named as that kind
/// is, served from the repository's releases, and of a sensible size.
fn setup_asset(assets: &serde_json::Value, installer: Installer, arch: &str) -> Option<SetupAsset> {
    let checksums = match (asset_url(assets, CHECKSUMS), asset_url(assets, CHECKSUMS_SIGNATURE)) {
        (Some(list), Some(signature)) => Some(SignedChecksums { list: list.into(), signature: signature.into() }),
        _ => None,
    };
    assets.as_array()?.iter().find_map(|a| {
        let name = a["name"].as_str()?;
        let url = a["browser_download_url"].as_str()?;
        let size = ByteCount::new(a["size"].as_u64()?);
        let usable = installer.matches(name, arch)
            && url.starts_with(DOWNLOADS)
            && (ByteCount::new(1)..=MAX_SETUP_BYTES).contains(&size);
        let sha256 = a["digest"]
            .as_str()
            .and_then(|d| d.strip_prefix("sha256:"))
            .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_ascii_lowercase);
        usable.then(|| SetupAsset {
            name: name.to_string(),
            url: url.to_string(),
            size,
            sha256,
            installer,
            checksums: checksums.clone(),
        })
    })
}

/// Why GitHub's answer or a downloaded Setup is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReleaseError {
    /// What the JSON parser said.
    #[error("GitHub's answer is not JSON: {0}")]
    NotJson(String),
    #[error("GitHub's answer names no release.")]
    NoRelease,
    /// The bytes downloaded and the size GitHub lists.
    #[error("The update is {0} bytes, not the {1} GitHub lists.")]
    WrongSize(ByteCount, ByteCount),
    /// What the file should have been.
    #[error("The update is not {0}.")]
    NotProgram(&'static str),
    #[error("The update does not match the checksum GitHub lists.")]
    WrongChecksum,
    #[error("This release's checksums are not signed, so UDM will not install it.")]
    Unsigned,
    /// What minisign said.
    #[error("The signature on this release's checksums is not UDM's ({0}), so UDM will not install it.")]
    BadSignature(String),
    #[error("This release's signed checksums do not list the update, so UDM will not install it.")]
    NotListed,
    #[error("The update does not match this release's signed checksums.")]
    WrongSignedChecksum,
}

/// Whether `bytes` start (or end) as an `installer` file does, and if not,
/// what it should have been.
fn looks_like(installer: Installer, bytes: &[u8]) -> Result<(), ReleaseError> {
    let (ok, what) = match installer {
        Installer::WindowsSetup => (bytes.starts_with(b"MZ"), "a Windows program"),
        // A UDIF disk image ends with a 512-byte trailer that starts "koly".
        Installer::MacDiskImage => {
            (bytes.len() >= 512 && bytes[bytes.len() - 512..].starts_with(b"koly"), "a macOS disk image")
        }
        // An ELF program with the AppImage type 2 mark at byte 8.
        Installer::AppImage => {
            (bytes.starts_with(b"\x7fELF") && bytes.get(8..11) == Some(b"AI\x02".as_slice()), "an AppImage")
        }
        Installer::LinuxTar => (bytes.starts_with(&[0x1f, 0x8b]), "a gzip archive"),
    };
    if ok {
        Ok(())
    } else {
        Err(ReleaseError::NotProgram(what))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A release's `SHA256SUMS` and the text of its minisign signature.
pub struct Signed<'a> {
    pub list: &'a [u8],
    pub signature: &'a str,
}

/// Check a downloaded update against what the release says about it: its
/// size, its kind, and its SHA-256 on the release's list of checksums,
/// which must carry a good signature by `key` (minisign's public key
/// format). GitHub's own checksum, when it lists one, must match too.
pub fn check_setup(asset: &SetupAsset, bytes: &[u8], signed: Option<Signed>, key: &str) -> Result<(), ReleaseError> {
    let got = ByteCount::new(bytes.len() as u64);
    if got != asset.size {
        return Err(ReleaseError::WrongSize(got, asset.size));
    }
    looks_like(asset.installer, bytes)?;
    let signed = signed.ok_or(ReleaseError::Unsigned)?;
    verify_list(&signed, key)?;
    let list = std::str::from_utf8(signed.list).map_err(|_| ReleaseError::NotListed)?;
    let want = listed_sha256(list, &asset.name).ok_or(ReleaseError::NotListed)?;
    let got = sha256_hex(bytes);
    if got != want {
        return Err(ReleaseError::WrongSignedChecksum);
    }
    if asset.sha256.as_ref().is_some_and(|github| *github != got) {
        return Err(ReleaseError::WrongChecksum);
    }
    Ok(())
}

/// Whether `signed.signature` is a good signature of `signed.list` by
/// `key`. Only minisign's prehashed signatures are taken.
fn verify_list(signed: &Signed, key: &str) -> Result<(), ReleaseError> {
    let bad = |e: minisign_verify::Error| ReleaseError::BadSignature(e.to_string());
    let key = minisign_verify::PublicKey::decode(key).map_err(bad)?;
    let signature = minisign_verify::Signature::decode(signed.signature).map_err(bad)?;
    key.verify(signed.list, &signature, false).map_err(bad)
}

/// The release in GitHub's answer for the latest release, with the file
/// that updates a copy installed as `install` on a machine whose processor
/// is `arch`.
pub fn parse_release(json: &[u8], install: Install, arch: &str) -> Result<Release, ReleaseError> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|e| ReleaseError::NotJson(e.to_string()))?;
    let tag = v["tag_name"].as_str().ok_or(ReleaseError::NoRelease)?;
    let url = v["html_url"].as_str().unwrap_or("https://github.com/tajbinkhan/UDM/releases");
    // Only a link to GitHub is ever shown.
    let url = if url.starts_with("https://github.com/") { url } else { "https://github.com/tajbinkhan/UDM/releases" };
    Ok(Release {
        version: tag.trim().trim_start_matches(['v', 'V']).to_string(),
        url: url.to_string(),
        setup: install.installer().and_then(|kind| setup_asset(&v["assets"], kind, arch)),
        package_manager: install == Install::LinuxPackage,
    })
}

/// This copy's version.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// GitHub's releases at `url`, reached through the settings' proxy, for
/// this copy as it was installed, checked against UDM's release key.
pub struct GitHub {
    settings: Arc<LiveSettings>,
    retries: Arc<NetState>,
    url: String,
    install: Install,
    key: &'static str,
}

impl GitHub {
    pub fn new(settings: Arc<LiveSettings>, retries: Arc<NetState>, url: &str) -> Self {
        let install = crate::update_in_place::this_install();
        GitHub { settings, retries, url: url.to_string(), install, key: RELEASE_KEY }
    }

    /// The same, for a copy installed as `install`, trusting `key` instead
    /// of UDM's release key: for tests with a test key.
    pub fn with(mut self, install: Install, key: &'static str) -> Self {
        self.install = install;
        self.key = key;
        self
    }

    /// Fetch `url` through the proxy the settings choose, up to `limit` bytes.
    fn fetch(&self, url: &str, headers: &[(String, String)], limit: ByteCount) -> Result<Vec<u8>, engine::EngineError> {
        let settings = self.settings.get();
        let net = config_for(&Download { url: url.into(), ..Download::default() }, &settings, &self.retries);
        engine::fetch(url, headers, &net, limit).map(|page| page.body)
    }
}

impl ReleaseSource for GitHub {
    fn latest(&self) -> Result<Release, UpdateError> {
        let headers = [
            ("User-Agent".to_string(), format!("UDM/{VERSION}")),
            ("Accept".to_string(), "application/vnd.github+json".to_string()),
        ];
        let body =
            self.fetch(&self.url, &headers, ByteCount::mib(1)).map_err(|e| UpdateError::Unreachable(e.to_string()))?;
        parse_release(&body, self.install, std::env::consts::ARCH).map_err(|e| UpdateError::Answer(e.to_string()))
    }

    fn fetch_setup(&self, setup: &SetupAsset) -> Result<PathBuf, UpdateError> {
        let headers = [("User-Agent".to_string(), format!("UDM/{VERSION}"))];
        let get = |url: &str, limit| self.fetch(url, &headers, limit).map_err(|e| UpdateError::Download(e.to_string()));
        // The signed list first: a release without one is refused before
        // its installer is downloaded.
        let checksums =
            setup.checksums.as_ref().ok_or_else(|| UpdateError::BadSetup(ReleaseError::Unsigned.to_string()))?;
        let list = get(&checksums.list, MAX_CHECKSUMS_BYTES)?;
        let signature = get(&checksums.signature, MAX_CHECKSUMS_BYTES)?;
        let signed = Signed { list: &list, signature: &String::from_utf8_lossy(&signature) };
        info!(url = %setup.url, "downloading the update");
        let body = get(&setup.url, MAX_SETUP_BYTES)?;
        check_setup(setup, &body, Some(signed), self.key).map_err(|e| UpdateError::BadSetup(e.to_string()))?;
        // A fresh folder only this user can write to, so nothing can swap
        // the checked file before it is installed.
        let dir = crate::paths::update_dir()
            .map_err(|e| UpdateError::Save { path: "update".into(), reason: e.to_string() })?;
        let path = dir.join(&setup.name);
        crate::update_in_place::save_private(&dir, &path, &body)
            .map_err(|e| UpdateError::Save { path: path.display().to_string(), reason: e.to_string() })?;
        Ok(path)
    }

    fn run_setup(&self, setup: &SetupAsset, path: &Path) -> Result<(), UpdateError> {
        crate::update_in_place::install(setup.installer, path)
            .map_err(|e| UpdateError::Start { path: path.display().to_string(), reason: e.to_string() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testserver::TestServer;
    use std::collections::HashMap;
    use udm_domain::settings::Settings;

    /// A throwaway key made with `installer/sign-checksums.js keygen` for
    /// these tests, the list it signed, and the signature.
    const TEST_KEY: &str = "untrusted comment: minisign public key 0D8A3690E4BCB7BC\n\
        RWS8t7zkkDaKDQzPHF4XUAM+gjUOqQyR1M9SnAIawhiHDWEDcwJ86d62\n";
    const LIST: &str = "574ce2739035aaff515080c231b4fb9ed9103174d63e201caec23d3d9a657dfc  UDM-Setup-0.6.0.exe\n\
        d9298a10d1b0735837dc4bd85dac641b0f3cef27a47e5d53a54f2f3f5b2fcffa  udm-0.6.0-linux-x86_64.tar.gz\n";
    const SIGNATURE: &str = "untrusted comment: signature from UDM's release key\n\
        RUS8t7zkkDaKDRE4b4iHy5xG3IDrKbTvH+UbvbmKOGXjsLvFGBKLNljk8mjA1bR7Jixvf+D2DsFKJ9G5ylglqY3ILTc80O7nDgc=\n\
        trusted comment: timestamp:1794652006\tfile:SHA256SUMS\thashed\n\
        HW3QQ4rVJRPI1qaAyXnK9IpjJjj1ckbDHm4Gzyc23ym1ygLpHyUcs8xzae2/dQWtGOXUCngdDznt/KCigTEfAA==\n";
    /// The Setup the list names.
    const SETUP: &[u8] = b"MZ setup";

    fn github(url: &str) -> GitHub {
        GitHub::new(Arc::new(LiveSettings::new(Settings::default(), None)), Arc::default(), url)
            .with(Install::WindowsSetup, TEST_KEY)
    }

    fn setup(url: &str, checksums: Option<SignedChecksums>) -> SetupAsset {
        SetupAsset {
            name: "UDM-Setup-0.6.0.exe".into(),
            url: url.into(),
            size: ByteCount::new(SETUP.len() as u64),
            sha256: None,
            installer: Installer::WindowsSetup,
            checksums,
        }
    }

    fn signed() -> Option<Signed<'static>> {
        Some(Signed { list: LIST.as_bytes(), signature: SIGNATURE })
    }

    #[test]
    fn the_latest_release_is_read_from_the_server() {
        let answer = br#"{"tag_name":"v9.0.0","html_url":"https://github.com/tajbinkhan/UDM/releases/tag/v9.0.0"}"#;
        let server = TestServer::start(answer.to_vec());
        let release = github(&server.url).latest().unwrap();
        assert_eq!(release.version, "9.0.0");
        assert!(release.setup.is_none());
    }

    #[test]
    fn a_server_that_is_not_there_is_unreachable() {
        let gone = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}/latest", l.local_addr().unwrap())
        };
        assert!(matches!(github(&gone).latest(), Err(UpdateError::Unreachable(_))));
    }

    #[test]
    fn a_signed_update_is_downloaded_and_saved() {
        let files = HashMap::from([
            ("/UDM-Setup-0.6.0.exe".to_string(), SETUP.to_vec()),
            ("/SHA256SUMS".to_string(), LIST.as_bytes().to_vec()),
            ("/SHA256SUMS.minisig".to_string(), SIGNATURE.as_bytes().to_vec()),
        ]);
        let server = TestServer::start_files(files);
        let sums = SignedChecksums {
            list: format!("{}/SHA256SUMS", server.url),
            signature: format!("{}/SHA256SUMS.minisig", server.url),
        };
        let asset = setup(&format!("{}/UDM-Setup-0.6.0.exe", server.url), Some(sums.clone()));
        let path = github(&server.url).fetch_setup(&asset).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), SETUP);
        let _ = std::fs::remove_file(&path);

        // The same files, checked against another key, are refused.
        let other = GitHub::new(Arc::new(LiveSettings::new(Settings::default(), None)), Arc::default(), &server.url)
            .with(
                Install::WindowsSetup,
                "untrusted comment: minisign public key\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\n",
            );
        assert!(matches!(other.fetch_setup(&asset), Err(UpdateError::BadSetup(_))));
    }

    #[test]
    fn a_release_without_signed_checksums_is_refused_before_downloading() {
        let server = TestServer::start(SETUP.to_vec());
        let asset = setup(&server.url, None);
        match github(&server.url).fetch_setup(&asset) {
            Err(UpdateError::BadSetup(why)) => assert_eq!(why, ReleaseError::Unsigned.to_string()),
            other => panic!("{other:?}"),
        }
        assert_eq!(server.served.load(std::sync::atomic::Ordering::Relaxed), 0, "nothing was downloaded");
    }

    #[test]
    fn an_update_must_be_on_the_signed_list() {
        let asset = setup("u", None);
        assert_eq!(check_setup(&asset, SETUP, signed(), TEST_KEY), Ok(()));
        assert_eq!(check_setup(&asset, SETUP, None, TEST_KEY), Err(ReleaseError::Unsigned));
        assert_eq!(
            check_setup(&asset, b"MZ setuP", signed(), TEST_KEY),
            Err(ReleaseError::WrongSignedChecksum),
            "the same size, other bytes"
        );
        let unlisted = SetupAsset { name: "UDM-Setup-0.6.1.exe".into(), ..asset.clone() };
        assert_eq!(check_setup(&unlisted, SETUP, signed(), TEST_KEY), Err(ReleaseError::NotListed));

        // A list changed after signing.
        let forged = LIST.replace("574c", "474c");
        let forged = Some(Signed { list: forged.as_bytes(), signature: SIGNATURE });
        assert!(matches!(check_setup(&asset, SETUP, forged, TEST_KEY), Err(ReleaseError::BadSignature(_))));
        let garbage = Some(Signed { list: LIST.as_bytes(), signature: "not a signature" });
        assert!(matches!(check_setup(&asset, SETUP, garbage, TEST_KEY), Err(ReleaseError::BadSignature(_))));

        // GitHub's own checksum must agree as well.
        let github_says = SetupAsset { sha256: Some("0".repeat(64)), ..asset.clone() };
        assert_eq!(check_setup(&github_says, SETUP, signed(), TEST_KEY), Err(ReleaseError::WrongChecksum));
        assert_eq!(
            check_setup(&asset, b"MZ", signed(), TEST_KEY),
            Err(ReleaseError::WrongSize(ByteCount::new(2), ByteCount::new(8)))
        );
    }

    #[test]
    fn the_pinned_key_reads() {
        assert!(minisign_verify::PublicKey::decode(RELEASE_KEY).is_ok());
    }

    #[test]
    fn each_kind_of_update_must_look_like_one() {
        assert_eq!(looks_like(Installer::WindowsSetup, b"MZ.."), Ok(()));
        assert_eq!(looks_like(Installer::WindowsSetup, b"XX.."), Err(ReleaseError::NotProgram("a Windows program")));
        let mut dmg = vec![0u8; 2048];
        dmg[2048 - 512..2048 - 508].copy_from_slice(b"koly");
        assert_eq!(looks_like(Installer::MacDiskImage, &dmg), Ok(()));
        assert!(looks_like(Installer::MacDiskImage, &dmg[..1000]).is_err());
        assert_eq!(looks_like(Installer::AppImage, b"\x7fELF\x02\x01\x01\x00AI\x02...."), Ok(()));
        assert!(looks_like(Installer::AppImage, b"\x7fELF\x02\x01\x01\x00\x00\x00\x00....").is_err(), "a plain ELF");
        assert_eq!(looks_like(Installer::LinuxTar, &[0x1f, 0x8b, 8]), Ok(()));
        assert!(looks_like(Installer::LinuxTar, b"PK\x03\x04").is_err());
    }

    fn release_json(assets: &str) -> String {
        format!(
            r#"{{"tag_name":"v0.6.0","html_url":"https://github.com/tajbinkhan/UDM/releases/tag/v0.6.0","assets":[{assets}]}}"#
        )
    }

    fn asset(name: &str, size: usize) -> String {
        format!(
            r#"{{"name":"{name}","browser_download_url":"https://github.com/tajbinkhan/UDM/releases/download/v0.6.0/{name}","size":{size}}}"#
        )
    }

    #[test]
    fn releases_are_read_from_githubs_answer() {
        let r = parse_release(
            br#"{"tag_name":"v0.4.1","html_url":"https://github.com/tajbinkhan/UDM/releases/tag/v0.4.1"}"#,
            Install::WindowsSetup,
            "x86_64",
        )
        .unwrap();
        assert_eq!(r.version, "0.4.1");
        assert!(r.url.ends_with("/v0.4.1"));
        let elsewhere =
            parse_release(br#"{"tag_name":"v9","html_url":"https://evil.example/x"}"#, Install::WindowsSetup, "x86_64")
                .unwrap();
        assert_eq!(elsewhere.url, "https://github.com/tajbinkhan/UDM/releases");
        assert!(parse_release(b"{}", Install::WindowsSetup, "x86_64").is_err());
        assert!(parse_release(b"not json", Install::WindowsSetup, "x86_64").is_err());
        assert_eq!(r.setup, None);
    }

    #[test]
    fn each_install_finds_its_file_and_the_signed_list() {
        let json = release_json(
            &[
                asset("UDM-Setup-0.6.0.exe", 8),
                asset("UDM-0.6.0-macos.dmg", 9),
                asset("UDM-0.6.0-x86_64.AppImage", 10),
                asset("udm-0.6.0-linux-x86_64.tar.gz", 11),
                asset("udm_0.6.0_amd64.deb", 12),
                asset("SHA256SUMS", 200),
                asset("SHA256SUMS.minisig", 300),
            ]
            .join(","),
        );
        let read = |install| parse_release(json.as_bytes(), install, "x86_64").unwrap();
        let windows = read(Install::WindowsSetup).setup.unwrap();
        assert_eq!((windows.name.as_str(), windows.installer), ("UDM-Setup-0.6.0.exe", Installer::WindowsSetup));
        let sums = windows.checksums.unwrap();
        assert!(sums.list.ends_with("/v0.6.0/SHA256SUMS") && sums.signature.ends_with("/v0.6.0/SHA256SUMS.minisig"));
        assert_eq!(read(Install::MacApp).setup.unwrap().name, "UDM-0.6.0-macos.dmg");
        assert_eq!(read(Install::AppImage).setup.unwrap().name, "UDM-0.6.0-x86_64.AppImage");
        assert_eq!(read(Install::LinuxTar).setup.unwrap().installer, Installer::LinuxTar);
        let package = read(Install::LinuxPackage);
        assert!(package.setup.is_none() && package.package_manager);
        assert!(read(Install::Other).setup.is_none());
        assert!(parse_release(json.as_bytes(), Install::AppImage, "aarch64").unwrap().setup.is_none());

        // Without both checksum files, the setup is still offered, and
        // refused when it is fetched.
        let bare = release_json(&asset("UDM-Setup-0.6.0.exe", 8));
        let setup = parse_release(bare.as_bytes(), Install::WindowsSetup, "x86_64").unwrap().setup.unwrap();
        assert_eq!(setup.checksums, None);

        let elsewhere = json.replace(
            "https://github.com/tajbinkhan/UDM/releases/download/v0.6.0/UDM-Setup",
            "https://evil.example/UDM-Setup",
        );
        assert_eq!(
            parse_release(elsewhere.as_bytes(), Install::WindowsSetup, "x86_64").unwrap().setup,
            None,
            "only from the repository's releases"
        );
    }

    #[test]
    fn githubs_digest_is_kept() {
        let json = release_json(
            r#"{"name":"UDM-Setup-0.6.0.exe","browser_download_url":"https://github.com/tajbinkhan/UDM/releases/download/v0.6.0/UDM-Setup-0.6.0.exe","size":8,"digest":"sha256:574CE2739035AAFF515080C231B4FB9ED9103174D63E201CAEC23D3D9A657DFC"}"#,
        );
        let setup = parse_release(json.as_bytes(), Install::WindowsSetup, "x86_64").unwrap().setup.unwrap();
        assert_eq!(setup.sha256.as_deref(), Some("574ce2739035aaff515080c231b4fb9ed9103174d63e201caec23d3d9a657dfc"));
        assert_eq!(check_setup(&setup, SETUP, signed(), TEST_KEY), Ok(()));
    }
}
