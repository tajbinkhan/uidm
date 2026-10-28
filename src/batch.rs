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

/// The most URLs one batch may produce, so a mistyped pattern cannot add a
/// million downloads.
pub const MAX_BATCH: usize = 10_000;

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
fn parse_range(inner: &str) -> Result<Option<Part>, String> {
    let (range, step) = match inner.split_once(':') {
        Some((r, s)) => (r, Some(s)),
        None => (inner, None),
    };
    let Some((a, b)) = range.split_once('-') else {
        return Ok(None);
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    if digits(a) && digits(b) {
        let from: u64 = a.parse().map_err(|_| format!("[{inner}]: the number is too big"))?;
        let to: u64 = b.parse().map_err(|_| format!("[{inner}]: the number is too big"))?;
        let step = match step {
            None => 1,
            Some(s) if digits(s) => s.parse().map_err(|_| format!("[{inner}]: the step is too big"))?,
            Some(_) => return Ok(None),
        };
        if step == 0 {
            return Err(format!("[{inner}]: the step must be at least 1"));
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
            return Err(format!("[{inner}]: both letters must be the same case"));
        }
        return Ok(Some(Part::Letters { from, to }));
    }
    Ok(None)
}

fn parse(pattern: &str) -> Result<Vec<Part>, String> {
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
pub fn count(pattern: &str) -> Result<u64, String> {
    Ok(parse(pattern.trim())?
        .iter()
        .fold(1u64, |n, p| n.saturating_mul(p.count())))
}

/// Every URL `pattern` produces, in order. Fails when the pattern is not a
/// web address or produces more than `MAX_BATCH`.
pub fn expand(pattern: &str) -> Result<Vec<String>, String> {
    let pattern = pattern.trim();
    if !is_download_url(pattern) {
        return Err("The pattern must start with http://, https:// or ftp://.".into());
    }
    let parts = parse(pattern)?;
    let total = parts.iter().fold(1u64, |n, p| n.saturating_mul(p.count()));
    if total > MAX_BATCH as u64 {
        return Err(format!(
            "The pattern makes {total} addresses; at most {MAX_BATCH} can be added at once."
        ));
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
        urls.push(pieces.concat());
    }
    Ok(urls)
}

/// The URLs in a pasted list or text file, one per line: blank lines,
/// `#` comments and repeats are skipped. Also returns how many other lines
/// were not web addresses. Fails when more than `MAX_BATCH` are left.
pub fn parse_list(text: &str) -> Result<(Vec<String>, usize), String> {
    let mut seen = std::collections::HashSet::new();
    let mut urls = Vec::new();
    let mut rejected = 0;
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !is_download_url(line) || line.contains(char::is_whitespace) {
            rejected += 1;
            continue;
        }
        if seen.insert(line.to_string()) {
            urls.push(line.to_string());
        }
    }
    if urls.len() > MAX_BATCH {
        return Err(format!(
            "The list has {} addresses; at most {MAX_BATCH} can be added at once.",
            urls.len()
        ));
    }
    Ok((urls, rejected))
}

/// The download link in copied `text` worth offering: one http(s) or ftp address,
/// alone on the clipboard, whose file type is one of `types` (any file type
/// when `types` is empty).
pub fn clipboard_link(text: &str, types: &[String]) -> Option<String> {
    let url = text.trim();
    if !is_download_url(url) || url.contains(char::is_whitespace) || url.len() > 4096 {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    let last = parsed.path_segments()?.next_back()?;
    let (_, ext) = last.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    let wanted = !ext.is_empty() && (types.is_empty() || types.contains(&ext));
    wanted.then(|| url.to_string())
}

/// Every http, https or ftp address in `text`, once each and in order:
/// copied lines, a web page's text, or addresses in quotes or brackets.
/// Punctuation that ends a sentence is not taken as part of an address.
pub fn links_in(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut urls = Vec::new();
    let separators = |c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | '|');
    for word in text.split(separators) {
        // An address may follow other text, as in `link:https://...`.
        let lower = word.to_ascii_lowercase();
        let Some(start) = ["https://", "http://", "ftp://"].iter().filter_map(|p| lower.find(p)).min() else {
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
        if seen.insert(url.to_string()) {
            urls.push(url.to_string());
        }
    }
    urls
}

/// Whether `s` starts like an address UIDM can download: http, https or ftp.
pub fn is_download_url(s: &str) -> bool {
    is_web_url(s) || s.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("ftp://"))
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
                    <a href=\"ftp://f.net/c.iso\">c</a> https://x.com/a.zip\n\
                    see https://en.wikipedia.org/wiki/Zip_(file_format) or link:http://y.org/d.pdf!\n\
                    not a link: https:// nor www.x.com/e.zip";
        assert_eq!(
            links_in(text),
            [
                "https://x.com/a.zip",
                "https://x.com/b.zip",
                "ftp://f.net/c.iso",
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
        let err = expand("http://x/[1-100]/[1-101]").unwrap_err();
        assert!(err.contains("10100"), "{err}");
        assert!(expand("http://x/[1-99999999999999999999]").is_err());
        assert!(count("http://x/[0-18446744073709551615]/[1-9]").unwrap() == u64::MAX);
        assert_eq!(expand("ftp://x/[1-2]").unwrap(), ["ftp://x/1", "ftp://x/2"]);
        assert!(expand("file://x/[1-2]").is_err());
    }

    #[test]
    fn clipboard_links() {
        let types = vec!["zip".to_string(), "mp4".to_string()];
        assert_eq!(
            clipboard_link("  https://x.com/a/file.ZIP?sig=1 \n", &types).as_deref(),
            Some("https://x.com/a/file.ZIP?sig=1")
        );
        assert_eq!(clipboard_link("https://x.com/page.html", &types), None);
        assert_eq!(clipboard_link("https://x.com/", &types), None);
        assert_eq!(clipboard_link("see https://x.com/a.zip", &types), None);
        assert_eq!(clipboard_link("https://x.com/a.zip https://x.com/b.zip", &types), None);
        assert_eq!(clipboard_link("ftp://x.com/a.zip", &types).as_deref(), Some("ftp://x.com/a.zip"));
        assert_eq!(clipboard_link("file://x.com/a.zip", &types), None);
        assert_eq!(clipboard_link("https://x.com/a.html", &[]).as_deref(), Some("https://x.com/a.html"));
        assert_eq!(clipboard_link("https://x.com/download", &[]), None);
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
