use std::{io::Write, path::PathBuf, str::FromStr};

use anyhow::Error;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub auth_key: String,
    pub video_dir: String,
    pub audio_dir: String,
    pub expire_time: i64,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct MaybeConfig {
    host: Option<String>,
    port: Option<u16>,
    auth_key: Option<String>,
    video_dir: Option<String>,
    audio_dir: Option<String>,
    expire_time: Option<i64>,
}

impl Config {
    pub fn new() -> Self {
        let path = crate::PATH.join("config.json");
        // ensure the folder exists
        let mut file = std::fs::File::open(&path).unwrap_or_else(|_| std::fs::File::create(&path).unwrap());
        let maybe_config: MaybeConfig = serde_json::from_reader(&mut file).unwrap_or_default();
        let config = Config {
            host: maybe_config.host.unwrap_or_else(|| safe_get("Host")),
            port: maybe_config.port.unwrap_or_else(|| safe_get("Port")),
            auth_key: maybe_config.auth_key.unwrap_or_else(|| safe_get("AuthKey")),
            expire_time: maybe_config.expire_time.unwrap_or_else(|| safe_get("Join handle expire time (seconds)")),
            video_dir: maybe_config
                .video_dir
                .unwrap_or_else(|| safe_get_dir("Video directory (absolute path) DIRECTORIES WILL BE CREATED IF THEY DO NOT EXIST")),
            audio_dir: maybe_config
                .audio_dir
                .unwrap_or_else(|| safe_get_dir("Audio directory (absolute path) DIRECTORIES WILL BE CREATED IF THEY DO NOT EXIST")),
        };
        validate_dir(&config.video_dir).unwrap();
        validate_dir(&config.audio_dir).unwrap();
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(serde_json::to_string_pretty(&config).unwrap().as_bytes()).unwrap();
        config
    }
    pub fn get_video_dir(&self) -> PathBuf {
        PathBuf::from(&self.video_dir)
    }
    pub fn get_audio_dir(&self) -> PathBuf {
        PathBuf::from(&self.audio_dir)
    }
}

fn safe_get<T: FromStr>(prompt: &str) -> T {
    // read from terminal until user enters a valid value, then return it
    let mut input = String::new();
    loop {
        println!("{}: ", prompt);
        std::io::stdin().read_line(&mut input).unwrap();
        match input.trim().parse::<T>() {
            Ok(val) => return val,
            Err(_) => {
                println!("Invalid input");
                input.clear();
            }
        }
    }
}

fn safe_get_dir(prompt: &str) -> String {
    // read from terminal until user enters a valid directory, then return it
    let mut input = String::new();
    loop {
        println!("{}: ", prompt);
        std::io::stdin().read_line(&mut input).unwrap();
        match validate_dir(input.trim()) {
            Ok(_) => return input.trim().to_string(),
            Err(e) => {
                println!("{}", e);
                input.clear();
            }
        }
    }
}

fn validate_dir(path: &str) -> Result<String, Error> {
    // ensure the directory exists and is accessible
    let path = std::path::Path::new(path);
    if !path.exists() {
        std::fs::create_dir(path)?;
    }
    if !path.is_dir() {
        return Err(anyhow::anyhow!("{} is not a directory", path.display()));
    }
    Ok(path.to_string_lossy().into_owned())
}
