use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_CONFIG_FILE: &str = "~/.config/dap-sync/config.toml";
pub const DEFAULT_SYNC_COMMAND: &str = "podkit sync -c main";
pub const DEFAULT_STORAGEBOX_SOURCE: &str = "storagebox:masters";
pub const DEFAULT_STORAGEBOX_HOERSPIELE_SOURCE: &str = "storagebox:backup/hoerspiele";
pub const DEFAULT_EXCLUDES: &[&str] = &["/Music/**", ".DS_Store", ".localized"];

fn expand_home(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(s)
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawPaths {
    local_music_dir: Option<String>,
    local_hoerspiele_dir: Option<String>,
    output_dir: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawStoragebox {
    source: Option<String>,
    hoerspiele_source: Option<String>,
    exclude: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawSync {
    command: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawDevice {
    name: Option<String>,
    firmware: Option<String>,
    content_type: Option<String>,
    sync_command: Option<String>,
    mount_point: Option<String>,
    music_folder: Option<String>,
    cache_dir: Option<String>,
    preserve_fat_order: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawConfig {
    paths: RawPaths,
    storagebox: RawStoragebox,
    sync: RawSync,
    devices: Vec<RawDevice>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Firmware {
    Apple,
    Rockbox,
    Android,
    Mlove,
    Sony,
}

impl Firmware {
    pub fn from_str(s: &str) -> Firmware {
        match s.trim().to_lowercase().as_str() {
            "rockbox" => Firmware::Rockbox,
            "android" => Firmware::Android,
            "mlove" => Firmware::Mlove,
            "sony" => Firmware::Sony,
            _ => Firmware::Apple,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Firmware::Apple => "apple",
            Firmware::Rockbox => "rockbox",
            Firmware::Android => "android",
            Firmware::Mlove => "mlove",
            Firmware::Sony => "sony",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Device {
    pub name: String,
    pub firmware: Firmware,
    pub content_type: String,
    pub sync_command: String,
    pub mount_point: Option<PathBuf>,
    pub music_folder: String,
    pub preserve_fat_order: bool,
    pub mounted: bool,
    pub last_sync: Option<DateTime<Local>>,
    pub last_status: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub config_file: PathBuf,
    pub local_music_dir: PathBuf,
    pub local_hoerspiele_dir: PathBuf,
    pub output_dir: PathBuf,
    pub storagebox_source: String,
    pub storagebox_hoerspiele_source: String,
    pub storagebox_excludes: Vec<String>,
    pub devices: Vec<Device>,
}

impl Config {
    /// Refresh `Device::mounted` from the kernel mount table (no subprocess).
    /// A device counts as mounted when its volume is mounted at the configured
    /// path or at a udisks-suffixed sibling (e.g. `IPOD` → `IPOD1`).
    pub fn refresh_mounts(&mut self) {
        for d in self.devices.iter_mut() {
            d.mounted = d
                .mount_point
                .as_deref()
                .map(|p| crate::disk::mounted_path_for(p).is_some())
                .unwrap_or(false);
        }
    }

    fn apply_state(&mut self, state: &StateStore) {
        for d in self.devices.iter_mut() {
            if let Some(rec) = state.records.get(&d.name) {
                d.last_sync = rec.last_sync;
                d.last_status = rec.status.clone();
            }
        }
    }
}

pub struct StateStore {
    pub records: BTreeMap<String, StateRecord>,
}

#[derive(Debug, Clone)]
pub struct StateRecord {
    pub last_sync: Option<DateTime<Local>>,
    pub status: String,
}

fn default_config_path() -> PathBuf {
    expand_home(DEFAULT_CONFIG_FILE)
}

pub fn load(config_path: Option<String>) -> Result<Config> {
    let path = match config_path {
        Some(p) => expand_home(&p),
        None => default_config_path(),
    };

    let mut cfg = if path.exists() {
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config at {}", path.display()))?;
        let raw: RawConfig = toml::from_str(&text)
            .with_context(|| format!("failed to parse config at {}", path.display()))?;
        let local_music = raw
            .paths
            .local_music_dir
            .map(|s| expand_home(&s))
            .unwrap_or_else(|| expand_home("~/Music"));
        let local_hoerspiele = raw
            .paths
            .local_hoerspiele_dir
            .map(|s| expand_home(&s))
            .unwrap_or_else(|| expand_home("~/hoerspiele"));
        let output_dir = raw
            .paths
            .output_dir
            .map(|s| expand_home(&s))
            .unwrap_or_else(|| expand_home("~/.local/share/dap-sync"));
        let sync_cmd = raw
            .sync
            .command
            .clone()
            .unwrap_or_else(|| DEFAULT_SYNC_COMMAND.to_string());

        let mut devices = Vec::new();
        for rd in raw.devices.iter().filter(|d| d.name.is_some()) {
            let name = rd.name.clone().unwrap();
            let firmware = Firmware::from_str(rd.firmware.as_deref().unwrap_or("apple"));
            let content_type = rd
                .content_type
                .clone()
                .unwrap_or_else(|| "music".to_string())
                .trim()
                .to_lowercase();
            let sync_command = rd
                .sync_command
                .clone()
                .unwrap_or_else(|| sync_cmd.clone());
            let mount_point = rd
                .mount_point
                .clone()
                .map(|s| expand_home(&s));
            let music_folder = rd
                .music_folder
                .clone()
                .unwrap_or_else(|| {
                    match firmware {
                        Firmware::Mlove => "Hörspiele".to_string(),
                        _ => "Music".to_string(),
                    }
                });
            let preserve_fat_order = rd
                .preserve_fat_order
                .unwrap_or_else(|| firmware == Firmware::Mlove);

            devices.push(Device {
                name,
                firmware,
                content_type,
                sync_command,
                mount_point,
                music_folder,
                preserve_fat_order,
                mounted: false,
                last_sync: None,
                last_status: String::new(),
            });
        }

        Config {
            config_file: path.clone(),
            local_music_dir: local_music,
            local_hoerspiele_dir: local_hoerspiele,
            output_dir,
            storagebox_source: raw
                .storagebox
                .source
                .unwrap_or_else(|| DEFAULT_STORAGEBOX_SOURCE.to_string()),
            storagebox_hoerspiele_source: raw
                .storagebox
                .hoerspiele_source
                .unwrap_or_else(|| DEFAULT_STORAGEBOX_HOERSPIELE_SOURCE.to_string()),
            storagebox_excludes: raw
                .storagebox
                .exclude
                .map(|v| v.clone())
                .unwrap_or_else(|| DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect()),
            devices,
        }
    } else {
        Config {
            config_file: path.clone(),
            local_music_dir: expand_home("~/Music"),
            local_hoerspiele_dir: expand_home("~/hoerspiele"),
            output_dir: expand_home("~/.local/share/dap-sync"),
            storagebox_source: DEFAULT_STORAGEBOX_SOURCE.to_string(),
            storagebox_hoerspiele_source: DEFAULT_STORAGEBOX_HOERSPIELE_SOURCE.to_string(),
            storagebox_excludes: DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect(),
            devices: Vec::new(),
        }
    };

    cfg.refresh_mounts();
    let store = load_state(&cfg.output_dir);
    cfg.apply_state(&store);

    Ok(cfg)
}

pub fn state_file(output_dir: &Path) -> PathBuf {
    output_dir.join("sync-state.toml")
}

pub fn load_state(output_dir: &Path) -> StateStore {
    let file = state_file(output_dir);
    let mut records = BTreeMap::new();
    if let Ok(text) = fs::read_to_string(&file) {
        if let Ok(table) = text.parse::<toml::Table>() {
            for (name, val) in table.iter() {
                if let Some(sub) = val.as_table() {
                    let last_sync = sub
                        .get("last_sync")
                        .and_then(|v| v.as_str())
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                        .map(|dt| dt.with_timezone(&Local));
                    let status = sub
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    records.insert(name.clone(), StateRecord { last_sync, status });
                }
            }
        }
    }
    StateStore { records }
}

pub fn save_state(
    output_dir: &Path,
    name: &str,
    last_sync: DateTime<Local>,
    status: &str,
) {
    let file = state_file(output_dir);
    let mut table: toml::Table = fs::read_to_string(&file)
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .unwrap_or_default();

    let mut sub = toml::map::Map::new();
    sub.insert("last_sync".into(), toml::Value::String(last_sync.to_rfc3339()));
    sub.insert("status".into(), toml::Value::String(status.to_string()));
    table.insert(name.into(), toml::Value::Table(sub));

    if let Some(parent) = file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(text) = toml::to_string(&table) {
        let _ = fs::write(&file, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home() {
        let p = expand_home("~/foo/bar");
        assert_eq!(p, dirs::home_dir().unwrap().join("foo/bar"));
        assert_eq!(expand_home("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn loads_devices_from_config() {
        let dir = std::env::temp_dir().join("dap-tui-config-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
[paths]
local_music_dir = "~/Music"

[storagebox]
source = "storagebox:masters"
exclude = [".DS_Store"]

[[devices]]
name = "Test iPod"
firmware = "rockbox"
mount_point = "/media/ipod"
music_folder = "Music"
"#,
        )
        .unwrap();
        let cfg = load(Some(path.display().to_string())).unwrap();
        assert_eq!(cfg.devices.len(), 1);
        let d = &cfg.devices[0];
        assert_eq!(d.name, "Test iPod");
        assert_eq!(d.firmware.label(), "rockbox");
        assert_eq!(d.music_folder, "Music");
        assert_eq!(cfg.storagebox_excludes, vec![".DS_Store"]);
        assert_eq!(cfg.local_music_dir, dirs::home_dir().unwrap().join("Music"));
    }

    #[test]
    fn loads_sony_firmware_as_rockbox_style() {
        let dir = std::env::temp_dir().join("dap-tui-sony-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
[[devices]]
name = "Sony NW-A55"
firmware = "sony"
mount_point = "/media/SD_CARD"
music_folder = "MUSIC"
"#,
        )
        .unwrap();
        let cfg = load(Some(path.display().to_string())).unwrap();
        assert_eq!(cfg.devices.len(), 1);
        let d = &cfg.devices[0];
        assert_eq!(d.firmware, Firmware::Sony);
        assert_eq!(d.music_folder, "MUSIC");
        assert_eq!(d.preserve_fat_order, false);
    }
}
