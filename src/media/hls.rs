//! HLS playlists (RFC 8216).
//!
//! A master playlist lists the qualities (variants), each its own media
//! playlist, and maybe separate audio tracks. A media playlist lists the
//! segments, with `#EXT-X-KEY` lines for the keys of encrypted ones and
//! `#EXT-X-MAP` for a header segment.
//!
//! Only AES-128 with a key at a URL is supported. Other key systems
//! (`SAMPLE-AES`, or any `KEYFORMAT` other than `identity`, such as FairPlay
//! or Widevine) are DRM, and refused.

use super::{resolve, MediaInfo, MediaKind, MediaOption, Segment, SegmentKey, Track, DRM_REFUSED, LIVE_REFUSED};

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub url: String,
    pub bandwidth: Option<u64>,
    pub height: Option<u32>,
    /// The `GROUP-ID` of its separate audio tracks.
    pub audio: Option<String>,
}

/// A separate audio track (`#EXT-X-MEDIA:TYPE=AUDIO`).
#[derive(Debug, Clone, PartialEq)]
pub struct Rendition {
    pub group: String,
    pub name: String,
    /// Its media playlist. Without one the audio is in the variant itself.
    pub url: Option<String>,
    pub default: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Master {
    /// Best first.
    pub variants: Vec<Variant>,
    pub audio: Vec<Rendition>,
    /// A `#EXT-X-SESSION-KEY` names a DRM key system.
    pub drm: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Playlist {
    Master(Master),
    Media(Track),
}

/// The attributes of a tag such as `BANDWIDTH=1280000,CODECS="a,b"`.
fn attributes(list: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = list.trim();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let name = rest[..eq].trim().to_ascii_uppercase();
        rest = &rest[eq + 1..];
        let value;
        if let Some(quoted) = rest.strip_prefix('"') {
            let end = quoted.find('"').unwrap_or(quoted.len());
            value = quoted[..end].to_string();
            rest = quoted.get(end + 1..).unwrap_or("");
            rest = rest.trim_start_matches(|c: char| c != ',');
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            value = rest[..end].trim().to_string();
            rest = &rest[end..];
        }
        rest = rest.strip_prefix(',').unwrap_or(rest).trim_start();
        out.push((name, value));
    }
    out
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

/// Whether a key tag names a key system UIDM must not handle.
fn is_drm_key(attrs: &[(String, String)]) -> bool {
    let method = attr(attrs, "METHOD").unwrap_or("NONE");
    let format = attr(attrs, "KEYFORMAT").unwrap_or("identity");
    let uri = attr(attrs, "URI").unwrap_or("");
    method != "NONE"
        && (method != "AES-128"
            || !format.eq_ignore_ascii_case("identity")
            || uri.starts_with("skd:"))
}

fn parse_iv(hex: &str) -> Result<[u8; 16], String> {
    let digits = hex.trim().trim_start_matches("0x").trim_start_matches("0X");
    if digits.len() > 32 || digits.is_empty() {
        return Err(format!("bad IV {hex}"));
    }
    let n = u128::from_str_radix(digits, 16).map_err(|_| format!("bad IV {hex}"))?;
    Ok(n.to_be_bytes())
}

/// `n[@o]` of `#EXT-X-BYTERANGE`, with `next` the offset to use without `@o`.
fn byte_range(spec: &str, next: u64) -> Result<(u64, u64), String> {
    let (len, offset) = match spec.split_once('@') {
        Some((l, o)) => (l, o.trim().parse().map_err(|_| format!("bad byte range {spec}"))?),
        None => (spec, next),
    };
    let len: u64 = len.trim().parse().map_err(|_| format!("bad byte range {spec}"))?;
    if len == 0 {
        return Err(format!("empty byte range {spec}"));
    }
    Ok((offset, offset + len - 1))
}

/// The extension a joined track gets, from how its segments are named.
fn track_ext(init: bool, first: Option<&str>, audio: bool) -> &'static str {
    let path = first
        .map(|u| u.split(['?', '#']).next().unwrap_or(u).to_ascii_lowercase())
        .unwrap_or_default();
    if init || path.ends_with(".m4s") || path.ends_with(".mp4") || path.ends_with(".m4a") {
        if audio {
            "m4a"
        } else {
            "mp4"
        }
    } else if path.ends_with(".aac") {
        "aac"
    } else {
        "ts"
    }
}

/// Read a playlist fetched from `base`. `audio` says a media playlist is an
/// audio track, for its file extension.
pub fn parse(text: &str, base: &str, audio: bool) -> Result<Playlist, String> {
    let text = text.trim_start_matches('\u{feff}');
    if !text.trim_start().starts_with("#EXTM3U") {
        return Err("This is not an HLS playlist.".into());
    }
    if text.contains("#EXT-X-STREAM-INF") {
        parse_master(text, base).map(Playlist::Master)
    } else {
        parse_media(text, base, audio).map(Playlist::Media)
    }
}

fn parse_master(text: &str, base: &str) -> Result<Master, String> {
    let mut master = Master::default();
    let mut pending: Option<Vec<(String, String)>> = None;
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending = Some(attributes(rest));
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA:") {
            let a = attributes(rest);
            if attr(&a, "TYPE") != Some("AUDIO") {
                continue;
            }
            master.audio.push(Rendition {
                group: attr(&a, "GROUP-ID").unwrap_or_default().to_string(),
                name: attr(&a, "NAME")
                    .or(attr(&a, "LANGUAGE"))
                    .unwrap_or("Audio")
                    .to_string(),
                url: attr(&a, "URI").map(|u| resolve(base, u)).transpose()?,
                default: attr(&a, "DEFAULT") == Some("YES"),
            });
        } else if let Some(rest) = line.strip_prefix("#EXT-X-SESSION-KEY:") {
            if is_drm_key(&attributes(rest)) {
                master.drm = true;
            }
        } else if !line.is_empty() && !line.starts_with('#') {
            if let Some(a) = pending.take() {
                let height = attr(&a, "RESOLUTION")
                    .and_then(|r| r.split_once(['x', 'X']))
                    .and_then(|(_, h)| h.parse().ok());
                master.variants.push(Variant {
                    url: resolve(base, line)?,
                    bandwidth: attr(&a, "BANDWIDTH").and_then(|b| b.parse().ok()),
                    height,
                    audio: attr(&a, "AUDIO").map(str::to_string),
                });
            }
        }
    }
    if master.variants.is_empty() {
        return Err("The playlist lists no videos.".into());
    }
    master
        .variants
        .sort_by_key(|v| std::cmp::Reverse((v.height.unwrap_or(0), v.bandwidth.unwrap_or(0))));
    Ok(master)
}

fn parse_media(text: &str, base: &str, audio: bool) -> Result<Track, String> {
    let mut segments = Vec::new();
    let mut init = None;
    let mut key: Option<(String, Option<[u8; 16]>)> = None;
    let mut sequence: u64 = 0;
    let mut range: Option<String> = None;
    // Where a `#EXT-X-BYTERANGE` without an offset starts: just after the
    // previous range of the same URL.
    let mut next_offset: Option<(String, u64)> = None;
    let mut ended = false;
    let mut duration = 0.0;
    for line in text.lines().map(str::trim) {
        if let Some(n) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            sequence = n.trim().parse().map_err(|_| format!("bad media sequence {n}"))?;
        } else if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let secs = rest.split(',').next().unwrap_or("").trim();
            duration += secs.parse::<f64>().unwrap_or(0.0);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            let a = attributes(rest);
            if is_drm_key(&a) {
                return Err(DRM_REFUSED.into());
            }
            key = match attr(&a, "METHOD") {
                Some("AES-128") => {
                    let uri = attr(&a, "URI").ok_or("an encryption key has no address")?;
                    Some((resolve(base, uri)?, attr(&a, "IV").map(parse_iv).transpose()?))
                }
                _ => None,
            };
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            let a = attributes(rest);
            let uri = attr(&a, "URI").ok_or("a header segment has no address")?;
            let range = attr(&a, "BYTERANGE").map(|r| byte_range(r, 0)).transpose()?;
            init = Some(Segment {
                url: resolve(base, uri)?,
                range,
                key: None,
            });
        } else if let Some(rest) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            range = Some(rest.trim().to_string());
        } else if line == "#EXT-X-ENDLIST" {
            ended = true;
        } else if !line.is_empty() && !line.starts_with('#') {
            let url = resolve(base, line)?;
            let n = sequence + segments.len() as u64;
            let seg_range = match range.take() {
                Some(spec) => {
                    // A range without an offset continues the previous range
                    // of the same URL.
                    let next = match &next_offset {
                        Some((u, o)) if *u == url => *o,
                        _ => 0,
                    };
                    let r = byte_range(&spec, next)?;
                    next_offset = Some((url.clone(), r.1 + 1));
                    Some(r)
                }
                None => None,
            };
            segments.push(Segment {
                url,
                range: seg_range,
                key: key.as_ref().map(|(url, iv)| SegmentKey {
                    url: url.clone(),
                    iv: iv.unwrap_or_else(|| (n as u128).to_be_bytes()),
                }),
            });
        }
    }
    if !ended {
        return Err(LIVE_REFUSED.into());
    }
    if segments.is_empty() {
        return Err("The playlist has no segments.".into());
    }
    let ext = track_ext(init.is_some(), segments.first().map(|s| s.url.as_str()), audio);
    Ok(Track {
        init,
        segments,
        ext,
        duration: Some(duration),
    })
}

/// The qualities a master playlist offers, best first, and its audio tracks.
/// `duration` comes from one of its media playlists.
pub fn describe(master: &Master, duration: Option<f64>) -> MediaInfo {
    let video = master
        .variants
        .iter()
        .map(|v| MediaOption {
            id: v.url.clone(),
            label: super::quality_label(v.height, v.bandwidth),
            height: v.height,
            bandwidth: v.bandwidth,
            size: super::estimate_size(v.bandwidth, duration),
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    let audio = master
        .audio
        .iter()
        .filter_map(|r| r.url.clone().map(|u| (u, r)))
        .filter(|(u, _)| seen.insert(u.clone()))
        .map(|(url, r)| MediaOption {
            id: url,
            label: r.name.clone(),
            ..MediaOption::default()
        })
        .collect();
    MediaInfo {
        kind: Some(MediaKind::Hls),
        video,
        audio,
        duration,
        drm: master.drm,
        error: master.drm.then(|| DRM_REFUSED.to_string()),
        not_stream: false,
    }
}

/// The variant `choice` names, or the best.
pub fn pick_variant<'a>(master: &'a Master, choice: Option<&str>) -> &'a Variant {
    choice
        .and_then(|id| master.variants.iter().find(|v| v.url == id))
        .unwrap_or(&master.variants[0])
}

/// The separate audio track to fetch with `variant`: the chosen one if it is
/// in the variant's group, else the group's default, else its first. `None`
/// when the variant carries its own audio.
pub fn pick_audio<'a>(master: &'a Master, variant: &Variant, choice: Option<&str>) -> Option<&'a str> {
    let group = variant.audio.as_deref()?;
    let in_group: Vec<&Rendition> = master
        .audio
        .iter()
        .filter(|r| r.group == group && r.url.is_some())
        .collect();
    let chosen = choice.and_then(|id| in_group.iter().find(|r| r.url.as_deref() == Some(id)));
    chosen
        .or_else(|| in_group.iter().find(|r| r.default))
        .or_else(|| in_group.first())
        .and_then(|r| r.url.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://cdn.x/v/master.m3u8";

    #[test]
    fn attribute_lists() {
        let a = attributes(r#"BANDWIDTH=1280000,CODECS="avc1.4d401f,mp4a.40.2",RESOLUTION=1280x720, AUDIO="aud""#);
        assert_eq!(attr(&a, "BANDWIDTH"), Some("1280000"));
        assert_eq!(attr(&a, "CODECS"), Some("avc1.4d401f,mp4a.40.2"));
        assert_eq!(attr(&a, "RESOLUTION"), Some("1280x720"));
        assert_eq!(attr(&a, "AUDIO"), Some("aud"));
    }

    #[test]
    fn master_playlists() {
        let text = "#EXTM3U
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aac\",NAME=\"English\",DEFAULT=YES,URI=\"audio/en.m3u8\"
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aac\",NAME=\"Deutsch\",URI=\"audio/de.m3u8\"
#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",URI=\"subs/en.m3u8\"
#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,AUDIO=\"aac\"
360/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,AUDIO=\"aac\"
1080/index.m3u8?token=1
";
        let Playlist::Master(m) = parse(text, BASE, false).unwrap() else { panic!() };
        assert_eq!(m.variants.len(), 2);
        assert_eq!(m.variants[0].url, "https://cdn.x/v/1080/index.m3u8?token=1");
        assert_eq!(m.variants[0].height, Some(1080));
        assert_eq!(m.audio.len(), 2);
        let best = pick_variant(&m, None);
        assert_eq!(pick_audio(&m, best, None), Some("https://cdn.x/v/audio/en.m3u8"));
        assert_eq!(
            pick_audio(&m, best, Some("https://cdn.x/v/audio/de.m3u8")),
            Some("https://cdn.x/v/audio/de.m3u8")
        );
        assert_eq!(pick_variant(&m, Some("https://cdn.x/v/360/index.m3u8")).height, Some(360));
        assert_eq!(pick_variant(&m, Some("nope")).height, Some(1080));

        let info = describe(&m, Some(100.0));
        assert_eq!(info.video[0].label, "1080p, 5.0 Mbps");
        assert_eq!(info.video[0].size, Some(62_500_000));
        assert_eq!(info.audio.len(), 2);
        assert!(!info.drm);
    }

    #[test]
    fn media_playlists_with_keys() {
        let text = "#EXTM3U
#EXT-X-TARGETDURATION:10
#EXT-X-MEDIA-SEQUENCE:7
#EXTINF:10.0,
seg7.ts
#EXT-X-KEY:METHOD=AES-128,URI=\"https://keys.x/k1\"
#EXTINF:10.0,
seg8.ts
#EXT-X-KEY:METHOD=AES-128,URI=\"k2\",IV=0x0000000000000000000000000000ABCD
#EXTINF:5.5,
seg9.ts
#EXT-X-KEY:METHOD=NONE
#EXTINF:1,
seg10.ts
#EXT-X-ENDLIST
";
        let Playlist::Media(t) = parse(text, BASE, false).unwrap() else { panic!() };
        assert_eq!(t.segments.len(), 4);
        assert_eq!(t.ext, "ts");
        assert_eq!(t.duration, Some(26.5));
        assert_eq!(t.segments[0].key, None);
        let k = t.segments[1].key.as_ref().unwrap();
        assert_eq!(k.url, "https://keys.x/k1");
        assert_eq!(k.iv, 8u128.to_be_bytes(), "the IV defaults to the sequence number");
        let k = t.segments[2].key.as_ref().unwrap();
        assert_eq!(k.url, "https://cdn.x/v/k2");
        assert_eq!(k.iv, 0xABCDu128.to_be_bytes());
        assert_eq!(t.segments[3].key, None);
    }

    #[test]
    fn byte_ranges_and_headers() {
        let text = "#EXTM3U
#EXT-X-MAP:URI=\"main.mp4\",BYTERANGE=\"700@0\"
#EXTINF:4,
#EXT-X-BYTERANGE:1000@700
main.mp4
#EXTINF:4,
#EXT-X-BYTERANGE:500
main.mp4
#EXT-X-ENDLIST
";
        let Playlist::Media(t) = parse(text, BASE, false).unwrap() else { panic!() };
        assert_eq!(t.init.as_ref().unwrap().range, Some((0, 699)));
        assert_eq!(t.segments[0].range, Some((700, 1699)));
        assert_eq!(t.segments[1].range, Some((1700, 2199)));
        assert_eq!(t.ext, "mp4");
        let Playlist::Media(a) = parse(text, BASE, true).unwrap() else { panic!() };
        assert_eq!(a.ext, "m4a");
    }

    #[test]
    fn drm_and_live_are_refused() {
        let fairplay = "#EXTM3U
#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://key\",KEYFORMAT=\"com.apple.streamingkeydelivery\"
#EXTINF:4,
a.ts
#EXT-X-ENDLIST
";
        assert_eq!(parse(fairplay, BASE, false).unwrap_err(), DRM_REFUSED);
        let widevine = "#EXTM3U
#EXT-X-KEY:METHOD=AES-128,URI=\"data:x\",KEYFORMAT=\"urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed\"
#EXTINF:4,
a.ts
#EXT-X-ENDLIST
";
        assert_eq!(parse(widevine, BASE, false).unwrap_err(), DRM_REFUSED);
        let session = "#EXTM3U
#EXT-X-SESSION-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\",KEYFORMAT=\"com.apple.streamingkeydelivery\"
#EXT-X-STREAM-INF:BANDWIDTH=1
a.m3u8
";
        let Playlist::Master(m) = parse(session, BASE, false).unwrap() else { panic!() };
        assert!(describe(&m, None).drm);
        let live = "#EXTM3U\n#EXTINF:4,\na.ts\n#EXTINF:4,\nb.ts\n";
        assert_eq!(parse(live, BASE, false).unwrap_err(), LIVE_REFUSED);
        assert!(parse("<html>", BASE, false).is_err());
    }
}
