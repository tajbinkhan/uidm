//! Many downloads at once: URLs from a pattern such as
//! `https://example.com/photo[001-120].jpg`, or from a pasted list.
//!
//! A pattern holds one or more ranges in square brackets:
//!
//! - `[1-20]` counts 1, 2, ... 20; `[001-120]` pads to the width of the first
//!   number (001, 002, ... 120);
//! - `[0-100:5]` counts in steps of 5;
//! - `[a-z]` and `[A-Z]` run through letters;
//! - a range may count down, as in `[10-1]`.
//!
//! With several ranges every combination is produced, the first range
//! changing slowest. Brackets that are not a range, such as an IPv6 host, are
//! kept as they are.
//!
//! IDM's own form is also understood (see [`expand_asterisk`]): one `*` in the
//! address, replaced by numbers or letters from From to To, with numbers
//! padded with zeros to a chosen wildcard size. People moving from IDM type
//! addresses this way, so the same list comes out for the same settings.

use crate::address::Address;

/// The most URLs one batch may produce, so a mistyped pattern cannot add a
/// million downloads.
pub const MAX_BATCH: usize = 10_000;

/// Which end of an IDM-style range a value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    From,
    To,
}

impl std::fmt::Display for Bound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Bound::From => "From",
            Bound::To => "To",
        })
    }
}

/// Why a pattern, an IDM-style address or a list makes no downloads. The
/// window shows these, translated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BatchError {
    /// A number in the range `[{0}]` does not fit.
    #[error("[{0}]: the number is too big")]
    NumberTooBig(String),
    #[error("[{0}]: the step is too big")]
    StepTooBig(String),
    #[error("[{0}]: the step must be at least 1")]
    StepZero(String),
    #[error("[{0}]: both letters must be the same case")]
    MixedCaseRange(String),
    #[error("The pattern must start with http://, https:// or ftp://.")]
    PatternNotWeb,
    #[error("The pattern makes {0} addresses; at most {max} can be added at once.", max = MAX_BATCH)]
    PatternTooBig(u64),
    #[error("The address must start with http://, https:// or ftp://.")]
    AddressNotWeb,
    #[error("The address must have a * where the numbers or letters go.")]
    NoAsterisk,
    #[error("The address may have only one *.")]
    TwoAsterisks,
    #[error("{0} must be a whole number.")]
    NotWholeNumber(Bound),
    #[error("{0} is too big.")]
    BoundTooBig(Bound),
    #[error("The wildcard size must be from 1 to {max}.", max = MAX_WILDCARD_SIZE)]
    WildcardSize,
    #[error("From ({0}) must not be greater than To ({1}).")]
    NumbersReversed(u64, u64),
    #[error("{0} must be one letter, a to z.")]
    NotOneLetter(Bound),
    #[error("From and To must be letters of the same case.")]
    MixedCaseLetters,
    #[error("From ({0}) must not be after To ({1}).")]
    LettersReversed(char, char),
    /// From, To and how many addresses they make.
    #[error("From {0} to {1} makes {2} addresses; at most {max} can be added at once.", max = MAX_BATCH)]
    RangeTooBig(String, String, u64),
    #[error("The list has {0} addresses; at most {max} can be added at once.", max = MAX_BATCH)]
    ListTooLong(usize),
}

/// One part of a pattern.
#[derive(Debug, Clone, PartialEq)]
enum Part {
    Text(String),
    Numbers { from: u64, to: u64, step: u64, width: usize },
    Letters { from: char, to: char },
}

impl Part {
    fn count(&self) -> u64 {
        match *self {
            Part::Text(_) => 1,
            Part::Numbers { from, to, step, .. } => (from.abs_diff(to) / step).saturating_add(1),
            Part::Letters { from, to } => (from as u64).abs_diff(to as u64) + 1,
        }
    }

    fn nth(&self, i: u64) -> String {
        match self {
            Part::Text(t) => t.clone(),
            Part::Numbers { from, to, step, width } => {
                let n = if to >= from { from + i * step } else { from - i * step };
                format!("{n:0width$}")
            }
            Part::Letters { from, to } => {
                let (f, t) = (*from as u32, *to as u32);
                let c = if t >= f { f + i as u32 } else { f - i as u32 };
                char::from_u32(c).map(String::from).unwrap_or_default()
            }
        }
    }
}

/// Read the range inside one pair of brackets, or `None` if it is not one.
fn parse_range(inner: &str) -> Result<Option<Part>, BatchError> {
    let (range, step) = match inner.split_once(':') {
        Some((r, s)) => (r, Some(s)),
        None => (inner, None),
    };
    let Some((a, b)) = range.split_once('-') else {
        return Ok(None);
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    if digits(a) && digits(b) {
        let from: u64 = a.parse().map_err(|_| BatchError::NumberTooBig(inner.to_string()))?;
        let to: u64 = b.parse().map_err(|_| BatchError::NumberTooBig(inner.to_string()))?;
        let step = match step {
            None => 1,
            Some(s) if digits(s) => s.parse().map_err(|_| BatchError::StepTooBig(inner.to_string()))?,
            Some(_) => return Ok(None),
        };
        if step == 0 {
            return Err(BatchError::StepZero(inner.to_string()));
        }
        let width = if a.len() > 1 && a.starts_with('0') { a.len() } else { 1 };
        return Ok(Some(Part::Numbers { from, to, step, width }));
    }
    let letter = |s: &str| {
        let mut chars = s.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_alphabetic() => Some(c),
            _ => None,
        }
    };
    if let (Some(from), Some(to), None) = (letter(a), letter(b), step) {
        if from.is_ascii_lowercase() != to.is_ascii_lowercase() {
            return Err(BatchError::MixedCaseRange(inner.to_string()));
        }
        return Ok(Some(Part::Letters { from, to }));
    }
    Ok(None)
}

fn parse(pattern: &str) -> Result<Vec<Part>, BatchError> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('[') {
        text.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            text.push_str(&rest[open..]);
            rest = "";
            break;
        };
        match parse_range(&after[..close])? {
            Some(range) => {
                if !text.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut text)));
                }
                parts.push(range);
            }
            None => text.push_str(&rest[open..open + close + 2]),
        }
        rest = &after[close + 1..];
    }
    text.push_str(rest);
    if !text.is_empty() {
        parts.push(Part::Text(text));
    }
    Ok(parts)
}

/// How many URLs `pattern` produces, without producing them.
pub fn count(pattern: &str) -> Result<u64, BatchError> {
    Ok(parse(pattern.trim())?.iter().fold(1u64, |n, p| n.saturating_mul(p.count())))
}

/// Every URL `pattern` produces, in order. Fails when the pattern is not a
/// web address or produces more than `MAX_BATCH`.
pub fn expand(pattern: &str) -> Result<Vec<Address>, BatchError> {
    let pattern = pattern.trim();
    if !is_download_url(pattern) {
        return Err(BatchError::PatternNotWeb);
    }
    let parts = parse(pattern)?;
    let total = parts.iter().fold(1u64, |n, p| n.saturating_mul(p.count()));
    if total > MAX_BATCH as u64 {
        return Err(BatchError::PatternTooBig(total));
    }
    let mut urls = Vec::with_capacity(total as usize);
    for mut i in 0..total {
        // Mixed-radix digits, the last part changing fastest.
        let mut pieces = vec![String::new(); parts.len()];
        for (k, part) in parts.iter().enumerate().rev() {
            let n = part.count();
            pieces[k] = part.nth(i % n);
            i /= n;
        }
        // Each starts as the pattern does, which is a download address.
        urls.extend(Address::parse(&pieces.concat()).ok());
    }
    Ok(urls)
}

/// What the `*` in an IDM-style address is replaced with, as in IDM's
/// "Replace asterisk to" choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wildcard {
    /// Whole numbers from From to To, padded to the wildcard size.
    Numbers,
    /// Single letters from From to To, in the case they are typed.
    Letters,
}

/// The widest wildcard size accepted: the digits of the biggest number.
pub const MAX_WILDCARD_SIZE: usize = 20;

/// Every URL an IDM-style batch address makes: the one `*` in `address`
/// replaced by each number or letter from `from` to `to`. Numbers are
/// left-padded with zeros to `size` digits (1 means no padding, IDM's
/// default); letters ignore `size`. As in IDM, From must not be after To.
/// Anything else in the address, brackets included, is kept as typed.
/// Fails like [`expand`] when the address is not a web address or the list
/// would hold more than `MAX_BATCH`.
pub fn expand_asterisk(
    address: &str,
    kind: Wildcard,
    from: &str,
    to: &str,
    size: usize,
) -> Result<Vec<Address>, BatchError> {
    let address = address.trim();
    if !is_download_url(address) {
        return Err(BatchError::AddressNotWeb);
    }
    let Some((before, after)) = address.split_once('*') else {
        return Err(BatchError::NoAsterisk);
    };
    if after.contains('*') {
        return Err(BatchError::TwoAsterisks);
    }
    let (from, to) = (from.trim(), to.trim());
    let part = match kind {
        Wildcard::Numbers => {
            let number = |s: &str, bound: Bound| {
                if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
                    return Err(BatchError::NotWholeNumber(bound));
                }
                s.parse::<u64>().map_err(|_| BatchError::BoundTooBig(bound))
            };
            let (a, b) = (number(from, Bound::From)?, number(to, Bound::To)?);
            if !(1..=MAX_WILDCARD_SIZE).contains(&size) {
                return Err(BatchError::WildcardSize);
            }
            if a > b {
                return Err(BatchError::NumbersReversed(a, b));
            }
            Part::Numbers { from: a, to: b, step: 1, width: size }
        }
        Wildcard::Letters => {
            let letter = |s: &str, bound: Bound| {
                let mut chars = s.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_ascii_alphabetic() => Ok(c),
                    _ => Err(BatchError::NotOneLetter(bound)),
                }
            };
            let (a, b) = (letter(from, Bound::From)?, letter(to, Bound::To)?);
            if a.is_ascii_lowercase() != b.is_ascii_lowercase() {
                return Err(BatchError::MixedCaseLetters);
            }
            if a > b {
                return Err(BatchError::LettersReversed(a, b));
            }
            Part::Letters { from: a, to: b }
        }
    };
    let total = part.count();
    if total > MAX_BATCH as u64 {
        return Err(BatchError::RangeTooBig(from.to_string(), to.to_string(), total));
    }
    // Each starts as `address` does, which is a download address.
    Ok((0..total).filter_map(|i| Address::parse(&format!("{before}{}{after}", part.nth(i))).ok()).collect())
}

/// The URLs in a pasted list or text file, one per line: blank lines,
/// `#` comments and repeats are skipped. Also returns how many other lines
/// were not web addresses. Fails when more than `MAX_BATCH` are left.
pub fn parse_list(text: &str) -> Result<(Vec<Address>, usize), BatchError> {
    let mut seen = std::collections::HashSet::new();
    let mut urls = Vec::new();
    let mut rejected = 0;
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let address = match Address::parse(line) {
            Ok(a) if !line.contains(char::is_whitespace) => a,
            _ => {
                rejected += 1;
                continue;
            }
        };
        if seen.insert(line.to_string()) {
            urls.push(address);
        }
    }
    if urls.len() > MAX_BATCH {
        return Err(BatchError::ListTooLong(urls.len()));
    }
    Ok((urls, rejected))
}

/// The download link in copied `text` worth offering: one http(s) or ftp(s) address,
/// alone on the clipboard, whose file type is one of `types` (any file type
/// when `types` is empty), or a torrent's magnet link. A `.torrent` file's
/// address is offered whatever the types when `torrents` (the settings take
/// `.torrent` files as torrents), and as any file otherwise.
pub fn clipboard_link(text: &str, types: &[String], torrents: bool) -> Option<Address> {
    let url = text.trim();
    if url.contains(char::is_whitespace) || url.len() > 4096 {
        return None;
    }
    let address = Address::parse(url).ok()?;
    if crate::torrent::starts_like_magnet(url) {
        return Some(address);
    }
    let parsed = url::Url::parse(url).ok()?;
    let last = parsed.path_segments()?.next_back()?;
    let (_, ext) = last.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    let wanted = !ext.is_empty() && (types.is_empty() || types.contains(&ext) || (torrents && ext == "torrent"));
    wanted.then_some(address)
}

/// Every http, https, ftp or ftps address and every magnet link in `text`,
/// once each and in order: copied lines, a web page's text, or addresses in
/// quotes or brackets. Punctuation that ends a sentence is not taken as part
/// of an address.
pub fn links_in(text: &str) -> Vec<Address> {
    let mut seen = std::collections::HashSet::new();
    let mut urls = Vec::new();
    let separators = |c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | '|');
    for word in text.split(separators) {
        if let Some(start) = word.to_ascii_lowercase().find("magnet:?") {
            let link = word[start..].trim_end_matches(['.', ',', ';', '!', ')', ']']);
            if let Ok(address) = Address::parse(link) {
                if seen.insert(link.to_string()) {
                    urls.push(address);
                }
            }
            continue;
        }
        // An address may follow other text, as in `link:https://...`.
        let lower = word.to_ascii_lowercase();
        let Some(start) = ["https://", "http://", "ftp://", "ftps://"].iter().filter_map(|p| lower.find(p)).min()
        else {
            continue;
        };
        let mut url = &word[start..];
        // A bracket that closes one opened before the address is not its
        // own, nor is a full stop or comma at the end of a sentence.
        loop {
            let trimmed = url.trim_end_matches(['.', ',', ';', ':', '!', '?']);
            let trimmed = match trimmed.strip_suffix(')') {
                Some(t) if !t.contains('(') => t,
                _ => trimmed,
            };
            let trimmed = match trimmed.strip_suffix(']') {
                Some(t) if !t.contains('[') => t,
                _ => trimmed,
            };
            if trimmed.len() == url.len() {
                break;
            }
            url = trimmed;
        }
        if url.len() > 4096 || url::Url::parse(url).ok().and_then(|u| u.host_str().map(|_| ())).is_none() {
            continue;
        }
        // It starts with one of the schemes found above.
        let Ok(address) = Address::parse(url) else {
            continue;
        };
        if seen.insert(url.to_string()) {
            urls.push(address);
        }
    }
    urls
}

/// Whether `s` is an address UDM can download: one that starts like an
/// http, https, ftp or ftps address, or a magnet link with a BitTorrent
/// info hash (`torrent::is_magnet`).
pub fn is_download_url(s: &str) -> bool {
    is_web_url(s) || is_ftp_url(s) || crate::torrent::is_magnet(s)
}

/// Whether `s` starts like an FTP address: `ftp://`, or `ftps://` (FTP over
/// TLS from the first byte), in any case.
pub fn is_ftp_url(s: &str) -> bool {
    ["ftp://", "ftps://"].iter().any(|p| s.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p)))
}

/// Whether `s` starts like an http or https address.
pub fn is_web_url(s: &str) -> bool {
    let lower = s.get(..8).unwrap_or(s).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_found_anywhere_in_copied_text() {
        let text = "Files:\n  https://x.com/a.zip, and (https://x.com/b.zip).\n\
                    <a href=\"ftp://f.net/c.iso\">c</a> https://x.com/a.zip FTPS://g.net/e.iso\n\
                    see https://en.wikipedia.org/wiki/Zip_(file_format) or link:http://y.org/d.pdf!\n\
                    not a link: https:// nor www.x.com/e.zip";
        assert_eq!(
            links_in(text),
            [
                "https://x.com/a.zip",
                "https://x.com/b.zip",
                "ftp://f.net/c.iso",
                "FTPS://g.net/e.iso",
                "https://en.wikipedia.org/wiki/Zip_(file_format)",
                "http://y.org/d.pdf",
            ]
        );
        assert!(links_in("nothing here").is_empty());
    }

    #[test]
    fn padded_numbers() {
        let urls = expand("https://x.com/photo[001-120].jpg").unwrap();
        assert_eq!(urls.len(), 120);
        assert_eq!(urls[0], "https://x.com/photo001.jpg");
        assert_eq!(urls[9], "https://x.com/photo010.jpg");
        assert_eq!(urls[119], "https://x.com/photo120.jpg");
        // No leading zero, no padding.
        assert_eq!(expand("http://x/[8-10]").unwrap(), ["http://x/8", "http://x/9", "http://x/10"]);
    }

    #[test]
    fn steps_and_counting_down() {
        assert_eq!(expand("http://x/[0-10:5]").unwrap(), ["http://x/0", "http://x/5", "http://x/10"]);
        // A step that does not land on the end stops before it.
        assert_eq!(expand("http://x/[1-8:3]").unwrap(), ["http://x/1", "http://x/4", "http://x/7"]);
        assert_eq!(expand("http://x/[3-1]").unwrap(), ["http://x/3", "http://x/2", "http://x/1"]);
        assert_eq!(expand("http://x/[05-01:2]").unwrap(), ["http://x/05", "http://x/03", "http://x/01"]);
        assert!(expand("http://x/[1-5:0]").is_err());
    }

    #[test]
    fn letters() {
        assert_eq!(expand("http://x/[a-c].png").unwrap(), ["http://x/a.png", "http://x/b.png", "http://x/c.png"]);
        assert_eq!(expand("http://x/[C-A]").unwrap(), ["http://x/C", "http://x/B", "http://x/A"]);
        assert!(expand("http://x/[a-C]").is_err());
    }

    #[test]
    fn several_ranges_make_every_combination() {
        assert_eq!(
            expand("https://x/[a-b]/[1-3].jpg").unwrap(),
            [
                "https://x/a/1.jpg",
                "https://x/a/2.jpg",
                "https://x/a/3.jpg",
                "https://x/b/1.jpg",
                "https://x/b/2.jpg",
                "https://x/b/3.jpg",
            ]
        );
    }

    #[test]
    fn other_brackets_stay() {
        assert_eq!(expand("http://[::1]:8080/f[1-2]").unwrap(), ["http://[::1]:8080/f1", "http://[::1]:8080/f2"]);
        assert_eq!(expand("http://x/a[b]c[").unwrap(), ["http://x/a[b]c["]);
        assert_eq!(expand("http://x/[1-2-3]").unwrap(), ["http://x/[1-2-3]"]);
        assert_eq!(expand("http://x/plain.zip").unwrap(), ["http://x/plain.zip"]);
    }

    #[test]
    fn runaway_patterns_are_refused() {
        assert_eq!(count("http://x/[1-100]/[1-100]").unwrap(), 10_000);
        assert_eq!(expand("http://x/[1-100]/[1-100]").unwrap().len(), 10_000);
        assert_eq!(expand("http://x/[1-100]/[1-101]").unwrap_err(), BatchError::PatternTooBig(10_100));
        assert!(expand("http://x/[1-99999999999999999999]").is_err());
        assert!(count("http://x/[0-18446744073709551615]/[1-9]").unwrap() == u64::MAX);
        assert_eq!(expand("ftp://x/[1-2]").unwrap(), ["ftp://x/1", "ftp://x/2"]);
        assert_eq!(expand("ftps://x/[1-2]").unwrap(), ["ftps://x/1", "ftps://x/2"]);
        assert!(expand("file://x/[1-2]").is_err());
    }

    #[test]
    fn asterisk_numbers_match_idm() {
        let urls = expand_asterisk("https://x.com/photo*.jpg", Wildcard::Numbers, "1", "120", 3).unwrap();
        assert_eq!(urls.len(), 120);
        assert_eq!(urls[0], "https://x.com/photo001.jpg");
        assert_eq!(urls[119], "https://x.com/photo120.jpg");
        // The same list as the bracket form.
        assert_eq!(urls, expand("https://x.com/photo[001-120].jpg").unwrap());
        // Size 1, IDM's default, pads nothing; a number wider than the size
        // is kept whole.
        assert_eq!(
            expand_asterisk("http://x/*", Wildcard::Numbers, "8", "10", 1).unwrap(),
            ["http://x/8", "http://x/9", "http://x/10"]
        );
        assert_eq!(
            expand_asterisk("http://x/f*", Wildcard::Numbers, " 99 ", "100", 2).unwrap(),
            ["http://x/f99", "http://x/f100"]
        );
        assert_eq!(expand_asterisk("http://x/*", Wildcard::Numbers, "5", "5", 4).unwrap(), ["http://x/0005"]);
        // Brackets are kept as typed in this form.
        assert_eq!(
            expand_asterisk("http://x/[1-2]/*", Wildcard::Numbers, "0", "1", 1).unwrap(),
            ["http://x/[1-2]/0", "http://x/[1-2]/1"]
        );
    }

    #[test]
    fn asterisk_letters_keep_case_and_ignore_size() {
        assert_eq!(
            expand_asterisk("http://x/*.png", Wildcard::Letters, "a", "c", 5).unwrap(),
            ["http://x/a.png", "http://x/b.png", "http://x/c.png"]
        );
        assert_eq!(
            expand_asterisk("http://x/*", Wildcard::Letters, "Y", "Z", 1).unwrap(),
            ["http://x/Y", "http://x/Z"]
        );
        assert!(expand_asterisk("http://x/*", Wildcard::Letters, "a", "Z", 1).is_err());
        assert!(expand_asterisk("http://x/*", Wildcard::Letters, "ab", "c", 1).is_err());
        assert!(expand_asterisk("http://x/*", Wildcard::Letters, "1", "3", 1).is_err());
    }

    #[test]
    fn asterisk_mistakes_are_refused() {
        let err = expand_asterisk("http://x/*", Wildcard::Numbers, "10", "1", 1).unwrap_err();
        assert_eq!(err, BatchError::NumbersReversed(10, 1));
        assert_eq!(err.to_string(), "From (10) must not be greater than To (1).");
        assert_eq!(
            expand_asterisk("http://x/*", Wildcard::Letters, "c", "a", 1).unwrap_err(),
            BatchError::LettersReversed('c', 'a')
        );
        assert_eq!(
            expand_asterisk("http://x/*", Wildcard::Numbers, "", "2", 1).unwrap_err().to_string(),
            "From must be a whole number."
        );
        assert!(expand_asterisk("http://x/a", Wildcard::Numbers, "1", "2", 1).is_err());
        assert!(expand_asterisk("http://x/*/*", Wildcard::Numbers, "1", "2", 1).is_err());
        assert!(expand_asterisk("file://x/*", Wildcard::Numbers, "1", "2", 1).is_err());
        assert!(expand_asterisk("http://x/*", Wildcard::Numbers, "", "2", 1).is_err());
        assert!(expand_asterisk("http://x/*", Wildcard::Numbers, "-1", "2", 1).is_err());
        assert!(expand_asterisk("http://x/*", Wildcard::Numbers, "1", "99999999999999999999", 1).is_err());
        assert!(expand_asterisk("http://x/*", Wildcard::Numbers, "1", "2", 0).is_err());
        assert!(expand_asterisk("http://x/*", Wildcard::Numbers, "1", "2", MAX_WILDCARD_SIZE + 1).is_err());
        // The batch limit holds here too.
        assert_eq!(expand_asterisk("http://x/*", Wildcard::Numbers, "1", "10000", 1).unwrap().len(), MAX_BATCH);
        let err = expand_asterisk("http://x/*", Wildcard::Numbers, "0", "10000", 1).unwrap_err();
        assert_eq!(err.to_string(), "From 0 to 10000 makes 10001 addresses; at most 10000 can be added at once.");
    }

    #[test]
    fn clipboard_links() {
        let types = vec!["zip".to_string(), "mp4".to_string()];
        assert_eq!(
            clipboard_link("  https://x.com/a/file.ZIP?sig=1 \n", &types, true).as_ref().map(Address::as_str),
            Some("https://x.com/a/file.ZIP?sig=1")
        );
        assert_eq!(clipboard_link("https://x.com/page.html", &types, true), None);
        assert_eq!(clipboard_link("https://x.com/", &types, true), None);
        assert_eq!(clipboard_link("see https://x.com/a.zip", &types, true), None);
        assert_eq!(clipboard_link("https://x.com/a.zip https://x.com/b.zip", &types, true), None);
        assert_eq!(
            clipboard_link("ftp://x.com/a.zip", &types, true).as_ref().map(Address::as_str),
            Some("ftp://x.com/a.zip")
        );
        assert_eq!(
            clipboard_link("ftps://x.com/a.zip", &types, true).as_ref().map(Address::as_str),
            Some("ftps://x.com/a.zip")
        );
        assert_eq!(clipboard_link("file://x.com/a.zip", &types, true), None);
        assert_eq!(
            clipboard_link("https://x.com/a.html", &[], true).as_ref().map(Address::as_str),
            Some("https://x.com/a.html")
        );
        assert_eq!(clipboard_link("https://x.com/download", &[], true), None);
        // A torrent is offered whatever the types: its magnet link, or its
        // `.torrent` file.
        let magnet = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=Album";
        assert_eq!(clipboard_link(magnet, &types, true).as_ref().map(Address::as_str), Some(magnet));
        assert_eq!(clipboard_link("magnet:?dn=no-hash", &types, true), None);
        assert_eq!(
            clipboard_link("https://x.com/a.torrent", &types, true).as_ref().map(Address::as_str),
            Some("https://x.com/a.torrent")
        );
        assert_eq!(clipboard_link("https://x.com/a.torrent", &types, false), None, "a file of a type not offered");
    }

    #[test]
    fn lists() {
        let text = "\u{feff}https://a/1.zip\n\n  https://a/2.zip  \r\n# a note\nhttps://a/1.zip\nnot a url\nhttp://a/with space\nHTTPS://A/3.zip\n";
        let (urls, rejected) = parse_list(text).unwrap();
        assert_eq!(urls, ["https://a/1.zip", "https://a/2.zip", "HTTPS://A/3.zip"]);
        assert_eq!(rejected, 2);
        let big: String = (0..=MAX_BATCH).map(|i| format!("https://a/{i}\n")).collect();
        assert!(parse_list(&big).is_err());
    }
}
