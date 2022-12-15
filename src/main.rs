mod config;
use actix_web::{get, web, HttpResponse, Responder};
use anyhow::{anyhow, Error};
use lazy_static::lazy_static;
use serde::Deserialize;
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::{sync::Mutex, task::JoinHandle};
use ytd_rs::Arg;

lazy_static! {
    static ref PATH: PathBuf = get_path().unwrap();
}

lazy_static! {
    static ref CONFIG: config::Config = config::Config::new();
}

#[tokio::main]
async fn main() {
    // start an actix web server with the config values and the routes defined below
    let joinhandles: Arc<Mutex<Handles>> = Arc::new(Mutex::new(Handles::new()));
    let joinhandles2 = joinhandles.clone();
    let joinhandles3 = joinhandles.clone();
    tokio::spawn(check_finished(joinhandles3));
    actix_web::HttpServer::new(move || {
        // uwu
        actix_web::App::new().app_data(web::Data::new(joinhandles2.clone())).service(get).service(check)
    })
    .bind(format!("{}:{}", CONFIG.host, CONFIG.port))
    .unwrap()
    .run()
    .await
    .unwrap();
    // now we await all remaining joinhandles
    let mut joinhandles = joinhandles.lock().await;
    for (id, handle) in joinhandles.handles.drain() {
        let x = handle.joinhandle.await;
        match x {
            Ok(Ok(_)) => println!("{} finished successfully", id),
            Ok(Err(e)) => println!("{} failed: {}\n\nInfo: {:?}\n", id, e, handle.info),
            Err(e) => println!("{} panicked: {:?}\n\nInfo: {:?}\n", id, e, handle.info),
        }
    }
}

// get with qeury parameters
#[get("/download")]
async fn get(info: web::Query<DownloadInfo>, data: web::Data<Arc<Mutex<Handles>>>) -> impl Responder {
    if !CONFIG.auth_key.is_empty() && info.auth_key.clone().unwrap_or_default() != CONFIG.auth_key {
        return HttpResponse::Unauthorized().body("Invalid auth key");
    }
    let id = {
        let mut data = data.lock().await;
        data.add(info.into_inner().clone()).await
    };
    HttpResponse::Ok().body(id)
}

#[get("/check/{id}")]
async fn check(info: web::Path<String>, data: web::Data<Arc<Mutex<Handles>>>) -> impl Responder {
    let mut data = data.lock().await;
    if let Some(handle) = data.handles.remove(&info.clone()) {
        if handle.joinhandle.is_finished() {
            // now we take the joinhandle out of the hashmap, await the result, and return it
            match handle.joinhandle.await {
                Ok(Ok(_)) => HttpResponse::Ok().body(format!("{} finished successfully", info.clone())),
                Ok(Err(e)) => HttpResponse::Ok().body(format!("{} failed: {}\n\nInfo: {:?}\n", info.clone(), e, handle.info)),
                Err(e) => HttpResponse::Ok().body(format!("{} panicked: {:?}\n\nInfo: {:?}\n", info.clone(), e, handle.info)),
            }
        } else {
            // if the joinhandle is not finished, we put it back in the hashmap and return not finished
            data.handles.insert(info.clone(), handle);
            HttpResponse::Ok().body("not finished")
        }
    } else {
        HttpResponse::Ok().body("not found")
    }
}

#[derive(Debug, Deserialize, Clone)]
struct DownloadInfo {
    url: String,
    audio: Option<bool>,
    auth_key: Option<String>,
}

struct Handles {
    handles: HashMap<String, Handle>,
}

struct Handle {
    info: DownloadInfo,
    joinhandle: JoinHandle<Result<(), Error>>,
    expires: i64,
}

impl Handle {
    pub fn new(info: DownloadInfo, joinhandle: JoinHandle<Result<(), Error>>) -> Self {
        Self {
            info,
            joinhandle,
            expires: chrono::Utc::now().timestamp() + CONFIG.expire_time,
        }
    }
}

impl Handles {
    fn new() -> Self {
        Self { handles: HashMap::new() }
    }
    async fn add(&mut self, info: DownloadInfo) -> String {
        // will generate a unique id for the download, then spawn a thread to download the video
        // and add the joinhandle to the hashmap
        let mut id = uuid::Uuid::new_v4().to_string();
        // ensure the id is unique
        while self.handles.contains_key(&id) {
            id = uuid::Uuid::new_v4().to_string();
        }
        let joinhandle = tokio::spawn(Self::download(id.clone(), info.clone()));
        self.handles.insert(id.clone(), Handle::new(info, joinhandle));
        id
    }
    async fn download(id: String, info: DownloadInfo) -> Result<(), Error> {
        // this will download the video and return either an error or ()
        let url = info.url;
        let audio = info.audio.unwrap_or(false);
        let path = PATH.join("tmp");
        // create the tmp directory if it doesn't exist
        if !path.exists() {
            tokio::fs::create_dir(&path).await?;
        }
        // now we will download the video using the ytdlp crate
        let mut args = vec![
            Arg::new("--quiet"),
            Arg::new_with_arg("--output", format!("{}.%(ext)s", id).as_str()),
            Arg::new("--embed-metadata"),
            Arg::new("--no-playlist"),
        ];
        if audio {
            args.push(Arg::new("-x"));
            args.push(Arg::new_with_arg("--audio-format", "mp3"));
        } else {
            args.push(Arg::new_with_arg("-S", "res,ext:mp4:m4a"));
            args.push(Arg::new_with_arg("--recode", "mp4"));
        }
        let ytd = ytd_rs::YoutubeDL::new(&path, args, url.as_str())?;
        let _response = tokio::task::spawn_blocking(move || ytd.download()).await??;

        let search_dir = path.join(format!("{}.{}", id, if audio { "mp3" } else { "mp4" }));
        // ensure the file exists
        if !search_dir.exists() {
            return Err(anyhow!("File does not exist"));
        }

        // now we will move the file to the appropriate directory
        let output_path = if audio { CONFIG.get_audio_dir() } else { CONFIG.get_video_dir() };
        let output_path = output_path.join(format!("{}.{}", id, if audio { "mp3" } else { "mp4" }));
        tokio::fs::copy(search_dir.clone(), output_path).await?;
        // now we will delete the tmp file
        tokio::fs::remove_file(search_dir).await?;

        Ok(())
    }
}

fn get_path() -> Result<PathBuf, Error> {
    // this will get the path to the executable
    let path = dirs::config_dir().unwrap().join("ytdl-rs");
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

async fn check_finished(data: Arc<Mutex<Handles>>) {
    // check periodically to see if any downloads have finished
    loop {
        let mut data = data.lock().await;
        let keys = data.handles.keys().cloned().collect::<Vec<String>>();
        for id in keys {
            if let Some(handle) = data.handles.remove(&id) {
                if handle.joinhandle.is_finished() && handle.expires < chrono::Utc::now().timestamp() {
                    let x = handle.joinhandle.await;
                    match x {
                        Ok(Ok(_)) => println!("{} finished successfully", id),
                        Ok(Err(e)) => println!("{} failed: {}\n\nInfo: {:?}\n", id, e, handle.info),
                        Err(e) => println!("{} panicked: {:?}\n\nInfo: {:?}\n", id, e, handle.info),
                    }
                } else {
                    data.handles.insert(id, handle);
                }
            } else {
                data.handles.remove(&id);
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

// pub async fn get_video(url: String, audio_only: bool, allow_playlist: bool) -> Result<Vec<VideoType>, anyhow::Error> {
//     #[cfg(not(feature = "download"))]
//     if allow_playlist {
//         let vid = get_video_info(url.clone()).await;
//         if let Ok(vid) = vid {
//             return Ok(vec![VideoType::Url(vid)]);
//         } else {
//             return Err(anyhow::anyhow!("Could not get video info"));
//         }
//     }
//     let id = nanoid::nanoid!(10);
//     let mut path = crate::Config::get().data_path.clone();
//     path.push("tmp");
//     std::fs::create_dir_all(&path)?;
//     let mut args = vec![
//         Arg::new("--quiet"),
//         Arg::new_with_arg("--output", format!("{}_%(playlist_index)s.%(ext)s", id).as_str()),
//         Arg::new("--embed-metadata"),
//     ];
//     if audio_only {
//         args.push(Arg::new("-x"));
//         args.push(Arg::new_with_arg("--audio-format", "mp3"));
//     } else {
//         args.push(Arg::new_with_arg("-S", "res,ext:mp4:m4a"));
//         args.push(Arg::new_with_arg("--recode", "mp4"));
//     }
//     if !allow_playlist {
//         args.push(Arg::new("--no-playlist"));
//     }
//     let ytd = ytd_rs::YoutubeDL::new(&path, args, url.as_str())?;
//     let response = tokio::task::spawn_blocking(move || ytd.download()).await??;

//     let file = response.output_dir();

//     let mut videos = Vec::new();
//     for entry in std::fs::read_dir(file)? {
//         let entry = entry?;
//         let path = entry.path();
//         if path.is_file() {
//             let file_name = path.file_name().unwrap().to_str().unwrap();
//             if file_name.starts_with(id.as_str()) {
//                 videos.push(Self::from_path(path, url.clone(), audio_only, id.clone())?);
//             }
//         }
//     }
//     if videos.is_empty() {
//         Err(anyhow::anyhow!("No videos found"))
//     } else {
//         videos.sort_by(|a, b| a.playlist_index.cmp(&b.playlist_index));
//         Ok(videos.iter().map(|v| VideoType::Disk(v.clone())).collect())
//     }
// }
