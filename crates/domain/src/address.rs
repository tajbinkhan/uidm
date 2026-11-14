//! Download addresses. `Address` is one UDM can download from: it starts
//! with `http://`, `https://`, `ftp://` or `ftps://` (in any case), or it is
//! a magnet link with a BitTorrent info hash (`torrent::MagnetLink`), the
//! same check the daemon makes before it takes a download
//! (`batch::is_download_url`) and no stricter, so every address accepted
//! before is accepted now. The text is kept as given, so the files and the
//! wire carry it unchanged.
//!
//! `Link` is the address a download names, as it was given: an `Address`
//! when it is one, else the text. Requests from clients, the list's rows
//! and the history file carry a `Link`, so a client's request for
//! `javascript:` can still be decoded and refused by name, and a row saved
//! with an address UDM does not take (by hand, or by an older version) is
//! still loaded and shown instead of failing the whole file. Starting such
//! a row fails where an `Address` is needed. Both serialize as the plain
//! string.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Why an address cannot be used: in a playlist or manifest, or as a
/// download's address.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddressError {
    /// The address, and why it does not parse.
    #[error("bad address {0}: {1}")]
    Bad(String, String),
    /// The scheme of an address that is not http or https.
    #[error("{0}: addresses are not supported in a stream")]
    Scheme(String),
    /// Not an http, https or ftp address (or a magnet link), so nothing
    /// can be downloaded from it.
    #[error("{0} is not an http, https or ftp address")]
    NotDownload(String),
}

/// An http, https, ftp or ftps address, as given.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Address(String);

impl Address {
    /// `text` as a download address: it must start with `http://`,
    /// `https://`, `ftp://` or `ftps://`, in any case. Nothing is trimmed or
    /// normalized.
    pub fn parse(text: &str) -> Result<Address, AddressError> {
        Address::try_from(text.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    /// Whether it is an http or https address, not ftp, ftps or a magnet
    /// link.
    pub fn is_web(&self) -> bool {
        crate::batch::is_web_url(&self.0)
    }

    /// The magnet link it is, for a torrent download.
    pub fn magnet(&self) -> Option<crate::torrent::MagnetLink> {
        crate::torrent::starts_like_magnet(&self.0).then(|| crate::torrent::MagnetLink::parse(&self.0).ok()).flatten()
    }

    /// Whether it is a torrent's magnet link.
    pub fn is_torrent(&self) -> bool {
        crate::torrent::starts_like_magnet(&self.0)
    }
}

impl TryFrom<String> for Address {
    type Error = AddressError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        if crate::batch::is_download_url(&text) {
            Ok(Address(text))
        } else {
            Err(AddressError::NotDownload(text))
        }
    }
}

impl From<Address> for String {
    fn from(a: Address) -> Self {
        a.0
    }
}

impl AsRef<str> for Address {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq<str> for Address {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Address {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// A download's address as it was given: an [`Address`], or text that is
/// not one. Made from any text, it is an `Address` exactly when
/// [`Address::parse`] takes the text.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct Link(Given);

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Given {
    Address(Address),
    /// Text `Address::parse` refuses; never a valid address.
    Other(String),
}

impl Link {
    /// `text`, an `Address` when it parses as one.
    pub fn new(text: impl Into<String>) -> Link {
        Link::from(text.into())
    }

    /// The address, when it is one UDM can download from.
    pub fn address(&self) -> Option<&Address> {
        match &self.0 {
            Given::Address(a) => Some(a),
            Given::Other(_) => None,
        }
    }

    /// The address, or why it is not one.
    pub fn to_address(&self) -> Result<Address, AddressError> {
        match &self.0 {
            Given::Address(a) => Ok(a.clone()),
            Given::Other(text) => Err(AddressError::NotDownload(text.clone())),
        }
    }

    pub fn is_address(&self) -> bool {
        self.address().is_some()
    }

    /// The text as given.
    pub fn as_str(&self) -> &str {
        match &self.0 {
            Given::Address(a) => a.as_str(),
            Given::Other(text) => text,
        }
    }

    pub fn into_string(self) -> String {
        match self.0 {
            Given::Address(a) => a.into_string(),
            Given::Other(text) => text,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.as_str().is_empty()
    }
}

/// No address at all, as a request made in code starts.
impl Default for Link {
    fn default() -> Self {
        Link(Given::Other(String::new()))
    }
}

impl From<String> for Link {
    fn from(text: String) -> Self {
        if crate::batch::is_download_url(&text) {
            Link(Given::Address(Address(text)))
        } else {
            Link(Given::Other(text))
        }
    }
}

impl From<&str> for Link {
    fn from(text: &str) -> Self {
        Link::from(text.to_string())
    }
}

impl From<Address> for Link {
    fn from(a: Address) -> Self {
        Link(Given::Address(a))
    }
}

impl From<Link> for String {
    fn from(l: Link) -> Self {
        l.into_string()
    }
}

impl AsRef<str> for Link {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Shown as the plain text, as a `String` was.
impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl PartialEq<str> for Link {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Link {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<Address> for Link {
    fn eq(&self, other: &Address) -> bool {
        self.as_str() == other.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_addresses_are_http_https_or_ftp_in_any_case() {
        for ok in [
            "http://x/a",
            "https://x/a",
            "ftp://f.net/c.iso",
            "ftps://f.net/c.iso",
            "HTTPS://X/A",
            "Ftp://f/",
            "FTPS://f/",
            "http://",
            "https://a b",
        ] {
            let a = Address::parse(ok).unwrap();
            assert_eq!(a.as_str(), ok, "kept as given");
            assert!(crate::batch::is_download_url(ok));
        }
        let magnet = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=a";
        let a = Address::parse(magnet).unwrap();
        assert!(a.is_torrent() && !a.is_web() && a.magnet().is_some());
        assert!(!Address::parse("https://x/").unwrap().is_torrent());
        for bad in [
            "magnet:?dn=no-hash",
            "",
            "javascript:x",
            "file:///etc/passwd",
            " https://x/a",
            "mailto:a@b",
            "ftpx://x/",
            "ftps:/x",
            "x:y",
            "www.x.com",
        ] {
            assert_eq!(Address::parse(bad), Err(AddressError::NotDownload(bad.to_string())), "{bad}");
            assert!(!crate::batch::is_download_url(bad));
        }
        assert!(Address::parse("https://x/").unwrap().is_web());
        assert!(!Address::parse("ftp://x/").unwrap().is_web());
        assert!(!Address::parse("ftps://x/").unwrap().is_web());
    }

    #[test]
    fn a_link_is_an_address_exactly_when_it_parses() {
        let good = Link::new("https://x/a.zip");
        assert_eq!(good.address(), Some(&Address::parse("https://x/a.zip").unwrap()));
        let bad = Link::new("javascript:void(0)");
        assert_eq!((bad.address(), bad.as_str()), (None, "javascript:void(0)"));
        assert_eq!(bad.to_address(), Err(AddressError::NotDownload("javascript:void(0)".into())));
        assert!(Link::default().is_empty() && !Link::default().is_address());
        assert_eq!(Link::from(Address::parse("ftp://f/").unwrap()), Link::new("ftp://f/"));
    }

    #[test]
    fn both_are_plain_strings_in_json() {
        let a = Address::parse("https://x/a?b=1").unwrap();
        assert_eq!(serde_json::to_string(&a).unwrap(), r#""https://x/a?b=1""#);
        assert_eq!(serde_json::from_str::<Address>(r#""ftp://f/c""#).unwrap(), Address::parse("ftp://f/c").unwrap());
        assert!(serde_json::from_str::<Address>(r#""file:///x""#).is_err());
        // A link takes anything, and gives it back unchanged.
        for text in [r#""https://x/a""#, r#""file:///etc/passwd""#, r#""""#, r#""not an address""#] {
            let l: Link = serde_json::from_str(text).unwrap();
            assert_eq!(serde_json::to_string(&l).unwrap(), text);
        }
        assert!(serde_json::from_str::<Link>(r#""https://x/a""#).unwrap().is_address());
        assert!(!serde_json::from_str::<Link>(r#""file:///x""#).unwrap().is_address());
        assert_eq!(format!("{:?}", Link::new("https://x/")), r#""https://x/""#);
    }
}
