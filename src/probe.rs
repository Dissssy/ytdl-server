//! Fetching a URL's info with yt-dlp and turning its formats into ranked choices.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::process::Stdio;

#[derive(Debug, Clone, Serialize)]
pub struct Info {
    pub url: String,
    pub title: String,
    pub thumbnail: Option<String>,
    pub duration: Option<f64>,
    pub site: String,
    pub uploader: Option<String>,
    #[serde(skip)]
    pub formats: Vec<Fmt>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fmt {
    pub id: String,
    pub ext: String,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub tbr: Option<f64>,
    pub size: Option<u64>,
    pub exact: bool,
}

#[derive(Deserialize)]
struct RawInfo {
    title: Option<String>,
    thumbnail: Option<String>,
    duration: Option<f64>,
    extractor_key: Option<String>,
    uploader: Option<String>,
    webpage_url: Option<String>,
    #[serde(default)]
    formats: Vec<RawFmt>,
    #[serde(flatten)]
    top: RawFmt,
}

#[derive(Deserialize, Default)]
struct RawFmt {
    format_id: Option<String>,
    ext: Option<String>,
    vcodec: Option<String>,
    acodec: Option<String>,
    height: Option<u32>,
    fps: Option<f64>,
    tbr: Option<f64>,
    abr: Option<f64>,
    vbr: Option<f64>,
    filesize: Option<f64>,
    filesize_approx: Option<f64>,
    protocol: Option<String>,
}

fn codec(c: &Option<String>) -> Option<String> {
    match c.as_deref() {
        None | Some("none") | Some("") => None,
        Some(c) => Some(c.to_string()),
    }
}

impl RawFmt {
    fn into_fmt(self, duration: Option<f64>) -> Option<Fmt> {
        let id = self.format_id?;
        let ext = self.ext.unwrap_or_default();
        if ext == "mhtml" || self.protocol.as_deref() == Some("mhtml") {
            return None; // storyboards
        }
        let vcodec = codec(&self.vcodec);
        let acodec = codec(&self.acodec);
        // Some sites give no codec info at all for a plain progressive file.
        let (vcodec, acodec) = match (&self.vcodec, &self.acodec, vcodec, acodec) {
            (None, None, None, None) => (Some("unknown".into()), Some("unknown".into())),
            (_, _, v, a) => (v, a),
        };
        let tbr = self.tbr.or(match (self.vbr, self.abr) {
            (Some(v), Some(a)) => Some(v + a),
            (v, a) => v.or(a),
        });
        let (size, exact) = match (self.filesize, self.filesize_approx) {
            (Some(s), _) => (Some(s as u64), true),
            (None, Some(s)) => (Some(s as u64), false),
            _ => (tbr.zip(duration).map(|(t, d)| (t * 125.0 * d) as u64), false),
        };
        Some(Fmt { id, ext, vcodec, acodec, height: self.height, fps: self.fps, tbr, size, exact })
    }
}

pub fn base_args() -> Vec<String> {
    let mut args: Vec<String> = ["--no-playlist", "--no-colors", "--force-ipv4"].map(String::from).to_vec();
    if let Ok(cookies) = std::env::var("COOKIES_PATH") {
        if std::path::Path::new(&cookies).exists() {
            args.push("--cookies".into());
            args.push(cookies);
        }
    }
    args
}

pub async fn probe(url: &str) -> Result<Info, crate::errors::Failure> {
    let out = tokio::process::Command::new("yt-dlp")
        .args(base_args())
        .args(["--dump-single-json", "--", url])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| crate::errors::Failure::internal(format!("couldn't run yt-dlp: {e}")))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(crate::errors::explain(stderr.lines()));
    }
    let raw: RawInfo = serde_json::from_slice(&out.stdout)
        .map_err(|e| crate::errors::Failure::internal(format!("unexpected yt-dlp output: {e}")))?;
    let duration = raw.duration;
    let mut formats: Vec<Fmt> = raw.formats.into_iter().filter_map(|f| f.into_fmt(duration)).collect();
    if formats.is_empty() {
        formats.extend(raw.top.into_fmt(duration));
    }
    Ok(Info {
        url: raw.webpage_url.unwrap_or_else(|| url.to_string()),
        title: raw.title.unwrap_or_else(|| "Untitled".into()),
        thumbnail: raw.thumbnail,
        duration,
        site: raw.extractor_key.unwrap_or_else(|| "Generic".into()),
        uploader: raw.uploader,
        formats,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// H.264/AAC MP4 that plays inline everywhere.
    Compat,
    /// Original codecs, highest quality.
    Best,
    /// MP3.
    Audio,
}

#[derive(Debug, Clone, Serialize)]
pub struct Choice {
    pub format: String,
    pub label: String,
    pub height: Option<u32>,
    /// Expected size of the finished file, in bytes.
    pub size: Option<u64>,
    pub exact: bool,
    pub reencode_video: bool,
    pub reencode_audio: bool,
    /// Set when re-encoding to a bitrate so the file fits a size cap.
    pub fit_kbps: Option<u32>,
    pub fit_height: Option<u32>,
}

fn is_h264(c: &Option<String>) -> bool {
    c.as_deref().is_some_and(|c| c.starts_with("avc") || c.starts_with("h264"))
}
fn is_aac(c: &Option<String>) -> bool {
    c.as_deref().is_some_and(|c| c.starts_with("mp4a") || c == "aac")
}
fn has_video(f: &Fmt) -> bool {
    f.vcodec.is_some()
}
fn audio_only(f: &Fmt) -> bool {
    f.acodec.is_some() && f.vcodec.is_none()
}
fn video_only(f: &Fmt) -> bool {
    f.vcodec.is_some() && f.acodec.is_none()
}

fn codec_name(c: &Option<String>) -> &'static str {
    let c = c.as_deref().unwrap_or("");
    match () {
        _ if c.starts_with("avc") || c.starts_with("h264") => "H.264",
        _ if c.starts_with("hev") || c.starts_with("hvc") || c.starts_with("h265") => "H.265",
        _ if c.starts_with("vp9") || c.starts_with("vp09") => "VP9",
        _ if c.starts_with("av01") => "AV1",
        _ if c.starts_with("vp8") => "VP8",
        _ => "video",
    }
}

fn label(height: Option<u32>, vcodec: &Option<String>) -> String {
    match height {
        Some(h) => format!("{h}p {}", codec_name(vcodec)),
        None => codec_name(vcodec).to_string(),
    }
}

fn add(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    Some(a? + b.unwrap_or(0))
}

const MP3_KBPS: f64 = 192.0;

/// Every sensible option for a mode, best quality first, at most one per height.
pub fn ladder(info: &Info, mode: Mode) -> Vec<Choice> {
    if mode == Mode::Audio {
        let size = info.duration.map(|d| (d * MP3_KBPS * 125.0) as u64);
        return vec![Choice {
            format: "ba/b".into(),
            label: "MP3".into(),
            height: None,
            size,
            exact: false,
            reencode_video: false,
            reencode_audio: false,
            fit_kbps: None,
            fit_height: None,
        }];
    }
    let best_audio = |aac_only: bool| {
        info.formats
            .iter()
            .filter(|f| audio_only(f) && (!aac_only || is_aac(&f.acodec)))
            .max_by(|a, b| a.tbr.unwrap_or(0.0).total_cmp(&b.tbr.unwrap_or(0.0)))
    };
    let audio = match mode {
        Mode::Compat => best_audio(true).or_else(|| best_audio(false)),
        _ => best_audio(false),
    };

    let mut options: Vec<Choice> = info
        .formats
        .iter()
        .filter(|f| has_video(f))
        .map(|v| {
            let (format, acodec, size, exact) = match (video_only(v), audio) {
                (true, Some(a)) => (format!("{}+{}", v.id, a.id), a.acodec.clone(), add(v.size, a.size), v.exact && a.exact),
                _ => (v.id.clone(), v.acodec.clone(), v.size, v.exact),
            };
            let compat = mode == Mode::Compat;
            Choice {
                format,
                label: label(v.height, &v.vcodec),
                height: v.height,
                size,
                exact,
                reencode_video: compat && !is_h264(&v.vcodec) && v.vcodec.as_deref() != Some("unknown"),
                reencode_audio: compat && acodec.is_some() && !is_aac(&acodec) && acodec.as_deref() != Some("unknown"),
                fit_kbps: None,
                fit_height: None,
            }
        })
        .collect();

    // Per height keep the best: no re-encode first, then the larger (higher bitrate) file.
    options.sort_by(|a, b| {
        b.height
            .cmp(&a.height)
            .then((a.reencode_video as u8).cmp(&(b.reencode_video as u8)))
            .then(match mode {
                Mode::Compat => (a.reencode_audio as u8).cmp(&(b.reencode_audio as u8)),
                _ => std::cmp::Ordering::Equal,
            })
            .then(b.size.unwrap_or(0).cmp(&a.size.unwrap_or(0)))
    });
    options.dedup_by_key(|c| c.height);
    options
}

/// The best option under `cap` bytes; if nothing fits, a re-encode sized to fit.
pub fn select(info: &Info, mode: Mode, cap: Option<u64>) -> Result<Choice> {
    let options = ladder(info, mode);
    let fits = |c: &Choice| match (cap, c.size) {
        (Some(cap), Some(size)) => size <= cap,
        _ => true,
    };
    if let Some(c) = options.iter().find(|c| fits(c)) {
        return Ok(c.clone());
    }
    let cap = cap.unwrap_or(u64::MAX);
    let duration = info.duration.ok_or_else(|| anyhow!("nothing fits under the size cap, and the length is unknown so it can't be re-encoded to fit"))?;
    // Leave ~8% for container overhead and rate-control overshoot.
    let total_kbps = cap as f64 * 8.0 / 1000.0 / duration * 0.92;
    let video_kbps = total_kbps - 96.0;
    if video_kbps < 80.0 {
        return Err(anyhow!("too long to fit under the size cap at any watchable quality"));
    }
    let fit_height = match video_kbps {
        k if k < 300.0 => 240,
        k if k < 600.0 => 360,
        k if k < 1200.0 => 480,
        k if k < 2500.0 => 720,
        _ => 1080,
    };
    // Smallest source that is at least the target height, so we never upscale or download 4K to shrink it.
    let source = options
        .iter()
        .rev()
        .find(|c| c.height.unwrap_or(0) >= fit_height)
        .or(options.first())
        .ok_or_else(|| anyhow!("no video formats found"))?;
    Ok(Choice {
        label: format!("{}p H.264, re-encoded to fit", fit_height.min(source.height.unwrap_or(fit_height))),
        size: Some(cap.min((total_kbps * 125.0 * duration) as u64)),
        exact: false,
        reencode_video: true,
        reencode_audio: true,
        fit_kbps: Some(video_kbps as u32),
        fit_height: Some(fit_height),
        ..source.clone()
    })
}
