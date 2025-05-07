mod config;
use actix_web::{get, web, HttpResponse, Responder};
use anyhow::{anyhow, Error};
use config::safe_get;
use lazy_static::lazy_static;
use notify::Watcher;
use rand::seq::SliceRandom;
use serde::Deserialize;
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::{sync::Mutex, task::JoinHandle};
use ytd_rs::Arg;

lazy_static! {
    static ref PATH: PathBuf = get_path().unwrap();
}

lazy_static! {
    static ref CONFIG: config::ConfigLoader = config::ConfigLoader::new();
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    match (args.get(1).map(|s| s.as_str()), args.get(2)) {
        (_, Some(_)) => {
            println!("Invalid arguments");
            return;
        }
        (Some("-a"), _) => {
            let user: String = safe_get("Username");
            let key: String = generate_api_key();
            let mut config = CONFIG.get().await;
            let invite_link = format!(
                "https://ytdlp.deadlyneurotox.in/invite.html?key={}",
                urlencoding::encode(&key)
            );
            config.auth_keys.push(config::AuthInfo { user, key });
            let file = std::fs::File::create(PATH.join("config.json")).unwrap();
            serde_json::to_writer_pretty(file, &config).unwrap();
            println!("User added, invite link: {}", invite_link);
            return;
        }
        (Some("-r"), _) => {
            let config = CONFIG.get().await;
            for user in config.auth_keys.iter() {
                println!("{}", user.user);
            }
            let user: String = safe_get("Username");
            let mut config = config.clone();
            let index = config
                .auth_keys
                .iter()
                .position(|x| x.user == user)
                .unwrap_or_else(|| {
                    println!("User not found");
                    std::process::exit(1);
                });
            config.auth_keys.remove(index);
            let file = std::fs::File::create(PATH.join("config.json")).unwrap();
            serde_json::to_writer_pretty(file, &config).unwrap();
            return;
        }
        (Some("-l"), _) => {
            for user in CONFIG.get().await.auth_keys.iter() {
                println!("{}", user.user);
            }
            return;
        }
        _ => {
            println!(
                "{}",
                ytd_rs::YoutubeDL::new(&PATH, vec![Arg::new("--version")], "",)
                    .expect("Failed to create ytdl object")
                    .download()
                    .expect("Failed to download")
                    .output()
            );
        }
    }

    let joinhandles: Arc<Mutex<Handles>> = Arc::new(Mutex::new(Handles::new()));
    let joinhandles2 = joinhandles.clone();
    let joinhandles3 = joinhandles.clone();
    tokio::spawn(check_finished(joinhandles3));
    actix_web::HttpServer::new(move || {
        actix_web::App::new()
            .app_data(web::Data::new(joinhandles2.clone()))
            .service(get)
            .service(check)
            .service(get_now)
            .service(get_info_route)
    }).client_request_timeout(std::time::Duration::from_secs(0))
    .bind(format!(
        "{}:{}",
        CONFIG.get().await.host,
        CONFIG.get().await.port
    ))
    .unwrap()
    .run()
    .await
    .unwrap();

    let mut joinhandles = joinhandles.lock().await;
    for (id, handle) in joinhandles.handles.drain() {
        let x = handle.joinhandle.await;
        match x {
            Ok(Ok(_)) => println!("{id} finished successfully"),
            Ok(Err(e)) => println!("{} failed: {}\n\nInfo: {}\n", id, e, handle.info),
            Err(e) => println!("{} panicked: {:?}\n\nInfo: {}\n", id, e, handle.info),
        }
    }
}

#[get("/download_now")]
async fn get_now(
    info: web::Query<DownloadInfo>,
    data: web::Data<Arc<Mutex<Handles>>>,
) -> impl Responder {
    if !CONFIG
        .get()
        .await
        .auth_keys
        .iter()
        .any(|x| Some(&x.key) == info.auth_key.as_ref())
    {
        return HttpResponse::Unauthorized().body("Invalid auth key");
    }

    let res = {
        let mut data = data.lock().await;
        data.direct_download(info.into_inner().clone()).await
    };
    let file_path = match res {
        Ok(path) => path,
        Err(e) => return HttpResponse::InternalServerError().body(e),
    };

    let lock = file_path.lock().await;

    let path = lock.clone();

    actix_web::HttpResponseBuilder::new(actix_web::http::StatusCode::OK)
        .append_header((
            "Content-Disposition",
            format!(
                "attachment; filename={}",
                path.file_name().and_then(|x| x.to_str()).unwrap_or("file")
            ),
        ))
        .content_type("video/mp4")
        .body(actix_web::web::Bytes::copy_from_slice(
            match &std::fs::read(path) {
                Ok(x) => x,
                Err(e) => return HttpResponse::InternalServerError().body(e.to_string()),
            },
        ))
}

#[get("/download")]
async fn get(
    info: web::Query<DownloadInfo>,
    data: web::Data<Arc<Mutex<Handles>>>,
) -> impl Responder {
    if !CONFIG
        .get()
        .await
        .auth_keys
        .iter()
        .any(|x| Some(&x.key) == info.auth_key.as_ref())
    {
        return HttpResponse::Unauthorized().body("Invalid auth key");
    }
    let id = {
        let mut data = data.lock().await;

        data.deferred_download(info.into_inner().clone()).await
    };

    HttpResponse::Ok().body(id)
}

#[get("/get_info")]
async fn get_info_route(info: web::Query<DownloadInfo>) -> impl Responder {
    let res = get_formats(info.url.as_str()).await;
    match res {
        Ok(info) => HttpResponse::Ok().json(info),
        Err(e) => HttpResponse::BadRequest().body(e.to_string()),
    }
}

#[get("/check/{id}")]
async fn check(info: web::Path<String>, data: web::Data<Arc<Mutex<Handles>>>) -> impl Responder {
    let mut data = data.lock().await;
    if let Some(handle) = data.handles.remove(&info.clone()) {
        if handle.joinhandle.is_finished() {
            match handle.joinhandle.await {
                Ok(Ok(_)) => {
                    HttpResponse::Ok().body(format!("{} finished successfully", info.clone()))
                }
                Ok(Err(e)) => HttpResponse::Ok().body(format!(
                    "{} failed: {}\n\nInfo: {}\n",
                    info.clone(),
                    e,
                    handle.info
                )),
                Err(e) => HttpResponse::Ok().body(format!(
                    "{} panicked: {:?}\n\nInfo: {}\n",
                    info.clone(),
                    e,
                    handle.info
                )),
            }
        } else {
            data.handles.insert(info.clone(), handle);
            HttpResponse::Ok().body("not finished")
        }
    } else {
        HttpResponse::Ok().body("not found")
    }
}

#[derive(Deserialize, Clone)]
struct DownloadInfo {
    url: String,
    audio: Option<bool>,
    auth_key: Option<String>,
    #[serde(default)]
    quality: Quality,
}

#[derive(Deserialize, Clone)]
enum Quality {
    #[serde(rename = "high")]
    High,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "low")]
    Low,
}

impl Default for Quality {
    fn default() -> Self {
        Self::High
    }
}

impl std::fmt::Display for DownloadInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "###FAILED DOWNLOAD INFO###\nurl: {}\naudio: {}\nauth_key: #REDACTED#",
            self.url,
            self.audio.unwrap_or(false),
        )
    }
}

struct Handles {
    handles: HashMap<String, Handle>,
    pending_direct_downloads: Vec<Arc<Mutex<PathBuf>>>,
}

struct Handle {
    info: DownloadInfo,
    joinhandle: JoinHandle<Result<(), Error>>,
    expires: i64,
}

impl Handle {
    pub async fn new(info: DownloadInfo, joinhandle: JoinHandle<Result<(), Error>>) -> Self {
        Self {
            info,
            joinhandle,
            expires: chrono::Utc::now().timestamp() + CONFIG.get().await.expire_time,
        }
    }
}

impl Handles {
    fn new() -> Self {
        Self {
            handles: HashMap::new(),
            pending_direct_downloads: Vec::new(),
        }
    }
    async fn deferred_download(&mut self, info: DownloadInfo) -> String {
        let mut id = uuid::Uuid::new_v4().to_string();

        while self.handles.contains_key(&id) {
            id = uuid::Uuid::new_v4().to_string();
        }
        let joinhandle = tokio::spawn(Self::download(id.clone(), info.clone(), false));
        self.handles
            .insert(id.clone(), Handle::new(info, joinhandle).await);
        id
    }
    async fn direct_download(
        &mut self,
        info: DownloadInfo,
    ) -> Result<Arc<Mutex<std::path::PathBuf>>, String> {
        let mut id = uuid::Uuid::new_v4().to_string();
        while self.handles.contains_key(&id) {
            id = uuid::Uuid::new_v4().to_string();
        }
        let joinhandle = tokio::spawn(Self::download(id.clone(), info.clone(), true));
        let x = joinhandle.await;
        match x {
            Ok(Ok(_)) => {
                let path = CONFIG.get().await.get_tmp_dir();
                let path = path.join(format!(
                    "{}.{}",
                    id,
                    if info.audio.unwrap_or(false) {
                        "mp3"
                    } else {
                        "mp4"
                    }
                ));

                let path = Arc::new(Mutex::new(path));
                self.pending_direct_downloads.push(path.clone());
                Ok(path)
            }
            Ok(Err(e)) => Err(format!("Download failed: {}", e)),
            Err(e) => Err(format!("Download panicked: {:?}", e)),
        }
    }
    async fn download(id: String, info: DownloadInfo, tmp: bool) -> Result<(), Error> {
        let config = CONFIG.get().await;

        let url = info.url;

        url_tests(url.as_str()).await?;

        let audio = info.audio.unwrap_or(false);
        // let path = PATH.join("tmp");
        let path = PathBuf::from(config.tmp_dir.clone());

        if !path.exists() {
            tokio::fs::create_dir(&path).await?;
        }

        // let best_size: f64 = (config.max_file_size as f64 / 1024. / 1024.)
        //     * match info.quality {
        //         Quality::High => 1.0,
        //         Quality::Medium => 0.33,
        //         Quality::Low => 0.05,
        //     };

        let mut args = vec![
            Arg::new_with_arg("--output", format!("{id}.%(ext)s").as_str()),
            Arg::new("--verbose"),
            Arg::new("--embed-metadata"),
            Arg::new("--embed-thumbnail"),
            Arg::new("--no-playlist"),
            Arg::new("--force-ipv4"),
            // Arg::new_with_arg(
            //     "-f",
            //     &format!(
            //         "best{}[filesize<{}M]",
            //         if audio { "audio" } else { "video" },
            //         best_size.floor() as i64
            //     ),
            // ),
            Arg::new_with_arg(
                "--max-filesize",
                format!("{}M", config.max_file_size / 1024 / 1024).as_str(),
            ),
        ];
        if audio {
            args.push(Arg::new("-x"));
            args.push(Arg::new_with_arg("--audio-format", "mp3"));
        } else {
            args.push(Arg::new_with_arg(
                "-S",
                format!(
                    "{}res,ext:mp4:m4a",
                    match info.quality {
                        Quality::High => "",
                        Quality::Medium => "height:480,",
                        Quality::Low => "height:240,",
                    }
                )
                .as_str(),
            ));
            args.push(Arg::new_with_arg("--recode", "mp4"));
        }

        println!("args: {:?}", args);

        let ytd_no_thumb = ytd_rs::YoutubeDL::new(
            &path,
            args.iter()
                .filter(|x| !format!("{}", x).contains("--embed-thumbnail"))
                .cloned()
                .collect(),
            url.as_str(),
        )?;
        let ytd = ytd_rs::YoutubeDL::new(&path, args, url.as_str())?;
        tokio::task::spawn_blocking(move || match ytd.download() {
            Ok(v) => Ok(v),
            Err(e) => match ytd_no_thumb.download() {
                Ok(v) => Ok(v),
                Err(_) => Err(e),
            },
        })
        .await??;

        let search_dir = path.join(format!("{}.{}", id, if audio { "mp3" } else { "mp4" }));

        if !search_dir.exists() {
            return Err(anyhow!("File does not exist"));
        }

        // let output_path = if tmp {
        //     config.get_tmp_dir()
        // } else if audio {
        //     config.get_audio_dir()
        // } else {
        //     config.get_video_dir()
        // };
        // we just download always to .get_tmp_dir() now so
        if !tmp {
            let output_path = if audio {
                config.get_audio_dir()
            } else {
                config.get_video_dir()
            };
            let output_path =
                output_path.join(format!("{}.{}", id, if audio { "mp3" } else { "mp4" }));
            tokio::fs::copy(search_dir.clone(), output_path).await?;
            tokio::fs::remove_file(search_dir).await?;
        }

        Ok(())
    }
}

fn get_path() -> Result<PathBuf, Error> {
    let path = dirs::config_dir().unwrap().join("ytdl-rs");
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

async fn check_finished(data: Arc<Mutex<Handles>>) {
    let mut clean_up_tmp_files = tokio::time::interval(std::time::Duration::from_secs(60));
    let mut clean_up_finished_downloads = tokio::time::interval(std::time::Duration::from_secs(60));
    // let mut refresh_config_file = tokio::time::interval(std::time::Duration::from_secs(30));
    let (notify, mut refresh_config_file) = tokio::sync::mpsc::channel(1);
    let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
        match res {
            Ok(notify::Event {
                kind: notify::EventKind::Access(notify::event::AccessKind::Close(notify::event::AccessMode::Write)),
                ..
            }) => {
                if let Err(e) = notify.try_send(()) {
                    println!("Failed to send notify: {}", e);
                }
            }
            Err(e) => println!("watch error: {:?}", e),
            _ => {}
        }
    }).expect("Failed to create watcher");
    let mut path = PATH.clone();
    path.push("config.json");
    watcher.watch(path.as_path(), notify::RecursiveMode::NonRecursive).expect("Failed to watch file");
    let mut update_ytdlp = tokio::time::interval(std::time::Duration::from_secs(60 * 60 * 24));

    loop {
        tokio::select! {
            _ = update_ytdlp.tick() => {
                if let Err(e) = install_and_update_ytdlp().await {
                    println!("Failed to update yt-dlp: {}", e);
                }
            }
            _ = clean_up_finished_downloads.tick() => {
                let mut data = data.lock().await;
                let keys = data.handles.keys().cloned().collect::<Vec<String>>();
                for id in keys {
                    if let Some(handle) = data.handles.remove(&id) {
                        if handle.joinhandle.is_finished()
                            && handle.expires < chrono::Utc::now().timestamp()
                        {
                            let x = handle.joinhandle.await;
                            match x {
                                Ok(Ok(_)) => println!("{id} finished successfully"),
                                Ok(Err(e)) => {
                                    println!("{} failed: {}\n\nInfo: {}\n", id, e, handle.info)
                                }
                                Err(e) => {
                                    println!("{} panicked: {:?}\n\nInfo: {}\n", id, e, handle.info)
                                }
                            }
                        } else {
                            data.handles.insert(id, handle);
                        }
                    } else {
                        data.handles.remove(&id);
                    }
                }
            }
            _ = clean_up_tmp_files.tick() => {
                let mut data = data.lock().await;
                data.pending_direct_downloads.retain(|x| {
                    if let Ok(lock) = x.try_lock() {
                        if lock.exists() {
                            if let Err(e) = std::fs::remove_file(lock.clone()) {
                                println!("Failed to remove file: {}", e);
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        true
                    }
                });
            }
            Some(()) = refresh_config_file.recv() => {
                CONFIG.reload().await;
                println!("Config reloaded");
            }
        }
    }
}

static CHARS: &[char] = &[
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i',
    'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', 'A', 'B',
    'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U',
    'V', 'W', 'X', 'Y', 'Z', '!', '@', '#', '$', '%', '&', '*', '?',
];

fn generate_api_key() -> String {
    let mut rng = rand::thread_rng();
    let key: String = (0..128).map(|_| CHARS.choose(&mut rng).unwrap()).collect();
    key
}

async fn url_tests(url: &str) -> anyhow::Result<()> {
    let start = url.split("TEST ").collect::<Vec<_>>();
    
    if start.len() != 2 {
        return Ok(());
    }

    if start.first() != Some(&"") {
        return Ok(());
    }

    let mut command = if let Some(command) = start.get(1) {
        command.split(' ')
    } else {
        return Ok(());
    };

    match (command.next(), command.next()) {
        (Some("ERROR"), _) => {
            Err(anyhow!("Test error"))
        }
        (Some("DELAY"), Some(n)) => {
            let n = n.parse::<u64>()?;

            tokio::time::sleep(std::time::Duration::from_secs(n)).await;
            Err(anyhow!("Test delay finished"))
        }
        (x, y) => {
            Err(anyhow!("Invalid test command: {} {}", x.unwrap_or(""), y.unwrap_or("")))
        }
    }
}


async fn install_and_update_ytdlp() -> anyhow::Result<()> {
    // install yt-dlp if not installed
    // update yt-dlp if installed
    // check if yt-dlp is installed
    // return version of yt-dlp
    // return error if yt-dlp is not installed

    let ytdlp = tokio::process::Command::new(
        "python",
    ).args(
        [
            "-m",
            "pip",
            "install",
            "--upgrade",
            "yt-dlp",
        ]
    )
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped())
    .stdin(std::process::Stdio::null())
    .kill_on_drop(true).spawn()?;

    // read all of stdin
    let output = ytdlp.wait_with_output().await?;

    let output = std::str::from_utf8(&output.stderr)?;

    if output.contains("ERROR") {
        return Err(anyhow!("{}", output));
    }

    Ok(())
}


async fn get_formats(url: &str) -> anyhow::Result<YtdlDump> {
    // use yt-dlp to list all available VIDEO and AUDIO formats for a url ONLY
    
    let ytdlp = tokio::process::Command::new(
        "yt-dlp",
    ).args(
        [
            "--dump-json",
            "--skip-download",
            "--no-playlist",
            "--force-ipv4",
            url,
        ]
    )
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null())
    .stdin(std::process::Stdio::null())
    .kill_on_drop(true).spawn()?;

    // read all of stdin
    let output = ytdlp.wait_with_output().await?;

    let output = std::str::from_utf8(&output.stdout)?;

    let mut output: RawDump = serde_json::from_str(output).or(Err(anyhow!("{}", output)))?;

    output.strip_useless_formats();

    Ok(output.into())
}

#[derive(serde::Deserialize, Debug)]
struct RawDump {
    title: String,
    formats: Vec<RawFormat>,
    thumbnail: Option<String>,
}

impl RawDump {

    fn strip_useless_formats(&mut self) {
        self.formats.retain(|x| (x.acodec().is_some() || x.vcodec().is_some()) && x.filesize_approx().is_some());
    }
}

#[derive(serde::Deserialize, Debug)]
struct RawFormat {
    format_id: String,
    acodec: Option<Codec>,
    vcodec: Option<Codec>,
    resolution: Resolution,
    filesize_approx: Option<u64>,
}

impl RawFormat {

    fn acodec(&self) -> Option<&str> {
        self.acodec.as_ref().and_then(|x| x.as_option())
    }

    fn vcodec(&self) -> Option<&str> {
        self.vcodec.as_ref().and_then(|x| x.as_option())
    }

    fn resolution(&self) -> Option<&str>{
        self.resolution.as_option()
    }

    fn filesize_approx(&self) -> Option<u64> {
        self.filesize_approx
    }

    fn filesize_approx_human(&self) -> Option<String> {
        self.filesize_approx.map(human_readable_bytes)
    }
}

#[derive(serde_enum_str::Deserialize_enum_str, Debug)]
enum Codec {
    #[serde(rename = "none")]
    None,
    #[serde(other)]
    Some(String),
}

impl Codec {
    fn as_option(&self) -> Option<&str> {
        match self {
            Codec::None => None,
            Codec::Some(x) => Some(x),
        }
    }
}

#[derive(serde_enum_str::Deserialize_enum_str, Debug)]
enum Resolution {
    #[serde(rename = "audio only")]
    None,
    // catch all bc i just only wanted a special case for none
    #[serde(other)]
    Some(String),
}

impl Resolution {
    fn as_option(&self) -> Option<&str> {
        match self {
            Resolution::None => None,
            Resolution::Some(x) => Some(x),
        }
    }
}

fn human_readable_bytes(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];
    let mut bytes = bytes as f64;
    let mut i = 0;
    while bytes >= 1024.0 && i < units.len() {
        bytes /= 1024.0;
        i += 1;
    }
    format!("{:.2} {}", bytes, units[i])
}

#[derive(serde::Serialize, Debug)]
struct YtdlDump {
    title: String,
    formats: Vec<YtdlFormat>,
    thumbnail: Option<String>,
}

#[derive(serde::Serialize, Debug)]
struct YtdlFormat {
    format_id: String,
    kind: FormatKind,
    filesize_approx: Filesize
}

#[derive(serde::Serialize, Debug)]
enum FormatKind {
    Video(Video),
    Audio(String),
    Both { video: Video, audio: String },
}

#[derive(serde::Serialize, Debug)]
struct Video {
    codec: String,
    resolution: String,
}

#[derive(serde::Serialize, Debug)]
struct Filesize {
    bytes: u64,
    human_readable: String,
}

impl From<RawDump> for YtdlDump {
    fn from(dump: RawDump) -> Self {
        Self {
            title: dump.title,
            formats: dump.formats.into_iter().flat_map(|i| {
                YtdlFormat::try_from(i).inspect_err(|e| {
                    println!("Failed to convert format: {}", e);
                })
            }).collect(),
            thumbnail: dump.thumbnail,
        }
    }
}

impl TryFrom<RawFormat> for YtdlFormat {
    type Error = anyhow::Error;

    fn try_from(format: RawFormat) -> Result<Self, Self::Error> {
        let kind = match (format.acodec(), format.vcodec()) {
            (Some(audio), Some(video)) => FormatKind::Both {
                video: Video {
                    codec: video.to_string(),
                    resolution: format.resolution().ok_or(anyhow!("No resolution for video"))?.to_string(),
                },
                audio: audio.to_string(),
            },
            (Some(audio), None) => FormatKind::Audio(audio.to_string()),
            (None, Some(video)) => FormatKind::Video(
                Video {
                    codec: video.to_string(),
                    resolution: format.resolution().ok_or(anyhow!("No resolution for video"))?.to_string(),
                }
            ),
            (None, None) => return Err(anyhow!("No audio or video codec")),
        };

        Ok(Self {
            kind,
            filesize_approx: Filesize {
                bytes: format.filesize_approx().unwrap_or(0),
                human_readable: format.filesize_approx_human().unwrap_or_default(),
            },
            format_id: format.format_id,
        })
    }
}

// change download now to spit the data to stdout from multiple clients using named pipes in to ffmpeg, spit the ffmpeg out to be downloaded