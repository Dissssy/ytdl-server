mod config;
use actix_web::{get, web, HttpResponse, Responder};
use anyhow::{anyhow, Error};
use config::safe_get;
use lazy_static::lazy_static;
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
    })
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
        let audio = info.audio.unwrap_or(false);
        let path = PATH.join("tmp");

        if !path.exists() {
            tokio::fs::create_dir(&path).await?;
        }

        let mut args = vec![
            Arg::new_with_arg("--output", format!("{id}.%(ext)s").as_str()),
            Arg::new("--verbose"),
            Arg::new("--embed-metadata"),
            Arg::new("--embed-thumbnail"),
            Arg::new("--no-playlist"),
            Arg::new_with_arg(
                "-f",
                &format!(
                    "best[filesize<{}M]/bv*+ba/b",
                    config.max_file_size / 1024 / 1024
                ),
            ),
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

        let output_path = if tmp {
            config.get_tmp_dir()
        } else if audio {
            config.get_audio_dir()
        } else {
            config.get_video_dir()
        };
        let output_path = output_path.join(format!("{}.{}", id, if audio { "mp3" } else { "mp4" }));
        tokio::fs::copy(search_dir.clone(), output_path).await?;

        tokio::fs::remove_file(search_dir).await?;

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
    let mut refresh_config_file = tokio::time::interval(std::time::Duration::from_secs(30));

    loop {
        tokio::select! {
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
            _ = refresh_config_file.tick() => {
                CONFIG.reload().await;
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
