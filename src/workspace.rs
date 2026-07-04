use std::{
    fs,
    path::{Path, PathBuf},
};

use color_eyre::eyre::{Result, WrapErr};

use crate::{config::ProfileConfig, policy};

#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    pub fn new(profile: &ProfileConfig) -> Self {
        Self {
            root: profile.root.clone(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn short_dir(&self) -> PathBuf {
        self.root.join("memories").join("short")
    }

    pub fn long_dir(&self) -> PathBuf {
        self.root.join("memories").join("long")
    }

    pub fn policies_dir(&self) -> PathBuf {
        self.root.join("policies")
    }

    pub fn inbox_dir(&self) -> PathBuf {
        self.root.join("inbox")
    }

    pub fn archive_dir(&self) -> PathBuf {
        self.root.join("archive")
    }

    pub fn rem_dir(&self) -> PathBuf {
        self.root.join(".rem")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.rem_dir().join("cache")
    }

    pub fn tx_dir(&self) -> PathBuf {
        self.rem_dir().join("tx")
    }

    pub fn index_path(&self) -> PathBuf {
        self.cache_dir().join("index.sqlite")
    }

    pub fn init(&self) -> Result<()> {
        for path in [
            self.short_dir(),
            self.long_dir(),
            self.policies_dir(),
            self.inbox_dir(),
            self.archive_dir(),
            self.cache_dir(),
        ] {
            fs::create_dir_all(&path)
                .wrap_err_with(|| format!("failed to create {}", path.display()))?;
        }

        policy::write_default_policies(self)
    }
}
