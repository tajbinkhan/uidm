//! DASH manifests (`.mpd`, ISO/IEC 23009-1).
//!
//! A manifest lists adaptation sets (video, audio, subtitles), each with
//! representations (qualities). Each representation's segments come in one of
//! three forms:
//!
//! - `SegmentTemplate`: addresses made from a pattern such as
//!   `video_$Number%05d$.m4s`, counted from a fixed segment duration or
//!   listed by a `SegmentTimeline`;
//! - `SegmentList`: every address written out;
//! - `SegmentBase` (or nothing): one file at the `BaseURL`.
//!
//! Templates and lists can sit on the period, adaptation set or
//! representation; the most specific wins, attribute by attribute. A
//! `ContentProtection` element marks DRM, which is refused, as are live
//! (`dynamic`) manifests.

use roxmltree::{Document, Node};

use super::{resolve, MediaInfo, MediaKind, MediaOption, Segment, Track, DRM_REFUSED, LIVE_REFUSED};

/// More segments than this in one track means a broken manifest.
const MAX_SEGMENTS: usize = 500_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    Video,
    Audio,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Representation {
    pub id: String,
    pub content: Content,
    pub bandwidth: Option<u64>,
    pub height: Option<u32>,
    pub language: Option<String>,
    pub drm: bool,
    /// Its segments, or why they could not be worked out.
    pub track: Result<Track, String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Manifest {
    /// Best first.
    pub video: Vec<Representation>,
    pub audio: Vec<Representation>,
    pub duration: Option<f64>,
}

fn child<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|n| n.is_element() && n.tag_name().name() == name)
}

fn children<'a, 'i: 'a>(node: Node<'a, 'i>, name: &'a str) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    node.children().filter(move |n| n.is_element() && n.tag_name().name() == name)
}

/// An ISO 8601 duration such as `PT1H2M3.5S`, in seconds.
pub fn parse_duration(s: &str) -> Option<f64> {
    let s = s.trim().strip_prefix('P')?;
    let (date, time) = s.split_once('T').unwrap_or((s, ""));
    let mut secs = 0.0;
    let mut add = |part: &str, units: &[(char, f64)]| -> Option<()> {
        let mut num = String::new();
        for c in part.chars() {
            if c.is_ascii_digit() || c == '.' {
                num.push(c);
            } else {
                let unit = units.iter().find(|(u, _)| *u == c)?.1;
                secs += num.parse::<f64>().ok()? * unit;
                num.clear();
            }
        }
        num.is_empty().then_some(())
    };
    add(date, &[('Y', 365.0 * 86400.0), ('M', 30.0 * 86400.0), ('W', 7.0 * 86400.0), ('D', 86400.0)])?;
    add(time, &[('H', 3600.0), ('M', 60.0), ('S', 1.0)])?;
    Some(secs)
}

/// The widest number a segment template may ask for (`$Number%020d$`).
const MAX_FIELD_WIDTH: usize = 20;

/// Fill a template's `$RepresentationID$`, `$Number$`, `$Time$` and
/// `$Bandwidth$`, with an optional `%0Nd` width, and `$$`.
pub fn fill(template: &str, id: &str, number: u64, time: u64, bandwidth: u64) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('$') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let tag = &after[..end];
        let (name, format) = tag.split_once('%').unwrap_or((tag, ""));
        // A manifest can ask for any width; a few hundred zeros would only
        // make the address fail, and billions would take all the memory.
        let width: usize = format
            .strip_prefix('0')
            .unwrap_or(format)
            .trim_end_matches('d')
            .parse::<usize>()
            .unwrap_or(1)
            .min(MAX_FIELD_WIDTH);
        match name {
            "" => out.push('$'),
            "RepresentationID" => out.push_str(id),
            "Number" => out.push_str(&format!("{number:0width$}")),
            "Time" => out.push_str(&format!("{time:0width$}")),
            "Bandwidth" => out.push_str(&format!("{bandwidth:0width$}")),
            _ => {
                out.push('$');
                out.push_str(tag);
                out.push('$');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The attribute `name` from the most specific of `levels` that has it.
fn inherited<'a>(levels: &[Option<Node<'a, '_>>], name: &str) -> Option<&'a str> {
    levels.iter().rev().flatten().find_map(|n| n.attribute(name))
}

/// `a-b` of a `mediaRange` or `range`.
fn parse_range(s: &str) -> Option<(u64, u64)> {
    let (a, b) = s.trim().split_once('-')?;
    let (a, b) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    (b >= a).then_some((a, b))
}

struct Context<'a> {
    base: String,
    id: &'a str,
    bandwidth: u64,
    /// The period's length in seconds, when known.
    duration: Option<f64>,
}

fn template_track<'a>(levels: &[Option<Node<'a, '_>>], cx: &Context) -> Result<(Option<Segment>, Vec<Segment>), String> {
    let media = inherited(levels, "media").ok_or("a segment template has no media pattern")?;
    let timescale: u64 = inherited(levels, "timescale").and_then(|t| t.parse().ok()).unwrap_or(1).max(1);
    let start_number: u64 = inherited(levels, "startNumber").and_then(|n| n.parse().ok()).unwrap_or(1);
    let init = inherited(levels, "initialization")
        .map(|i| resolve(&cx.base, &fill(i, cx.id, start_number, 0, cx.bandwidth)))
        .transpose()?
        .map(Segment::whole);
    let make = |number: u64, time: u64| -> Result<Segment, String> {
        Ok(Segment::whole(resolve(&cx.base, &fill(media, cx.id, number, time, cx.bandwidth))?))
    };
    let mut segments = Vec::new();
    let timeline = levels.iter().rev().flatten().find_map(|n| child(*n, "SegmentTimeline"));
    if let Some(timeline) = timeline {
        let end = cx.duration.map(|d| (d * timescale as f64) as u64);
        let mut t = 0u64;
        let mut number = start_number;
        for s in children(timeline, "S") {
            if let Some(start) = s.attribute("t").and_then(|v| v.parse().ok()) {
                t = start;
            }
            let d: u64 = s.attribute("d").and_then(|v| v.parse().ok()).ok_or("a timeline entry has no duration")?;
            if d == 0 {
                return Err("a timeline entry has no duration".into());
            }
            let r: i64 = s.attribute("r").and_then(|v| v.parse().ok()).unwrap_or(0);
            let repeats = if r < 0 {
                let end = end.ok_or("a timeline repeats to the end, but the length is unknown")?;
                end.saturating_sub(t).div_ceil(d).saturating_sub(1)
            } else {
                r as u64
            };
            for _ in 0..=repeats {
                if segments.len() >= MAX_SEGMENTS {
                    return Err("the manifest lists too many segments".into());
                }
                segments.push(make(number, t)?);
                t += d;
                number += 1;
            }
        }
    } else {
        let d: u64 = inherited(levels, "duration")
            .and_then(|v| v.parse().ok())
            .filter(|d| *d > 0)
            .ok_or("a segment template has neither a duration nor a timeline")?;
        let total = cx.duration.ok_or("the manifest does not say how long the video is")?;
        let count = ((total * timescale as f64) / d as f64).ceil() as u64;
        if count as usize > MAX_SEGMENTS {
            return Err("the manifest lists too many segments".into());
        }
        for i in 0..count {
            segments.push(make(start_number + i, i * d)?);
        }
    }
    Ok((init, segments))
}

fn list_track(list: Node, cx: &Context) -> Result<(Option<Segment>, Vec<Segment>), String> {
    let init = child(list, "Initialization")
        .map(|i| -> Result<Segment, String> {
            Ok(Segment {
                url: resolve(&cx.base, i.attribute("sourceURL").unwrap_or(""))?,
                range: i.attribute("range").and_then(parse_range),
                key: None,
            })
        })
        .transpose()?;
    let segments = children(list, "SegmentURL")
        .map(|s| {
            Ok(Segment {
                url: resolve(&cx.base, s.attribute("media").unwrap_or(""))?,
                range: s.attribute("mediaRange").and_then(parse_range),
                key: None,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    if segments.is_empty() {
        return Err("a segment list is empty".into());
    }
    Ok((init, segments))
}

/// The `BaseURL` of `node` resolved against `base`.
fn base_of(node: Node, base: &str) -> Result<String, String> {
    match child(node, "BaseURL").and_then(|b| b.text()) {
        Some(url) => resolve(base, url),
        None => Ok(base.to_string()),
    }
}

/// Read a manifest fetched from `url`.
pub fn parse(text: &str, url: &str) -> Result<Manifest, String> {
    let doc = Document::parse(text.trim_start_matches('\u{feff}')).map_err(|e| format!("The manifest is not valid XML: {e}"))?;
    let mpd = doc.root_element();
    if mpd.tag_name().name() != "MPD" {
        return Err("This is not a DASH manifest.".into());
    }
    if mpd.attribute("type") == Some("dynamic") {
        return Err(LIVE_REFUSED.into());
    }
    let periods: Vec<Node> = children(mpd, "Period").collect();
    let period = match periods.as_slice() {
        [one] => *one,
        [] => return Err("The manifest has no periods.".into()),
        _ => {
            return Err(format!(
                "The video is split into {} periods (often ads between parts), which UIDM cannot join.",
                periods.len()
            ))
        }
    };
    let total = mpd.attribute("mediaPresentationDuration").and_then(parse_duration);
    let duration = period.attribute("duration").and_then(parse_duration).or_else(|| {
        let start = period.attribute("start").and_then(parse_duration).unwrap_or(0.0);
        total.map(|t| t - start)
    });

    let mpd_base = base_of(mpd, url)?;
    let period_base = base_of(period, &mpd_base)?;
    let mut manifest = Manifest {
        duration,
        ..Manifest::default()
    };
    for set in children(period, "AdaptationSet") {
        let set_mime = set.attribute("mimeType").unwrap_or("");
        let set_base = base_of(set, &period_base)?;
        let set_drm = child(set, "ContentProtection").is_some();
        for rep in children(set, "Representation") {
            let mime = rep.attribute("mimeType").unwrap_or(set_mime);
            let kind = set.attribute("contentType").unwrap_or_else(|| mime.split('/').next().unwrap_or(""));
            let content = match kind {
                "video" => Content::Video,
                "audio" => Content::Audio,
                _ => continue,
            };
            let id = rep.attribute("id").unwrap_or("").to_string();
            let bandwidth = rep.attribute("bandwidth").and_then(|b| b.parse().ok());
            let cx = Context {
                base: base_of(rep, &set_base)?,
                id: &id,
                bandwidth: bandwidth.unwrap_or(0),
                duration,
            };
            let level = |name: &str| [child(period, name), child(set, name), child(rep, name)];
            let templates = level("SegmentTemplate");
            let list = child(rep, "SegmentList").or_else(|| child(set, "SegmentList"));
            let parts = if templates.iter().any(Option::is_some) {
                template_track(&templates, &cx)
            } else if let Some(list) = list {
                list_track(list, &cx)
            } else {
                // SegmentBase, or nothing: the whole file is the track.
                Ok((None, vec![Segment::whole(cx.base.clone())]))
            };
            let webm = mime.ends_with("webm");
            let ext = match (content, webm) {
                (_, true) => "webm",
                (Content::Video, false) => "mp4",
                (Content::Audio, false) => "m4a",
            };
            let r = Representation {
                content,
                bandwidth,
                height: rep.attribute("height").or(set.attribute("height")).and_then(|h| h.parse().ok()),
                language: set.attribute("lang").map(str::to_string),
                drm: set_drm || child(rep, "ContentProtection").is_some(),
                track: parts.map(|(init, segments)| Track {
                    init,
                    segments,
                    ext,
                    duration,
                }),
                id,
            };
            match content {
                Content::Video => manifest.video.push(r),
                Content::Audio => manifest.audio.push(r),
            }
        }
    }
    if manifest.video.is_empty() && manifest.audio.is_empty() {
        return Err("The manifest has no video or audio.".into());
    }
    let best = |r: &Representation| std::cmp::Reverse((r.height.unwrap_or(0), r.bandwidth.unwrap_or(0)));
    manifest.video.sort_by_key(best);
    manifest.audio.sort_by_key(best);
    Ok(manifest)
}

fn options(reps: &[Representation], duration: Option<f64>) -> Vec<MediaOption> {
    reps.iter()
        .filter(|r| !r.drm && r.track.is_ok())
        .map(|r| MediaOption {
            id: r.id.clone(),
            label: match (&r.language, r.content) {
                (Some(lang), Content::Audio) => format!("{lang}, {}", super::quality_label(None, r.bandwidth)),
                _ => super::quality_label(r.height, r.bandwidth),
            },
            height: r.height,
            bandwidth: r.bandwidth,
            size: super::estimate_size(r.bandwidth, duration),
        })
        .collect()
}

/// The qualities and audio tracks a manifest offers that can be downloaded.
pub fn describe(m: &Manifest) -> MediaInfo {
    let video = options(&m.video, m.duration);
    let audio = options(&m.audio, m.duration);
    let all = m.video.iter().chain(&m.audio);
    let drm = video.is_empty() && audio.is_empty() && all.clone().any(|r| r.drm);
    let error = if drm {
        Some(DRM_REFUSED.to_string())
    } else if video.is_empty() && audio.is_empty() {
        all.filter_map(|r| r.track.as_ref().err()).next().cloned().or(Some("Nothing in this manifest can be downloaded.".into()))
    } else {
        None
    };
    MediaInfo {
        kind: Some(MediaKind::Dash),
        video,
        audio,
        duration: m.duration,
        drm,
        error,
        not_stream: false,
        file: None,
    }
}

/// The tracks to fetch for `video` and `audio` (option ids, or the best):
/// the video and, apart from it, the audio. A stream with no video gives its
/// audio alone.
pub fn pick(m: &Manifest, video: Option<&str>, audio: Option<&str>) -> Result<(Track, Option<Track>), String> {
    let usable = |reps: &[Representation], id: Option<&str>| -> Result<Option<Track>, String> {
        let ok: Vec<&Representation> = reps.iter().filter(|r| !r.drm && r.track.is_ok()).collect();
        if ok.is_empty() {
            if reps.iter().any(|r| r.drm) {
                return Err(DRM_REFUSED.into());
            }
            if let Some(Err(e)) = reps.first().map(|r| &r.track) {
                return Err(e.clone());
            }
            return Ok(None);
        }
        let r = id.and_then(|id| ok.iter().find(|r| r.id == id)).unwrap_or(&ok[0]);
        Ok(r.track.clone().ok())
    };
    let v = usable(&m.video, video)?;
    let a = usable(&m.audio, audio)?;
    match (v, a) {
        (Some(v), a) => Ok((v, a)),
        (None, Some(a)) => Ok((a, None)),
        (None, None) => Err("Nothing in this manifest can be downloaded.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://cdn.x/movie/manifest.mpd";

    #[test]
    fn durations() {
        assert_eq!(parse_duration("PT1H2M3.5S"), Some(3723.5));
        assert_eq!(parse_duration("PT0S"), Some(0.0));
        assert_eq!(parse_duration("P1DT1S"), Some(86401.0));
        assert_eq!(parse_duration("PT10M"), Some(600.0));
        assert_eq!(parse_duration("1H"), None);
        assert_eq!(parse_duration("PT1X"), None);
    }

    #[test]
    fn templates() {
        assert_eq!(fill("$RepresentationID$/seg-$Number%05d$.m4s", "v1", 42, 0, 0), "v1/seg-00042.m4s");
        assert_eq!(fill("t_$Time$_$Bandwidth$.m4s", "v", 1, 9000, 128000), "t_9000_128000.m4s");
        assert_eq!(fill("a$$b$Unknown$c", "v", 1, 0, 0), "a$b$Unknown$c");
        assert_eq!(fill("open$Number", "v", 1, 0, 0), "open$Number");
    }

    const TEMPLATE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT10S">
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <SegmentTemplate timescale="1000" duration="4000" startNumber="1"
          initialization="$RepresentationID$/init.mp4" media="$RepresentationID$/$Number%03d$.m4s"/>
      <Representation id="720" bandwidth="3000000" width="1280" height="720"/>
      <Representation id="1080" bandwidth="6000000" width="1920" height="1080"/>
    </AdaptationSet>
    <AdaptationSet contentType="audio" mimeType="audio/mp4" lang="en">
      <Representation id="aud" bandwidth="128000">
        <BaseURL>audio/</BaseURL>
        <SegmentTemplate timescale="48000" initialization="init.m4a" media="t$Time$.m4s">
          <SegmentTimeline>
            <S t="0" d="96000" r="3"/>
            <S d="48000"/>
          </SegmentTimeline>
        </SegmentTemplate>
      </Representation>
    </AdaptationSet>
    <AdaptationSet contentType="text" mimeType="text/vtt">
      <Representation id="subs" bandwidth="1"><BaseURL>subs.vtt</BaseURL></Representation>
    </AdaptationSet>
  </Period>
</MPD>"#;

    #[test]
    fn segment_templates() {
        let m = parse(TEMPLATE, URL).unwrap();
        assert_eq!(m.duration, Some(10.0));
        assert_eq!(m.video.len(), 2);
        assert_eq!(m.video[0].id, "1080", "best first");
        let v = m.video[0].track.as_ref().unwrap();
        assert_eq!(v.init.as_ref().unwrap().url, "https://cdn.x/movie/1080/init.mp4");
        let urls: Vec<&str> = v.segments.iter().map(|s| s.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://cdn.x/movie/1080/001.m4s",
                "https://cdn.x/movie/1080/002.m4s",
                "https://cdn.x/movie/1080/003.m4s"
            ]
        );
        assert_eq!(v.ext, "mp4");

        let a = m.audio[0].track.as_ref().unwrap();
        assert_eq!(a.init.as_ref().unwrap().url, "https://cdn.x/movie/audio/init.m4a");
        let urls: Vec<&str> = a.segments.iter().map(|s| s.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://cdn.x/movie/audio/t0.m4s",
                "https://cdn.x/movie/audio/t96000.m4s",
                "https://cdn.x/movie/audio/t192000.m4s",
                "https://cdn.x/movie/audio/t288000.m4s",
                "https://cdn.x/movie/audio/t384000.m4s"
            ]
        );
        assert_eq!(a.ext, "m4a");

        let info = describe(&m);
        assert_eq!(info.video.len(), 2);
        assert_eq!(info.audio[0].label, "en, 128 kbps");
        assert_eq!(info.video[0].size, Some(7_500_000));
        let (v, a) = pick(&m, Some("720"), None).unwrap();
        assert!(v.segments[0].url.ends_with("/720/001.m4s"));
        assert!(a.unwrap().segments[0].url.ends_with("t0.m4s"));
    }

    #[test]
    fn timeline_repeating_to_the_end() {
        let text = r#"<MPD type="static" mediaPresentationDuration="PT9S"><Period>
          <AdaptationSet mimeType="video/mp4"><Representation id="v" bandwidth="1">
            <SegmentTemplate timescale="1" media="$Number$.m4s" startNumber="5">
              <SegmentTimeline><S t="0" d="2" r="-1"/></SegmentTimeline>
            </SegmentTemplate>
          </Representation></AdaptationSet></Period></MPD>"#;
        let m = parse(text, URL).unwrap();
        let t = m.video[0].track.as_ref().unwrap();
        let names: Vec<&str> = t.segments.iter().map(|s| s.url.rsplit('/').next().unwrap()).collect();
        assert_eq!(names, ["5.m4s", "6.m4s", "7.m4s", "8.m4s", "9.m4s"]);
    }

    #[test]
    fn lists_and_single_files() {
        let text = r#"<MPD type="static" mediaPresentationDuration="PT8S">
          <BaseURL>https://media.x/</BaseURL>
          <Period>
            <AdaptationSet mimeType="video/mp4">
              <Representation id="v" bandwidth="1000">
                <SegmentList>
                  <Initialization sourceURL="v.mp4" range="0-499"/>
                  <SegmentURL media="v.mp4" mediaRange="500-999"/>
                  <SegmentURL media="v.mp4" mediaRange="1000-1499"/>
                </SegmentList>
              </Representation>
            </AdaptationSet>
            <AdaptationSet mimeType="audio/webm">
              <Representation id="a" bandwidth="64000">
                <BaseURL>a.webm</BaseURL>
                <SegmentBase indexRange="100-200"/>
              </Representation>
            </AdaptationSet>
          </Period></MPD>"#;
        let m = parse(text, URL).unwrap();
        let v = m.video[0].track.as_ref().unwrap();
        assert_eq!(v.init.as_ref().unwrap().range, Some((0, 499)));
        assert_eq!(v.segments[1].url, "https://media.x/v.mp4");
        assert_eq!(v.segments[1].range, Some((1000, 1499)));
        let a = m.audio[0].track.as_ref().unwrap();
        assert_eq!(a.segments, [Segment::whole("https://media.x/a.webm")]);
        assert_eq!(a.ext, "webm");
    }

    #[test]
    fn drm_live_and_periods_are_refused() {
        let drm = r#"<MPD type="static" mediaPresentationDuration="PT8S"><Period>
          <AdaptationSet mimeType="video/mp4">
            <ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"/>
            <Representation id="v" bandwidth="1"><BaseURL>v.mp4</BaseURL></Representation>
          </AdaptationSet></Period></MPD>"#;
        let m = parse(drm, URL).unwrap();
        let info = describe(&m);
        assert!(info.drm);
        assert_eq!(info.error.as_deref(), Some(DRM_REFUSED));
        assert_eq!(pick(&m, None, None).unwrap_err(), DRM_REFUSED);

        let live = r#"<MPD type="dynamic"><Period/></MPD>"#;
        assert_eq!(parse(live, URL).unwrap_err(), LIVE_REFUSED);
        let two = r#"<MPD type="static"><Period/><Period/></MPD>"#;
        assert!(parse(two, URL).unwrap_err().contains("2 periods"));
        assert!(parse("<html/>", URL).is_err());
        assert!(parse("not xml", URL).is_err());
    }
}
