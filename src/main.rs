mod config;
mod errors;
mod jobs;
mod probe;

use actix_web::{
    delete, get,
    http::header,
    post,
    web::{self, Bytes, Data, Json, Path, Query},
    HttpRequest, HttpResponse,
};
use futures_util::{stream, Stream, StreamExt};
use jobs::{Jobs, LegacyQuality, Plan, State, Target};
use probe::Mode;
use serde::Deserialize;
use std::{
    collections::HashMap,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

const SITE: &str = "https://ytdlp.deadlyneurotox.in";

fn usage() {
    println!(
        "ytdl-server              run the server
ytdl-server -a NAME [--nas]   add a user and print their invite link
ytdl-server -r NAME       remove a user
ytdl-server --nas NAME on|off   set whether a user's downloads go to the NAS
ytdl-server -l            list users"
    );
}

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = config::path();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] => {}
        ["-a", name] => return config::cli::add(&path, name, false, SITE),
        ["-a", name, "--nas"] => return config::cli::add(&path, name, true, SITE),
        ["-r", name] => return config::cli::remove(&path, name),
        ["--nas", name, on] => return config::cli::set_nas(&path, name, matches!(*on, "on" | "true" | "yes")),
        ["-l"] => return config::cli::list(&path),
        _ => {
            usage();
            std::process::exit(2);
        }
    }

    let store = config::Store::open(path)?;
    let config = store.get();
    let jobs = Jobs::new(store);
    jobs.start();
    let web_dir = std::env::var("WEB_DIR").unwrap_or_else(|_| "web".into());
    println!("listening on {}:{}, serving the page from {web_dir}", config.host, config.port);

    let data = Data::from(jobs);
    actix_web::HttpServer::new(move || {
        actix_web::App::new()
            .app_data(data.clone())
            // cookies.txt exports from a whole browser can run to a few MB.
            .app_data(web::PayloadConfig::new(8 * 1024 * 1024))
            .app_data(web::JsonConfig::default().error_handler(|e, _| {
                let body = serde_json::json!({ "message": "Bad request.", "detail": e.to_string() });
                actix_web::error::InternalError::from_response(e, HttpResponse::BadRequest().json(body)).into()
            }))
            .service(legacy_download)
            .service(legacy_check)
            .service(legacy_download_now)
            .service(legacy_get_info)
            .service(api_me)
            .service(api_probe)
            .service(api_create)
            .service(api_list)
            .service(api_events)
            .service(api_get)
            .service(api_file)
            .service(api_delete)
            .service(api_cookies_put)
            .service(api_cookies_delete)
            .service(actix_files::Files::new("/", &web_dir).index_file("index.html"))
    })
    // Long downloads hold /download_now open; don't time out slow clients.
    .client_request_timeout(Duration::ZERO)
    .bind((config.host.as_str(), config.port))?
    .run()
    .await?;
    Ok(())
}

// ---------- auth ----------

/// The key from `Authorization: Bearer`, or `?key=` / `?auth_key=` (EventSource and links can't set headers).
fn key_of(req: &HttpRequest) -> Option<String> {
    if let Some(v) = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(k) = v.strip_prefix("Bearer ") {
            return Some(k.trim().to_string());
        }
    }
    let query = Query::<HashMap<String, String>>::from_query(req.query_string()).ok()?;
    query.get("key").or(query.get("auth_key")).cloned()
}

fn user_of(req: &HttpRequest, jobs: &Jobs) -> Result<config::User, HttpResponse> {
    key_of(req)
        .and_then(|k| jobs.config.user(&k))
        .ok_or_else(|| HttpResponse::Unauthorized().json(serde_json::json!({ "message": "That key isn't valid. Use your invite link again.", "detail": "" })))
}

fn bad(message: &str, detail: impl ToString) -> HttpResponse {
    HttpResponse::BadRequest().json(serde_json::json!({ "message": message, "detail": detail.to_string() }))
}

// ---------- legacy routes (the old page and the userscripts) ----------

#[derive(Deserialize)]
struct LegacyQuery {
    url: String,
    audio: Option<bool>,
    #[serde(default)]
    quality: Option<String>,
}

fn legacy_plan(q: &LegacyQuery) -> Plan {
    let quality = match q.quality.as_deref() {
        Some("medium") => LegacyQuality::Medium,
        Some("low") => LegacyQuality::Low,
        _ => LegacyQuality::High,
    };
    Plan::Legacy { audio: q.audio.unwrap_or(false), quality }
}

/// Queues a download into the NAS folder; returns the job id as text.
#[get("/download")]
async fn legacy_download(req: HttpRequest, q: Query<LegacyQuery>, jobs: Data<Jobs>) -> HttpResponse {
    let user = match user_of(&req, &jobs) {
        Ok(u) => u,
        Err(_) => return HttpResponse::Unauthorized().body("Invalid auth key"),
    };
    if !user.nas {
        return HttpResponse::Forbidden().body("This key can't save to the NAS");
    }
    let view = jobs.submit(&user.user, q.url.clone(), Target::Nas, legacy_plan(&q));
    HttpResponse::Ok().body(view.id)
}

#[get("/check/{id}")]
async fn legacy_check(id: Path<String>, jobs: Data<Jobs>) -> HttpResponse {
    let body = match jobs.get(&id) {
        None => "not found".to_string(),
        Some(v) => match v.state {
            State::Queued | State::Fetching | State::Processing => "not finished".to_string(),
            State::Failed | State::Cancelled => format!(
                "{} failed: {}\n\nInfo: ###FAILED DOWNLOAD INFO###\nurl: {}\naudio: {}\nauth_key: #REDACTED#\n",
                v.id,
                v.error.map(|e| e.to_string()).unwrap_or_else(|| "cancelled".into()),
                v.url,
                v.audio
            ),
            _ => format!("{} finished successfully", v.id),
        },
    };
    HttpResponse::Ok().body(body)
}

/// Queues a browser download and answers with the file once it's ready.
#[get("/download_now")]
async fn legacy_download_now(req: HttpRequest, q: Query<LegacyQuery>, jobs: Data<Jobs>) -> HttpResponse {
    let user = match user_of(&req, &jobs) {
        Ok(u) => u,
        Err(_) => return HttpResponse::Unauthorized().body("Invalid auth key"),
    };
    let mut events = jobs.events.subscribe();
    let view = jobs.submit(&user.user, q.url.clone(), Target::Browser, legacy_plan(&q));
    let mut state = view.state;
    while !state.finished() {
        match events.recv().await {
            Ok(v) if v.id == view.id => state = v.state,
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                state = jobs.get(&view.id).map(|v| v.state).unwrap_or(State::Failed);
            }
            Err(_) => break,
        }
    }
    match jobs.get(&view.id) {
        Some(v) if v.state == State::Ready => send_file(&jobs, &v.id, &user.user).await,
        Some(v) => HttpResponse::InternalServerError().body(v.error.map(|e| e.to_string()).unwrap_or_else(|| "Download failed".into())),
        None => HttpResponse::InternalServerError().body("Download failed"),
    }
}

#[get("/get_info")]
async fn legacy_get_info(req: HttpRequest, q: Query<LegacyQuery>, jobs: Data<Jobs>) -> HttpResponse {
    match user_of(&req, &jobs) {
        Ok(u) => probe_response(&jobs, &q.url, &u.user).await,
        Err(r) => r,
    }
}

// ---------- JSON API ----------

/// When the user's own cookies.txt was uploaded (unix seconds), if they have one.
fn cookies_uploaded(user: &str) -> Option<u64> {
    let modified = std::fs::metadata(config::user_cookies(user)).and_then(|m| m.modified()).ok()?;
    modified.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// The cookie lines of a cookies.txt, each keyed by (domain, path, name).
fn cookie_rows(text: &str) -> Vec<((String, String, String), String)> {
    text.lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter_map(|line| {
            let bare = line.strip_prefix("#HttpOnly_").unwrap_or(line);
            if bare.starts_with('#') {
                return None;
            }
            let f: Vec<&str> = bare.split('\t').collect();
            (f.len() == 7).then(|| ((f[0].to_ascii_lowercase(), f[2].to_string(), f[5].to_string()), line.to_string()))
        })
        .collect()
}

/// Stores the user's own cookies.txt (Netscape format), used for their downloads only.
#[post("/api/cookies")]
async fn api_cookies_put(req: HttpRequest, body: Bytes, jobs: Data<Jobs>) -> HttpResponse {
    let user = match user_of(&req, &jobs) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Ok(text) = std::str::from_utf8(&body) else {
        return bad("That isn't a cookies.txt file.", "not text");
    };
    let uploaded = cookie_rows(text);
    if uploaded.is_empty() {
        return bad(
            "That isn't a cookies.txt file.",
            "Expected Netscape-format lines with 7 tab-separated fields. Export with a cookies.txt browser extension.",
        );
    }
    let count = uploaded.len();
    // Upsert into what's already stored, keyed by domain, path and name; Remove starts fresh.
    let path = config::user_cookies(&user.user);
    let mut merged = std::fs::read_to_string(&path).map(|t| cookie_rows(&t)).unwrap_or_default();
    for (key, line) in uploaded {
        match merged.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = line,
            None => merged.push((key, line)),
        }
    }
    let total = merged.len();
    let mut text = String::from("# Netscape HTTP Cookie File\n");
    for (_, line) in &merged {
        text.push_str(line);
        text.push('\n');
    }
    let path = config::user_cookies(&user.user);
    let write = || -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &text)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, &path)
    };
    if let Err(e) = write() {
        return HttpResponse::InternalServerError().json(serde_json::json!({ "message": "Couldn't save the cookies.", "detail": e.to_string() }));
    }
    jobs.forget_probes(&user.user);
    println!("{} uploaded {count} cookies ({total} stored)", user.user);
    HttpResponse::Ok().json(serde_json::json!({ "cookies": cookies_uploaded(&user.user), "entries": count, "total": total }))
}

#[delete("/api/cookies")]
async fn api_cookies_delete(req: HttpRequest, jobs: Data<Jobs>) -> HttpResponse {
    let user = match user_of(&req, &jobs) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let _ = std::fs::remove_file(config::user_cookies(&user.user));
    jobs.forget_probes(&user.user);
    HttpResponse::NoContent().finish()
}

#[get("/api/me")]
async fn api_me(req: HttpRequest, jobs: Data<Jobs>) -> HttpResponse {
    match user_of(&req, &jobs) {
        Ok(u) => HttpResponse::Ok().json(serde_json::json!({
            "user": u.user,
            "nas": u.nas,
            "max_file_size": jobs.config.get().max_file_size,
            "cookies": cookies_uploaded(&u.user),
        })),
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct ProbeReq {
    url: String,
}

async fn probe_response(jobs: &Jobs, url: &str, user: &str) -> HttpResponse {
    let url = url.trim();
    if url.is_empty() {
        return bad("Paste a link first.", "");
    }
    match jobs.probe(url, user).await {
        Ok(info) => {
            let stats = jobs.stats.lock().unwrap().clone();
            HttpResponse::Ok().json(serde_json::json!({
                "info": &*info,
                "ladders": {
                    "compat": probe::ladder(&info, Mode::Compat),
                    "best": probe::ladder(&info, Mode::Best),
                    "audio": probe::ladder(&info, Mode::Audio),
                },
                "formats": &info.formats,
                "rates": { "speed": stats.speed_for(&info.site), "encode": stats.encode_rate },
            }))
        }
        Err(f) => HttpResponse::UnprocessableEntity().json(f),
    }
}

#[post("/api/probe")]
async fn api_probe(req: HttpRequest, body: Json<ProbeReq>, jobs: Data<Jobs>) -> HttpResponse {
    match user_of(&req, &jobs) {
        Ok(u) => probe_response(&jobs, &body.url, &u.user).await,
        Err(r) => r,
    }
}

#[derive(Deserialize)]
struct CreateReq {
    url: String,
    mode: Mode,
    /// Bytes.
    max_size: Option<u64>,
    format: Option<String>,
    /// "browser" (default) or "nas" (NAS users only).
    #[serde(default)]
    target: Option<String>,
}

#[post("/api/jobs")]
async fn api_create(req: HttpRequest, body: Json<CreateReq>, jobs: Data<Jobs>) -> HttpResponse {
    let user = match user_of(&req, &jobs) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let url = body.url.trim().to_string();
    if url.is_empty() {
        return bad("Paste a link first.", "");
    }
    let target = match body.target.as_deref() {
        Some("nas") if user.nas => Target::Nas,
        Some("nas") => return HttpResponse::Forbidden().json(serde_json::json!({ "message": "This key can't save to the NAS.", "detail": "" })),
        // "auto" means the NAS for NAS users, used by the browser extension.
        Some("auto") if user.nas => Target::Nas,
        _ => Target::Browser,
    };
    let plan = Plan::Pick { mode: body.mode, cap: body.max_size, format: body.format.clone().filter(|f| !f.is_empty()) };
    HttpResponse::Ok().json(jobs.submit(&user.user, url, target, plan))
}

#[get("/api/jobs")]
async fn api_list(req: HttpRequest, jobs: Data<Jobs>) -> HttpResponse {
    match user_of(&req, &jobs) {
        Ok(u) => HttpResponse::Ok().json(jobs.list(&u.user)),
        Err(r) => r,
    }
}

#[get("/api/jobs/{id}")]
async fn api_get(req: HttpRequest, id: Path<String>, jobs: Data<Jobs>) -> HttpResponse {
    let user = match user_of(&req, &jobs) {
        Ok(u) => u,
        Err(r) => return r,
    };
    match jobs.get(&id).filter(|v| v.owner == user.user) {
        Some(v) => HttpResponse::Ok().json(v),
        None => HttpResponse::NotFound().json(serde_json::json!({ "message": "That download is gone.", "detail": "" })),
    }
}

#[delete("/api/jobs/{id}")]
async fn api_delete(req: HttpRequest, id: Path<String>, jobs: Data<Jobs>) -> HttpResponse {
    let user = match user_of(&req, &jobs) {
        Ok(u) => u,
        Err(r) => return r,
    };
    if jobs.remove(&id, &user.user) {
        HttpResponse::NoContent().finish()
    } else {
        HttpResponse::NotFound().finish()
    }
}

/// Server-sent events: a snapshot of this user's jobs, then every change.
#[get("/api/events")]
async fn api_events(req: HttpRequest, jobs: Data<Jobs>) -> HttpResponse {
    let user = match user_of(&req, &jobs) {
        Ok(u) => u,
        Err(r) => return r,
    };
    let snapshot = serde_json::to_string(&jobs.list(&user.user)).unwrap_or_else(|_| "[]".into());
    let first = Bytes::from(format!("event: snapshot\ndata: {snapshot}\n\n"));
    let rx = jobs.events.subscribe();
    let owner = user.user.clone();
    let jobs2 = jobs.clone();
    let updates = stream::unfold((rx, owner, jobs2), |(mut rx, owner, jobs)| async move {
        loop {
            match tokio::time::timeout(Duration::from_secs(20), rx.recv()).await {
                Err(_) => return Some((Ok::<_, actix_web::Error>(Bytes::from_static(b": keepalive\n\n")), (rx, owner, jobs))),
                Ok(Ok(v)) if v.owner == owner => {
                    let json = serde_json::to_string(&v).unwrap_or_default();
                    return Some((Ok(Bytes::from(format!("event: job\ndata: {json}\n\n"))), (rx, owner, jobs)));
                }
                Ok(Ok(_)) => continue,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                    let snapshot = serde_json::to_string(&jobs.list(&owner)).unwrap_or_else(|_| "[]".into());
                    return Some((Ok(Bytes::from(format!("event: snapshot\ndata: {snapshot}\n\n"))), (rx, owner, jobs)));
                }
                Ok(Err(_)) => return None,
            }
        }
    });
    HttpResponse::Ok()
        .content_type("text/event-stream")
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .insert_header(("X-Accel-Buffering", "no"))
        .streaming(stream::once(async move { Ok::<_, actix_web::Error>(first) }).chain(updates))
}

#[get("/api/jobs/{id}/file")]
async fn api_file(req: HttpRequest, id: Path<String>, jobs: Data<Jobs>) -> HttpResponse {
    let user = match user_of(&req, &jobs) {
        Ok(u) => u,
        Err(r) => return r,
    };
    send_file(&jobs, &id, &user.user).await
}

/// Streams a finished file from disk; reaching the end marks the job saved.
async fn send_file(jobs: &Data<Jobs>, id: &str, owner: &str) -> HttpResponse {
    let Some((path, name)) = jobs.file(id, owner) else {
        return HttpResponse::NotFound().json(serde_json::json!({ "message": "That file isn't available any more.", "detail": "" }));
    };
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) => return HttpResponse::NotFound().json(serde_json::json!({ "message": "That file isn't available any more.", "detail": e.to_string() })),
    };
    let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let jobs = jobs.clone();
    let id = id.to_string();
    let body = Tracked {
        inner: tokio_util::io::ReaderStream::with_capacity(file, 64 * 1024),
        on_end: Some(Box::new(move || jobs.mark_saved(&id))),
    };
    HttpResponse::Ok()
        .content_type(mime_for(&name))
        .insert_header((header::CONTENT_DISPOSITION, content_disposition(&name)))
        .no_chunking(len)
        .streaming(body)
}

struct Tracked<S> {
    inner: S,
    on_end: Option<Box<dyn FnOnce() + Send>>,
}

impl<S: Stream + Unpin> Stream for Tracked<S> {
    type Item = S::Item;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let next = self.inner.poll_next_unpin(cx);
        if let Poll::Ready(None) = next {
            if let Some(f) = self.on_end.take() {
                f();
            }
        }
        next
    }
}

fn mime_for(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "opus" | "ogg" => "audio/ogg",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

fn content_disposition(name: &str) -> String {
    let ascii: String = name.chars().map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' { c } else { '_' }).collect();
    let encoded: String = name
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

