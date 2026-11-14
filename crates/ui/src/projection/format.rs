//! Sizes, speeds, times and dates as the window's language writes them,
//! so the frontend shows text it never has to format.

use udm_domain::bytes::ByteCount;
use udm_domain::country::CountryCode;
use udm_domain::download::{DownloadEntry, Status};
use udm_domain::queue;

use crate::contract::FileIcon;
use crate::i18n::{fill, Language};

const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

/// `n` bytes in the largest unit under 1024 of it, with `places` decimals.
pub fn bytes(lang: &'static Language, n: ByteCount, places: usize) -> String {
    let mut value = n.as_f64();
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} {}", lang.tr(UNITS[0]))
    } else {
        format!("{} {}", lang.number(value, places), lang.tr(UNITS[unit]))
    }
}

/// A speed: one second's worth of bytes, per second.
pub fn rate(lang: &'static Language, bytes_per_sec: f64, places: usize) -> String {
    format!("{}/s", bytes(lang, ByteCount::new(bytes_per_sec.max(0.0) as u64), places))
}

/// A time left.
pub fn duration(lang: &'static Language, secs: f64) -> String {
    let s = secs.round().max(0.0) as u64;
    let (m, h) = ((s / 60).to_string(), (s / 3600).to_string());
    match s {
        0..=59 => fill(lang.tr("{seconds}s"), &[("seconds", &s.to_string())]),
        60..=3599 => fill(lang.tr("{minutes}m {seconds}s"), &[("minutes", &m), ("seconds", &(s % 60).to_string())]),
        _ => fill(lang.tr("{hours}h {minutes}m"), &[("hours", &h), ("minutes", &((s % 3600) / 60).to_string())]),
    }
}

/// `unix` (seconds since 1970) as a local date and time, `offset` minutes
/// ahead of UTC.
pub fn date(lang: &'static Language, unix: u64, offset: i64) -> String {
    let minutes = unix as i64 / 60 + offset;
    let ymd = queue::civil_from_days(minutes.div_euclid(1440));
    lang.date_time(ymd, minutes.rem_euclid(1440) as u32)
}

/// A country as the list shows it: its flag, made of the two regional
/// indicator symbols for its letters, then its code ("🇩🇪 DE"). Where the
/// system draws no flags (Windows draws the letters again, "DE DE"), the
/// code alone (`Ctx::flags`).
pub fn country(code: CountryCode, flag: bool) -> String {
    if !flag {
        return code.to_string();
    }
    let flag: String = code
        .letters()
        .iter()
        .filter_map(|l| l.checked_sub(b'A').filter(|n| *n < 26))
        .filter_map(|n| char::from_u32(0x1F1E6 + u32::from(n)))
        .collect();
    format!("{flag} {code}")
}

/// How far along a download is, from 0 to 1, when that is known.
pub fn fraction(e: &DownloadEntry) -> Option<f32> {
    let parts = e.parts.filter(|[_, all]| *all > 0);
    match (e.total, parts) {
        _ if e.status == Status::Done => Some(1.0),
        (Some(t), _) if !t.is_zero() => Some((e.done.as_f64() / t.as_f64()).clamp(0.0, 1.0) as f32),
        (_, Some([done, all])) => Some((done as f32 / all as f32).clamp(0.0, 1.0)),
        _ => None,
    }
}

/// Seconds left at `bytes_per_sec`: from the total, or for a stream from
/// the average segment so far.
pub fn time_left(e: &DownloadEntry, bytes_per_sec: Option<f64>) -> Option<f64> {
    let bps = bytes_per_sec.filter(|s| *s > 0.0)?;
    let estimate = e.total.or_else(|| match e.parts {
        Some([done, all]) if done > 0 => Some(ByteCount::new(e.done.get() / done * all)),
        _ => None,
    })?;
    (estimate > e.done).then(|| (estimate - e.done).as_f64() / bps)
}

/// A status in a word, as the list's Status column shows it.
pub fn status_word(lang: &'static Language, status: Status) -> &'static str {
    lang.tr(match status {
        Status::Queued => "Queued",
        Status::Downloading => "Downloading",
        Status::Done => "Complete",
        Status::Failed => "Failed",
        Status::Canceled => "Canceled",
        Status::Interrupted => "Interrupted",
        Status::Confirming => "Confirm",
        Status::Waiting => "Waiting",
    })
}

/// What the progress window says a download is doing.
pub fn status_line(lang: &'static Language, e: &DownloadEntry) -> &'static str {
    lang.tr(match e.status {
        Status::Queued => "Waiting in its queue",
        Status::Confirming => "Waiting for the save-as dialog",
        Status::Waiting => "Waiting to be started",
        Status::Downloading if e.done.is_zero() => "Connecting...",
        Status::Downloading => "Receiving data...",
        Status::Done => "Done",
        Status::Failed => "Failed",
        Status::Canceled => "Canceled",
        Status::Interrupted => "Interrupted",
    })
}

/// The kind of file `name` is, by its extension.
pub fn file_icon(name: &str) -> FileIcon {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    icon_of_extension(&ext)
}

pub fn icon_of_extension(ext: &str) -> FileIcon {
    match ext {
        "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "cab" | "arj" | "lzh" | "ace" | "z" => {
            FileIcon::Archive
        }
        "pdf" | "doc" | "docx" | "txt" | "rtf" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp" | "epub"
        | "md" | "csv" | "pps" => FileIcon::Document,
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "wma" | "opus" | "aif" | "aiff" | "mid" | "midi" | "ra" => {
            FileIcon::Music
        }
        "exe" | "msi" | "deb" | "rpm" | "apk" | "dmg" | "appimage" | "pkg" | "bat" | "sh" | "msu" | "iso" | "bin"
        | "img" => FileIcon::Program,
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "flv" | "m4v" | "ts" | "mpg" | "mpeg" | "3gp" | "ogv"
        | "rm" | "rmvb" | "asf" | "qt" => FileIcon::Video,
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "svg" | "tif" | "tiff" | "ico" | "avif" | "heic" => {
            FileIcon::Image
        }
        _ => FileIcon::Other,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_country_shows_its_flag_and_code() {
        let code = |c: &str| udm_domain::country::CountryCode::parse(c).unwrap();
        assert_eq!(super::country(code("US"), true), "\u{1F1FA}\u{1F1F8} US");
        assert_eq!(super::country(code("de"), true), "\u{1F1E9}\u{1F1EA} DE");
        assert_eq!(super::country(code("de"), false), "DE", "where flags do not draw");
    }

    use super::*;
    use crate::i18n::resolve;

    #[test]
    fn sizes_take_the_largest_unit_and_the_language_s_decimals() {
        let en = resolve("en", &[]);
        assert_eq!(bytes(en, ByteCount::new(512), 2), "512 B");
        assert_eq!(bytes(en, ByteCount::new(70_686), 2), "69.03 KB");
        assert_eq!(bytes(en, ByteCount::new(41_943_040), 3), "40.000 MB");
        assert_eq!(bytes(resolve("de", &[]), ByteCount::new(1536), 1), "1,5 KB");
    }

    #[test]
    fn durations_and_dates_read() {
        let en = resolve("en", &[]);
        assert_eq!(duration(en, 25.0), "25s");
        assert_eq!(duration(en, 274.0), "4m 34s");
        assert_eq!(duration(en, 7300.0), "2h 1m");
        assert_eq!(date(en, 1_793_000_000, 60), "2026-10-26 08:33");
    }

    #[test]
    fn files_get_icons_by_type() {
        assert_eq!(file_icon("a.ZIP"), FileIcon::Archive);
        assert_eq!(file_icon("setup.exe"), FileIcon::Program);
        assert_eq!(file_icon("song.mp3"), FileIcon::Music);
        assert_eq!(file_icon("noext"), FileIcon::Other);
    }
}
