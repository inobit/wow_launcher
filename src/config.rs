use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::service::ServiceKind;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub mysql_path: String,
    pub auth_path: String,
    pub world_path: String,
    pub client_path: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mysql_path: String::new(),
            auth_path: String::new(),
            world_path: String::new(),
            client_path: String::new(),
        }
    }
}

impl Config {
    pub fn path_set(&self, kind: ServiceKind) -> bool {
        !self.path_for(kind).trim().is_empty()
    }

    pub fn path_for(&self, kind: ServiceKind) -> &str {
        match kind {
            ServiceKind::Mysql => &self.mysql_path,
            ServiceKind::Auth => &self.auth_path,
            ServiceKind::World => &self.world_path,
            ServiceKind::Client => &self.client_path,
        }
    }

    pub fn set_path(&mut self, kind: ServiceKind, value: String) {
        match kind {
            ServiceKind::Mysql => self.mysql_path = value,
            ServiceKind::Auth => self.auth_path = value,
            ServiceKind::World => self.world_path = value,
            ServiceKind::Client => self.client_path = value,
        }
    }

    #[allow(dead_code)]
    pub fn validate_all(&self) -> Result<(), String> {
        for kind in ServiceKind::ALL {
            let p = self.path_for(kind);
            if p.trim().is_empty() {
                return Err(format!("{} 未配置: 请为当前应用设置启动路径。", kind.label()));
            }
        }
        Ok(())
    }
}

fn config_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("wow_launcher.json"))
}

pub fn load() -> Config {
    match config_path() {
        Some(path) if path.exists() => {
            let data = std::fs::read(&path);
            match data {
                Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
                Err(e) => {
                    eprintln!("读取配置失败: {e}");
                    Config::default()
                }
            }
        }
        _ => Config::default(),
    }
}

pub fn save(config: &Config) -> Result<(), String> {
    let path = config_path().ok_or_else(|| "无法定位启动器目录".to_string())?;
    let json = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("写入配置失败: {e}"))
}