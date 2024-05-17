mod config;
use actix_web::{get, web, HttpResponse, Responder};
use anyhow::{anyhow, Error};
use config::safe_get;
use lazy_static::lazy_static;
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

// struct DropListener<T: std::fmt::Debug> {
//     inner: T,
// }

// impl<T: std::fmt::Debug> Drop for DropListener<T> {
//     fn drop(&mut self) {
//         println!("Dropping: {:?}", self.inner);
//     }
// }

#[tokio::main]
async fn main() {
    // check for any flags
    let args: Vec<String> = std::env::args().collect();
    // we only care about the first one, heres the mapping:
    // -a: add user, will prompt for a username and api key then exit
    // -r: remove user, will list all users and prompt for a username to remove
    // -l: list users, will list all users names

    match (args.get(1).map(|s| s.as_str()), args.get(2)) {
        (_, Some(_)) => {
            println!("Invalid arguments");
            return;
        }
        (Some("-a"), _) => {
            // add user
            let user: String = safe_get("Username");
            let key: String = safe_get("API Key");
            let mut config = CONFIG.get().await;
            config.auth_keys.push(config::AuthInfo { user, key });
            let file = std::fs::File::create(PATH.join("config.json")).unwrap();
            serde_json::to_writer_pretty(file, &config).unwrap();
            return;
        }
        (Some("-r"), _) => {
            // remove user
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
            // list users
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

    // start an actix web server with the config values and the routes defined below
    let joinhandles: Arc<Mutex<Handles>> = Arc::new(Mutex::new(Handles::new()));
    let joinhandles2 = joinhandles.clone();
    let joinhandles3 = joinhandles.clone();
    tokio::spawn(check_finished(joinhandles3));
    actix_web::HttpServer::new(move || {
        // uwu
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
    // now we await all remaining joinhandles
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
    // the same as get, but will block until the download is finished and then respond with the file itself (downloading it for the user)
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

// get with qeury parameters
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
        // println!("Got request: {:?}", info);
        let mut data = data.lock().await;
        // println!("Got lock");
        data.deferred_download(info.into_inner().clone()).await
    };
    // println!("Added to handles");
    HttpResponse::Ok().body(id)
}

#[get("/check/{id}")]
async fn check(info: web::Path<String>, data: web::Data<Arc<Mutex<Handles>>>) -> impl Responder {
    let mut data = data.lock().await;
    if let Some(handle) = data.handles.remove(&info.clone()) {
        if handle.joinhandle.is_finished() {
            // now we take the joinhandle out of the hashmap, await the result, and return it
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
            // if the joinhandle is not finished, we put it back in the hashmap and return not finished
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
    pending_direct_downloads: Vec<Arc<Mutex<PathBuf>>>, // this will store the paths of the files that are being downloaded directly, the mutex lock will be dropped once the download is finished so the file can be cleaned up
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
        // will generate a unique id for the download, then spawn a thread to download the video
        // and add the joinhandle to the hashmap
        let mut id = uuid::Uuid::new_v4().to_string();
        // ensure the id is unique
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
        // return Err(anyhow!("Download is disabled"));
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
            // Arg::new("--quiet"),
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

        // let ytd_info = ytd_rs::YoutubeDL::new(
        //     &path,
        //     args.iter()
        //         .chain([Arg::new("--skip-download"), Arg::new("--dump-json")].iter())
        //         .cloned()
        //         .collect::<Vec<Arg>>(),
        //     url.as_str(),
        // )?;

        // let info = tokio::task::spawn_blocking(move || ytd_info.download()).await??;
        // let sizes = serde_json::from_str::<YtdlInfo>(info.output())?.get_filesize();
        // // if there are no filesizes
        // if sizes.is_empty() {
        //     return Err(anyhow!("Cannot determine size, download failed"));
        // }
        // // if the file is too large
        // if sizes.iter().any(|x| x > &config.max_file_size) {
        //     return Err(anyhow!("File too large"));
        // }

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
        // ensure the file exists
        if !search_dir.exists() {
            return Err(anyhow!("File does not exist"));
        }

        // now we will move the file to the appropriate directory
        let output_path = if tmp {
            config.get_tmp_dir()
        } else if audio {
            config.get_audio_dir()
        } else {
            config.get_video_dir()
        };
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
                    // attempt to lock, if the lock fails, keep it
                    if let Ok(lock) = x.try_lock() {
                        // if the lock succeeds, check if the file exists, if it does, remove it
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

// {
//   "id": "radio",
//   "title": "radio",
//   "timestamp": null,
//   "direct": true,
//   "formats": [
//     {
//       "format_id": "mpeg",
//       "url": "http://127.0.0.1:60997/listen/dnr/radio.mp3",
//       "ext": "mp3",
//       "vcodec": "none",
//       "acodec": "mp3",
//       "protocol": "http",
//       "resolution": "audio only",
//       "aspect_ratio": null,
//       "filesize_approx": null,
//       "http_headers": {
//         "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/90.0.4430.85 Safari/537.36",
//         "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
//         "Accept-Language": "en-us,en;q=0.5",
//         "Sec-Fetch-Mode": "navigate"
//       },
//       "audio_ext": "mp3",
//       "video_ext": "none",
//       "vbr": 0,
//       "abr": null,
//       "tbr": null,
//       "format": "mpeg - audio only"
//     }
//   ],
//   "subtitles": {},
//   "http_headers": {
//     "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/90.0.4430.85 Safari/537.36",
//     "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
//     "Accept-Language": "en-us,en;q=0.5",
//     "Sec-Fetch-Mode": "navigate"
//   },
//   "hls_aes": null,
//   "webpage_url": "http://127.0.0.1:60997/listen/dnr/radio.mp3",
//   "original_url": "http://127.0.0.1:60997/listen/dnr/radio.mp3",
//   "webpage_url_basename": "radio.mp3",
//   "webpage_url_domain": "127.0.0.1:60997",
//   "extractor": "generic",
//   "extractor_key": "Generic",
//   "playlist": null,
//   "playlist_index": null,
//   "display_id": "radio",
//   "fulltitle": "radio",
//   "release_year": null,
//   "requested_subtitles": null,
//   "_has_drm": null,
//   "epoch": 1713685278,
//   "format_id": "mpeg",
//   "url": "http://127.0.0.1:60997/listen/dnr/radio.mp3",
//   "ext": "mp3",
//   "vcodec": "none",
//   "acodec": "mp3",
//   "protocol": "http",
//   "resolution": "audio only",
//   "aspect_ratio": null,
//   "filesize_approx": null,
//   "audio_ext": "mp3",
//   "video_ext": "none",
//   "vbr": 0,
//   "abr": null,
//   "tbr": null,
//   "format": "mpeg - audio only",
//   "_filename": "radio [radio].mp3",
//   "filename": "radio [radio].mp3",
//   "_type": "video",
//   "_version": {
//     "version": "2024.04.18.232703",
//     "current_git_head": null,
//     "release_git_head": "c9ce57d9bf51541da2381d99bc096a9d0ddf1f27",
//     "repository": "yt-dlp/yt-dlp-nightly-builds"
//   }
// }

// we just want to extract the approximate file size so we can exit if it's too large
// #[derive(Deserialize)]
// struct YtdlInfo {
//     formats: Vec<YtdlFormat>,
// }

// #[derive(Deserialize)]
// struct YtdlFormat {
//     filesize_approx: Option<u64>,
// }

// impl YtdlInfo {
//     fn get_filesize(&self) -> Vec<u64> {
//         self.formats
//             .iter()
//             .flat_map(|x| x.filesize_approx)
//             .collect::<Vec<u64>>()
//     }
// }
