use std::{
    fs,
    path::{Path, PathBuf},
};

use color_eyre::eyre::{Result, WrapErr, eyre};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AppConfig {
    #[serde(default = "default_profile_name")]
    pub profile_name: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub enable_sync: bool,
    #[serde(default)]
    pub editor: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            profile_name: default_profile_name(),
            data_dir: default_data_dir(),
            enable_sync: false,
            editor: std::env::var("EDITOR").ok(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn new() -> Result<Self> {
        let dirs = project_dirs()?;

        Ok(Self {
            path: dirs.config_dir().join("config.toml"),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_or_default(&self) -> Result<AppConfig> {
        if !self.path.exists() {
            return Ok(AppConfig::default());
        }

        let raw = fs::read_to_string(&self.path)
            .wrap_err_with(|| format!("failed to read {}", self.path.display()))?;

        toml::from_str(&raw).wrap_err_with(|| format!("failed to parse {}", self.path.display()))
    }

    pub fn ensure_exists(&self) -> Result<AppConfig> {
        let config = self.load_or_default()?;
        self.save(&config)?;

        Ok(config)
    }

    pub fn save(&self, config: &AppConfig) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
        }

        let raw = toml::to_string_pretty(config).wrap_err("failed to serialize config")?;
        fs::write(&self.path, raw)
            .wrap_err_with(|| format!("failed to write {}", self.path.display()))?;

        Ok(())
    }

    pub fn reset(&self) -> Result<AppConfig> {
        let config = AppConfig::default();
        self.save(&config)?;

        Ok(config)
    }
}

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("com", "jinwoo", "remem0")
        .ok_or_else(|| eyre!("could not resolve a user config directory"))
}

fn default_profile_name() -> String {
    "default".to_string()
}

fn default_data_dir() -> PathBuf {
    project_dirs()
        .map(|dirs| dirs.data_dir().to_path_buf())
        .unwrap_or_else(|_| PathBuf::from(".remem0"))
}
