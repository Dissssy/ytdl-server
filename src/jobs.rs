//! The job queue: one running job per user, a small global cap, progress via a broadcast channel.

use crate::{
    config,
    errors::Failure,
    probe::{self, Choice, Info, Mode},
};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::{broadcast, Notify},
};

/// Jobs running at once across all users.
const MAX_RUNNING: usize = 2;
/// A saved file is kept this long in case it's wanted again.
const KEEP_SAVED: Duration = Duration::from_secs(60 * 60);
/// An unclaimed file, and any finished job's record, is kept this long.
const KEEP_UNCLAIMED: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Queued,
    Fetching,
    Processing,
    /// Browser job finished, waiting to be saved.
    Ready,
    /// Browser job whose file was sent completely.
    Saved,
    /// NAS job finished.
    Done,
    Failed,
    Cancelled,
    /// File deleted after its retention time.
    Expired,
}

impl State {
    pub fn running(self) -> bool {
        matches!(self, State::Fetching | State::Processing)
    }
    pub fn finished(self) -> bool {
        !matches!(self, State::Queued | State::Fetching | State::Processing)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Browser,
    Nas,
}

/// What to download.
#[derive(Debug, Clone)]
pub enum Plan {
    /// The original /download and /download_now behaviour.
    Legacy { audio: bool, quality: LegacyQuality },
    /// A mode plus optional cap or explicit format, resolved after probing.
    Pick { mode: Mode, cap: Option<u64>, format: Option<String> },
}

#[derive(Debug, Clone, Copy)]
pub enum LegacyQuality {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    pub downloaded: u64,
    pub total: Option<u64>,
    /// Bytes per second.
    pub speed: Option<f64>,
    /// Seconds left.
    pub eta: Option<f64>,
    /// 0..1 during processing.
    pub processing: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct View {
    pub id: String,
    #[serde(skip)]
    pub owner: String,
    pub url: String,
    pub title: Option<String>,
    pub thumbnail: Option<String>,
    pub target: Target,
    pub audio: bool,
    pub choice: Option<String>,
    pub state: State,
    pub progress: Progress,
    pub error: Option<Failure>,
    pub filename: Option<String>,
    pub size: Option<u64>,
    /// Unix seconds.
    pub created: u64,
    pub expires: Option<u64>,
}

struct Job {
    view: View,
    plan: Plan,
    created: Instant,
    finished: Option<Instant>,
    saved: Option<Instant>,
    file: Option<PathBuf>,
    cancel: Arc<Notify>,
    last_emit: Instant,
}

pub struct Jobs {
    pub config: config::Store,
    jobs: Mutex<HashMap<String, Job>>,
    wake: Notify,
    pub events: broadcast::Sender<View>,
    pub stats: Mutex<Stats>,
    probes: Mutex<HashMap<String, (Instant, Arc<Info>)>>,
}

/// Rolling averages used for time estimates.
#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    /// Download speed per site, bytes per second.
    pub speed: HashMap<String, f64>,
    pub default_speed: f64,
    /// Seconds of encoding per second of video.
    pub encode_rate: f64,
}

impl Default for Stats {
    fn default() -> Self {
        Self { speed: HashMap::new(), default_speed: 4.0e6, encode_rate: 0.5 }
    }
}

impl Stats {
    pub fn speed_for(&self, site: &str) -> f64 {
        self.speed.get(site).copied().unwrap_or(self.default_speed)
    }
    fn record_speed(&mut self, site: &str, bps: f64) {
        if !(bps.is_finite() && bps > 0.0) {
            return;
        }
        let entry = self.speed.entry(site.to_string()).or_insert(bps);
        *entry = *entry * 0.7 + bps * 0.3;
        self.default_speed = self.default_speed * 0.9 + bps * 0.1;
    }
    fn record_encode(&mut self, rate: f64) {
        if rate.is_finite() && rate > 0.0 {
            self.encode_rate = self.encode_rate * 0.7 + rate * 0.3;
        }
    }
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn unix_in(d: Duration) -> u64 {
    now_unix() + d.as_secs()
}

impl Jobs {
    pub fn new(config: config::Store) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        Arc::new(Self {
            config,
            jobs: Mutex::new(HashMap::new()),
            wake: Notify::new(),
            events,
            stats: Mutex::new(Stats::default()),
            probes: Mutex::new(HashMap::new()),
        })
    }

    pub fn start(self: &Arc<Self>) {
        let tmp = self.config.get().tmp_dir.clone();
        if let Ok(entries) = std::fs::read_dir(&tmp) {
            for entry in entries.flatten() {
                let path = entry.path();
                let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            }
        }
        tokio::spawn(self.clone().scheduler());
        tokio::spawn(self.clone().sweeper());
        tokio::spawn(update_ytdlp());
    }

    pub fn submit(&self, owner: &str, url: String, target: Target, plan: Plan) -> View {
        let id = uuid::Uuid::new_v4().to_string();
        let audio = match &plan {
            Plan::Legacy { audio, .. } => *audio,
            Plan::Pick { mode, .. } => *mode == Mode::Audio,
        };
        // A recent probe of the same URL gives the list a title and picture right away.
        let known = self.probes.lock().unwrap().get(&format!("{owner}\n{url}")).map(|(_, i)| (i.title.clone(), i.thumbnail.clone()));
        let (title, thumbnail) = known.map_or((None, None), |(t, th)| (Some(t), th));
        let view = View {
            id: id.clone(),
            owner: owner.to_string(),
            url,
            title,
            thumbnail,
            target,
            audio,
            choice: None,
            state: State::Queued,
            progress: Progress::default(),
            error: None,
            filename: None,
            size: None,
            created: now_unix(),
            expires: None,
        };
        let job = Job {
            view: view.clone(),
            plan,
            created: Instant::now(),
            finished: None,
            saved: None,
            file: None,
            cancel: Arc::new(Notify::new()),
            last_emit: Instant::now(),
        };
        self.jobs.lock().unwrap().insert(id, job);
        let _ = self.events.send(view.clone());
        self.wake.notify_one();
        view
    }

    pub fn get(&self, id: &str) -> Option<View> {
        self.jobs.lock().unwrap().get(id).map(|j| j.view.clone())
    }

    pub fn list(&self, owner: &str) -> Vec<View> {
        let mut list: Vec<View> = self.jobs.lock().unwrap().values().filter(|j| j.view.owner == owner).map(|j| j.view.clone()).collect();
        list.sort_by(|a, b| b.created.cmp(&a.created));
        list
    }

    /// The finished file for an owner's ready job.
    pub fn file(&self, id: &str, owner: &str) -> Option<(PathBuf, String)> {
        let jobs = self.jobs.lock().unwrap();
        let job = jobs.get(id).filter(|j| j.view.owner == owner)?;
        matches!(job.view.state, State::Ready | State::Saved).then_some(())?;
        Some((job.file.clone()?, job.view.filename.clone()?))
    }

    pub fn mark_saved(&self, id: &str) {
        self.update(id, |job| {
            if job.view.state == State::Ready {
                job.view.state = State::Saved;
                job.saved = Some(Instant::now());
                job.view.expires = Some(unix_in(KEEP_SAVED));
            }
        });
    }

    /// Cancels a queued or running job, or removes a finished one.
    pub fn remove(&self, id: &str, owner: &str) -> bool {
        let mut jobs = self.jobs.lock().unwrap();
        let Some(job) = jobs.get_mut(id).filter(|j| j.view.owner == owner) else { return false };
        if job.view.state.finished() {
            let job = jobs.remove(id).unwrap();
            drop(jobs);
            remove_workdir(&job.view.id, &self.config.get().tmp_dir);
            let mut view = job.view;
            view.state = State::Expired;
            view.progress = Progress::default();
            let _ = self.events.send(view);
        } else {
            job.cancel.notify_one();
            if job.view.state == State::Queued {
                job.view.state = State::Cancelled;
                job.finished = Some(Instant::now());
                let _ = self.events.send(job.view.clone());
            }
        }
        true
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Job)) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(job) = jobs.get_mut(id) {
            let before = job.view.state;
            f(job);
            if job.view.state.finished() && job.finished.is_none() {
                job.finished = Some(Instant::now());
            }
            // Progress is throttled; state changes always go out.
            if before != job.view.state || job.last_emit.elapsed() >= Duration::from_millis(400) {
                job.last_emit = Instant::now();
                let _ = self.events.send(job.view.clone());
            }
        }
    }

    /// Cached per user, since each user's cookies can change what a site shows.
    pub async fn probe(&self, url: &str, user: &str) -> Result<Arc<Info>, Failure> {
        let cache_key = format!("{user}\n{url}");
        {
            let mut cache = self.probes.lock().unwrap();
            cache.retain(|_, (t, _)| t.elapsed() < Duration::from_secs(600));
            if let Some((_, info)) = cache.get(&cache_key) {
                return Ok(info.clone());
            }
        }
        let info = Arc::new(probe::probe(url, user).await?);
        self.probes.lock().unwrap().insert(cache_key, (Instant::now(), info.clone()));
        Ok(info)
    }

    /// Drops a user's cached probes, after their cookies change.
    pub fn forget_probes(&self, user: &str) {
        let prefix = format!("{user}\n");
        self.probes.lock().unwrap().retain(|k, _| !k.starts_with(&prefix));
    }

    async fn scheduler(self: Arc<Self>) {
        loop {
            let next: Vec<String> = {
                let jobs = self.jobs.lock().unwrap();
                let mut running = jobs.values().filter(|j| j.view.state.running()).count();
                let mut busy: Vec<&str> = jobs.values().filter(|j| j.view.state.running()).map(|j| j.view.owner.as_str()).collect();
                let mut queued: Vec<&Job> = jobs.values().filter(|j| j.view.state == State::Queued).collect();
                queued.sort_by_key(|j| j.created);
                let mut picked = vec![];
                for job in queued {
                    if running >= MAX_RUNNING {
                        break;
                    }
                    if !busy.contains(&job.view.owner.as_str()) {
                        busy.push(&job.view.owner);
                        running += 1;
                        picked.push(job.view.id.clone());
                    }
                }
                picked
            };
            for id in next {
                self.update(&id, |j| j.view.state = State::Fetching);
                let this = self.clone();
                tokio::spawn(async move {
                    this.run(&id).await;
                    this.wake.notify_one();
                });
            }
            self.wake.notified().await;
        }
    }

    async fn sweeper(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            let tmp = self.config.get().tmp_dir.clone();
            let mut expired = vec![];
            let mut dropped = vec![];
            {
                let jobs = self.jobs.lock().unwrap();
                for job in jobs.values() {
                    let since = |t: Option<Instant>| t.map(|t| t.elapsed());
                    let gone = match job.view.state {
                        State::Saved => since(job.saved).is_some_and(|d| d > KEEP_SAVED),
                        State::Ready => since(job.finished).is_some_and(|d| d > KEEP_UNCLAIMED),
                        _ => false,
                    };
                    if gone {
                        expired.push(job.view.id.clone());
                    } else if job.view.state.finished() && since(job.finished).is_some_and(|d| d > KEEP_UNCLAIMED) {
                        dropped.push(job.view.id.clone());
                    }
                }
            }
            for id in expired {
                remove_workdir(&id, &tmp);
                self.update(&id, |j| {
                    j.view.state = State::Expired;
                    j.file = None;
                    j.view.expires = None;
                });
            }
            for id in dropped {
                remove_workdir(&id, &tmp);
                if let Some(job) = self.jobs.lock().unwrap().remove(&id) {
                    let mut view = job.view;
                    view.state = State::Expired;
                    let _ = self.events.send(view);
                }
            }
        }
    }

    async fn run(&self, id: &str) {
        let Some((url, owner, target, plan, cancel)) = ({
            let jobs = self.jobs.lock().unwrap();
            jobs.get(id).map(|j| (j.view.url.clone(), j.view.owner.clone(), j.view.target, j.plan.clone(), j.cancel.clone()))
        }) else {
            return;
        };
        let config = self.config.get();
        let workdir = config.tmp_dir.join(id);
        let result = tokio::select! {
            r = self.execute(id, &url, &owner, target, &plan, &workdir, &config) => r,
            _ = cancel.notified() => Err(Failure::new("Cancelled.", "")),
        };
        match result {
            Ok(()) => {}
            Err(failure) => {
                remove_workdir(id, &config.tmp_dir);
                let cancelled = failure.message == "Cancelled.";
                println!("job {id} {}: {failure}", if cancelled { "cancelled" } else { "failed" });
                self.update(id, |j| {
                    j.view.state = if cancelled { State::Cancelled } else { State::Failed };
                    j.view.error = (!cancelled).then_some(failure);
                    j.view.expires = Some(unix_in(KEEP_UNCLAIMED));
                });
            }
        }
    }

    async fn execute(&self, id: &str, url: &str, owner: &str, target: Target, plan: &Plan, workdir: &Path, config: &config::Config) -> Result<(), Failure> {
        tokio::fs::create_dir_all(workdir).await.map_err(|e| Failure::internal(format!("creating {}: {e}", workdir.display())))?;

        // Resolve what to fetch.
        let (format_args, choice, info) = match plan {
            Plan::Legacy { audio: true, .. } => (vec!["-f", "ba/b", "-x", "--audio-format", "mp3"].into_iter().map(String::from).collect::<Vec<_>>(), None, None),
            Plan::Legacy { audio: false, quality } => {
                let sort = match quality {
                    LegacyQuality::High => "res,ext:mp4:m4a",
                    LegacyQuality::Medium => "res:480,ext:mp4:m4a",
                    LegacyQuality::Low => "res:240,ext:mp4:m4a",
                };
                (vec!["-S".into(), sort.into(), "--remux-video".into(), "mp4".into()], None, None)
            }
            Plan::Pick { mode, cap, format } => {
                let info = self.probe(url, owner).await?;
                self.update(id, |j| {
                    j.view.title = Some(info.title.clone());
                    j.view.thumbnail = info.thumbnail.clone();
                });
                let cap = cap.map(|c| c.min(config.max_file_size));
                let choice = match format {
                    Some(f) => probe::ladder(&info, *mode).into_iter().find(|c| &c.format == f).unwrap_or(Choice {
                        format: f.clone(),
                        label: format!("format {f}"),
                        height: None,
                        size: None,
                        exact: false,
                        reencode_video: false,
                        reencode_audio: false,
                        fit_kbps: None,
                        fit_height: None,
                    }),
                    None => probe::select(&info, *mode, cap).map_err(|e| Failure::new("Nothing fits under that size.", e.to_string()))?,
                };
                let mut args = vec!["-f".to_string(), choice.format.clone()];
                match mode {
                    Mode::Audio => args.extend(["-x", "--audio-format", "mp3", "--audio-quality", "192K"].map(String::from)),
                    Mode::Compat if choice.reencode_video || choice.reencode_audio || choice.fit_kbps.is_some() => {
                        args.extend(["--merge-output-format", "mkv"].map(String::from))
                    }
                    Mode::Compat => args.extend(["--merge-output-format", "mp4"].map(String::from)),
                    Mode::Best => args.extend(["--merge-output-format", "mp4/mkv"].map(String::from)),
                }
                let label = choice.label.clone();
                self.update(id, |j| {
                    j.view.choice = Some(label);
                    j.view.progress.total = choice.size;
                });
                (args, Some(choice), Some(info))
            }
        };

        let template = match target {
            Target::Nas => format!("{id}.%(ext)s"),
            Target::Browser => "%(title).120B [%(id)s].%(ext)s".to_string(),
        };
        let mut cmd = tokio::process::Command::new("yt-dlp");
        cmd.args(probe::base_args(owner))
            .args(["--newline", "--progress", "--windows-filenames", "--embed-metadata"])
            .args(["--max-filesize".to_string(), config.max_file_size.to_string()])
            .args(["-P".to_string(), workdir.display().to_string(), "-o".to_string(), template])
            .args(["--print", "before_dl:INFO %(.{title,thumbnail,extractor_key,duration})j"])
            .args(["--print", "after_move:FILE %(filepath)s"])
            .args([
                "--progress-template",
                "download:PROG %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s %(progress.speed)s %(progress.eta)s",
            ])
            .args(["--progress-template", "postprocess:POST %(progress.postprocessor)s"])
            .args(&format_args);
        if target == Target::Nas {
            cmd.arg("--embed-thumbnail");
        }
        cmd.arg("--").arg(url).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);

        let started = Instant::now();
        let mut child = cmd.spawn().map_err(|e| Failure::internal(format!("couldn't run yt-dlp: {e}")))?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let push_log = |log: &Arc<Mutex<Vec<String>>>, line: String| {
            let mut log = log.lock().unwrap();
            log.push(line);
            if log.len() > 60 {
                log.remove(0);
            }
        };
        let err_log = log.clone();
        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut log = err_log.lock().unwrap();
                log.push(line);
                if log.len() > 60 {
                    log.remove(0);
                }
            }
        });

        let mut file: Option<PathBuf> = None;
        let mut site = info.as_ref().map(|i| i.site.clone()).unwrap_or_else(|| "Generic".into());
        let mut duration = info.as_ref().and_then(|i| i.duration);
        let expected = choice.as_ref().and_then(|c| c.size);
        // Multi-stream downloads (video then audio) restart the byte counter per stream.
        let (mut done_streams, mut last_bytes, mut last_total) = (0u64, 0u64, 0u64);
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(rest) = line.strip_prefix("PROG ") {
                let mut parts = rest.split(' ').map(|p| p.parse::<f64>().ok());
                let bytes = parts.next().flatten().unwrap_or(0.0) as u64;
                let total = parts.next().flatten().or(parts.next().flatten()).map(|t| t as u64);
                let speed = parts.next().flatten();
                let eta = parts.next().flatten();
                if bytes + 1024 < last_bytes {
                    done_streams += last_total.max(last_bytes);
                }
                last_bytes = bytes;
                last_total = total.unwrap_or(bytes);
                let downloaded = done_streams + bytes;
                let total = match (expected, total) {
                    (Some(e), _) if e >= downloaded => Some(e),
                    (_, Some(t)) => Some(done_streams + t),
                    _ => None,
                };
                self.update(id, |j| {
                    j.view.progress.downloaded = downloaded;
                    j.view.progress.total = total;
                    j.view.progress.speed = speed;
                    j.view.progress.eta = match (total, speed) {
                        (Some(t), Some(s)) if s > 0.0 => Some(t.saturating_sub(downloaded) as f64 / s),
                        _ => eta,
                    };
                });
            } else if let Some(rest) = line.strip_prefix("POST ") {
                let _ = rest;
                self.update(id, |j| j.view.state = State::Processing);
            } else if let Some(rest) = line.strip_prefix("INFO ") {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(rest) {
                    if let Some(s) = v["extractor_key"].as_str() {
                        site = s.to_string();
                    }
                    duration = duration.or(v["duration"].as_f64());
                    let title = v["title"].as_str().map(String::from);
                    let thumb = v["thumbnail"].as_str().map(String::from);
                    self.update(id, |j| {
                        j.view.title = j.view.title.take().or(title);
                        j.view.thumbnail = j.view.thumbnail.take().or(thumb);
                    });
                }
            } else if let Some(rest) = line.strip_prefix("FILE ") {
                file = Some(PathBuf::from(rest));
            } else {
                push_log(&log, line);
            }
        }
        let status = child.wait().await.map_err(|e| Failure::internal(e.to_string()))?;
        let _ = stderr_task.await;
        let download_secs = started.elapsed().as_secs_f64();
        let lines = log.lock().unwrap().clone();
        if !status.success() {
            return Err(crate::errors::explain(lines.iter().map(String::as_str), owner));
        }
        let mut file = match file.filter(|f| f.exists()) {
            Some(f) => f,
            None => return Err(crate::errors::explain(lines.iter().map(String::as_str), owner)),
        };
        let downloaded = done_streams + last_bytes;
        if downloaded > 0 && download_secs > 0.5 {
            self.stats.lock().unwrap().record_speed(&site, downloaded as f64 / download_secs);
        }

        // Re-encode for compatibility or to fit a size cap.
        if let Some(choice) = choice.filter(|c| c.reencode_video || c.reencode_audio || c.fit_kbps.is_some()) {
            self.update(id, |j| {
                j.view.state = State::Processing;
                j.view.progress.processing = Some(0.0);
            });
            let encode_started = Instant::now();
            file = self.encode(id, &file, &choice, duration).await?;
            if let Some(d) = duration.filter(|d| *d > 0.0) {
                self.stats.lock().unwrap().record_encode(encode_started.elapsed().as_secs_f64() / d);
            }
        }

        let size = tokio::fs::metadata(&file).await.map(|m| m.len()).ok();
        let filename = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "download".into());
        match target {
            Target::Nas => {
                let dir = if plan_is_audio(plan) { &config.audio_dir } else { &config.video_dir };
                let dest = dir.join(&filename);
                move_file(&file, &dest).await.map_err(|e| Failure::internal(format!("moving to {}: {e}", dest.display())))?;
                remove_workdir(id, &config.tmp_dir);
                println!("job {id} saved to {}", dest.display());
                self.update(id, |j| {
                    j.view.state = State::Done;
                    j.view.progress = Progress { downloaded: size.unwrap_or(0), total: size, ..Default::default() };
                    j.view.filename = Some(filename);
                    j.view.size = size;
                    j.view.expires = Some(unix_in(KEEP_UNCLAIMED));
                });
            }
            Target::Browser => {
                println!("job {id} ready: {filename}");
                self.update(id, |j| {
                    j.view.state = State::Ready;
                    j.view.progress = Progress { downloaded: size.unwrap_or(0), total: size, ..Default::default() };
                    j.view.filename = Some(filename);
                    j.view.size = size;
                    j.view.expires = Some(unix_in(KEEP_UNCLAIMED));
                    j.file = Some(file);
                });
            }
        }
        Ok(())
    }

    async fn encode(&self, id: &str, input: &Path, choice: &Choice, duration: Option<f64>) -> Result<PathBuf, Failure> {
        let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| id.to_string());
        let output = input.with_file_name(format!("{stem}.mp4"));
        let output = if output == input { input.with_file_name(format!("{stem}.encoded.mp4")) } else { output };
        let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-y", "-i"].map(String::from).to_vec();
        args.push(input.display().to_string());
        args.extend(["-map", "0:v:0?", "-map", "0:a:0?"].map(String::from));
        if let Some(kbps) = choice.fit_kbps {
            args.extend(["-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p"].map(String::from));
            args.extend(["-b:v".into(), format!("{kbps}k"), "-maxrate".into(), format!("{}k", kbps * 6 / 5), "-bufsize".into(), format!("{}k", kbps * 2)]);
            if let Some(h) = choice.fit_height {
                args.extend(["-vf".into(), format!("scale=-2:min(ih\\,{h})")]);
            }
            args.extend(["-c:a", "aac", "-b:a", "96k"].map(String::from));
        } else {
            if choice.reencode_video {
                args.extend(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p"].map(String::from));
            } else {
                args.extend(["-c:v", "copy"].map(String::from));
            }
            if choice.reencode_audio {
                args.extend(["-c:a", "aac", "-b:a", "128k"].map(String::from));
            } else {
                args.extend(["-c:a", "copy"].map(String::from));
            }
        }
        args.extend(["-movflags", "+faststart", "-progress", "pipe:1", "-nostats"].map(String::from));
        args.push(output.display().to_string());

        let mut child = tokio::process::Command::new("ffmpeg")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Failure::internal(format!("couldn't run ffmpeg: {e}")))?;
        let stderr = child.stderr.take().unwrap();
        let stderr_task = tokio::spawn(async move {
            let mut tail = Vec::new();
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tail.push(line);
                if tail.len() > 20 {
                    tail.remove(0);
                }
            }
            tail
        });
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let (Some(us), Some(d)) = (line.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok()), duration) {
                let fraction = (us / 1e6 / d).clamp(0.0, 1.0);
                self.update(id, |j| j.view.progress.processing = Some(fraction));
            }
        }
        let status = child.wait().await.map_err(|e| Failure::internal(e.to_string()))?;
        let tail = stderr_task.await.unwrap_or_default();
        if !status.success() {
            return Err(Failure::new("Converting the video failed.", tail.last().cloned().unwrap_or_default()));
        }
        let _ = tokio::fs::remove_file(input).await;
        Ok(output)
    }
}

fn plan_is_audio(plan: &Plan) -> bool {
    match plan {
        Plan::Legacy { audio, .. } => *audio,
        Plan::Pick { mode, .. } => *mode == Mode::Audio,
    }
}

fn remove_workdir(id: &str, tmp: &Path) {
    let dir = tmp.join(id);
    if dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            println!("couldn't remove {}: {e}", dir.display());
        }
    }
}

async fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if tokio::fs::rename(from, to).await.is_ok() {
        return Ok(());
    }
    // Different filesystems: copy, then delete.
    tokio::fs::copy(from, to).await?;
    tokio::fs::remove_file(from).await
}

/// Upgrades yt-dlp at startup and daily; sites break it often.
async fn update_ytdlp() {
    let mut tick = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
    loop {
        tick.tick().await;
        let out = tokio::process::Command::new("python")
            .args(["-m", "pip", "install", "--quiet", "--upgrade", "--pre", "yt-dlp[default,curl-cffi]"])
            .stdin(Stdio::null())
            .output()
            .await;
        match out {
            Ok(o) if o.status.success() => println!("yt-dlp is up to date"),
            Ok(o) => println!("yt-dlp update failed: {}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("")),
            Err(e) => println!("yt-dlp update failed: {e}"),
        }
    }
}
