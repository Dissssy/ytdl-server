//! config.json: loaded at startup, re-read when its mtime changes.
//! A broken file on reload keeps the previous config instead of taking the server down.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub auth_keys: Vec<User>,
    pub video_dir: PathBuf,
    pub audio_dir: PathBuf,
    #[serde(default = "default_tmp")]
    pub tmp_dir: PathBuf,
    /// Hard cap on any single download, in bytes.
    #[serde(default = "default_max")]
    pub max_file_size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub user: String,
    pub key: String,
    /// NAS users' downloads go to video_dir/audio_dir; everyone else's to their browser.
    #[serde(default)]
    pub nas: bool,
}

fn default_host() -> String {
    "0.0.0.0".into()
}
fn default_port() -> u16 {
    12221
}
fn default_tmp() -> PathBuf {
    "/tmp/ytdlp-server".into()
}
fn default_max() -> u64 {
    2 * 1024 * 1024 * 1024
}

pub fn path() -> PathBuf {
    std::env::var_os("YTDL_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| "/root".into())
                .join(".config/ytdl-rs/config.json")
        })
}

/// Where users' uploaded cookies.txt files live, next to config.json.
pub fn user_cookies(user: &str) -> PathBuf {
    let safe: String = user.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    path().parent().map(Path::to_path_buf).unwrap_or_default().join("cookies").join(format!("{safe}.txt"))
}

fn load(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let config: Config = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    for dir in [&config.video_dir, &config.audio_dir, &config.tmp_dir] {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    if config.auth_keys.iter().any(|u| u.key.is_empty()) {
        bail!("an auth key is empty");
    }
    Ok(config)
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[derive(Clone)]
pub struct Store {
    path: PathBuf,
    inner: Arc<Mutex<(Arc<Config>, Option<SystemTime>)>>,
}

impl Store {
    pub fn open(path: PathBuf) -> Result<Self> {
        let config = load(&path)?;
        let stamp = mtime(&path);
        Ok(Self { path, inner: Arc::new(Mutex::new((Arc::new(config), stamp))) })
    }

    pub fn get(&self) -> Arc<Config> {
        let mut inner = self.inner.lock().unwrap();
        let stamp = mtime(&self.path);
        if stamp != inner.1 {
            inner.1 = stamp;
            match load(&self.path) {
                Ok(config) => {
                    println!("config reloaded");
                    inner.0 = Arc::new(config);
                }
                Err(e) => println!("config reload failed, keeping the previous one: {e:#}"),
            }
        }
        inner.0.clone()
    }

    pub fn user(&self, key: &str) -> Option<User> {
        self.get().auth_keys.iter().find(|u| u.key == key).cloned()
    }
}

/// CLI helpers edit the raw JSON so fields this version doesn't know about survive.
pub mod cli {
    use super::*;
    use rand::{distributions::Uniform, Rng};
    use serde_json::{json, Value};

    fn read(path: &Path) -> Result<Value> {
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }
    fn write(path: &Path, value: &Value) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(value)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
    fn keys(value: &mut Value) -> Result<&mut Vec<Value>> {
        let obj = value.as_object_mut().context("config.json is not an object")?;
        obj.entry("auth_keys").or_insert(json!([]));
        obj["auth_keys"].as_array_mut().context("auth_keys is not a list")
    }

    pub fn add(path: &Path, user: &str, nas: bool, site: &str) -> Result<()> {
        let mut value = read(path)?;
        let list = keys(&mut value)?;
        if list.iter().any(|u| u["user"] == user) {
            bail!("user {user} already exists");
        }
        // URL-safe alphabet so the key survives links and headers untouched.
        let alphabet: Vec<char> = ('0'..='9').chain('a'..='z').chain('A'..='Z').collect();
        let dist = Uniform::from(0..alphabet.len());
        let key: String = rand::thread_rng().sample_iter(dist).take(48).map(|i| alphabet[i]).collect();
        list.push(json!({ "user": user, "key": key, "nas": nas }));
        write(path, &value)?;
        println!("Added {user}{}. Invite link:\n{site}/invite.html?key={key}", if nas { " (NAS)" } else { "" });
        Ok(())
    }

    pub fn remove(path: &Path, user: &str) -> Result<()> {
        let mut value = read(path)?;
        let list = keys(&mut value)?;
        let before = list.len();
        list.retain(|u| u["user"] != user);
        if list.len() == before {
            bail!("no user named {user}");
        }
        write(path, &value)?;
        println!("Removed {user}");
        Ok(())
    }

    pub fn set_nas(path: &Path, user: &str, nas: bool) -> Result<()> {
        let mut value = read(path)?;
        let entry = keys(&mut value)?
            .iter_mut()
            .find(|u| u["user"] == user)
            .with_context(|| format!("no user named {user}"))?;
        entry["nas"] = json!(nas);
        write(path, &value)?;
        println!("{user}: nas = {nas}");
        Ok(())
    }

    pub fn list(path: &Path) -> Result<()> {
        let mut value = read(path)?;
        for u in keys(&mut value)?.iter() {
            let nas = u["nas"].as_bool().unwrap_or(false);
            println!("{}{}", u["user"].as_str().unwrap_or("?"), if nas { "  (NAS)" } else { "" });
        }
        Ok(())
    }
}
