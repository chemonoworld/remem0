use std::{
    env, fs,
    path::{Path, PathBuf},
};

use clap::ValueEnum;
use color_eyre::eyre::{Result, WrapErr, eyre};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AppConfig {
    #[serde(default = "default_active_profile")]
    pub active_profile: String,
    #[serde(default = "default_search")]
    pub default_search: String,
    #[serde(default)]
    pub editor: Option<String>,
    #[serde(default)]
    pub profiles: Vec<ProfileConfig>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            active_profile: default_active_profile(),
            default_search: default_search(),
            editor: env::var("EDITOR").ok(),
            profiles: Vec::new(),
        }
    }
}

impl AppConfig {
    pub fn active_profile(&self) -> Result<&ProfileConfig> {
        self.profile(&self.active_profile)
    }

    pub fn profile(&self, name: &str) -> Result<&ProfileConfig> {
        self.profiles
            .iter()
            .find(|profile| profile.name == name)
            .ok_or_else(|| eyre!("profile {name:?} is not configured"))
    }

    pub fn upsert_profile(&mut self, profile: ProfileConfig) {
        if let Some(existing) = self
            .profiles
            .iter_mut()
            .find(|existing| existing.name == profile.name)
        {
            *existing = profile;
        } else {
            self.profiles.push(profile);
            self.profiles
                .sort_by(|left, right| left.name.cmp(&right.name));
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProfileConfig {
    pub name: String,
    pub root: PathBuf,
    #[serde(default)]
    pub storage: StorageMode,
}

pub fn normalize_root(path: PathBuf) -> PathBuf {
    let path = expand_tilde(path);
    if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn expand_tilde(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return env::var_os("HOME").map(PathBuf::from).unwrap_or(path);
    }
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    path
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum StorageMode {
    #[default]
    Local,
    Obsidian,
    Git,
}

impl std::fmt::Display for StorageMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => f.write_str("local"),
            Self::Obsidian => f.write_str("obsidian"),
            Self::Git => f.write_str("git"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConfigStore {
    root: PathBuf,
    path: PathBuf,
}

impl ConfigStore {
    pub fn new() -> Result<Self> {
        let root = rem_home()?;
        Ok(Self {
            path: root.join("config.toml"),
            root,
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
        fs::create_dir_all(&self.root)
            .wrap_err_with(|| format!("failed to create {}", self.root.display()))?;

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

fn rem_home() -> Result<PathBuf> {
    if let Some(path) = env::var_os("REM_HOME") {
        return Ok(PathBuf::from(path));
    }

    if let Some(home) = env::var_os("HOME") {
        return Ok(PathBuf::from(home).join(".rem"));
    }

    Err(eyre!("could not resolve HOME; set REM_HOME explicitly"))
}

fn default_active_profile() -> String {
    "default".to_string()
}

fn default_search() -> String {
    "auto".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_profile_replaces_by_name() {
        let mut config = AppConfig::default();
        config.upsert_profile(ProfileConfig {
            name: "default".to_string(),
            root: PathBuf::from("/tmp/a"),
            storage: StorageMode::Local,
        });
        config.upsert_profile(ProfileConfig {
            name: "default".to_string(),
            root: PathBuf::from("/tmp/b"),
            storage: StorageMode::Git,
        });

        assert_eq!(config.profiles.len(), 1);
        assert_eq!(config.profiles[0].root, PathBuf::from("/tmp/b"));
        assert_eq!(config.profiles[0].storage, StorageMode::Git);
    }

    #[test]
    fn normalize_root_expands_home_tilde() {
        let Some(home) = env::var_os("HOME") else {
            return;
        };

        assert_eq!(
            normalize_root(PathBuf::from("~/rem-test")),
            PathBuf::from(home).join("rem-test")
        );
    }
}
