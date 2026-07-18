use std::{
    fs, io,
    path::{Path, PathBuf},
};

use color_eyre::eyre::{Result, WrapErr, eyre};

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

    pub fn conflicts_dir(&self) -> PathBuf {
        self.root.join("conflicts")
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

    pub fn ensure_layout(&self) -> Result<()> {
        for path in [
            self.root.join("memories"),
            self.short_dir(),
            self.long_dir(),
            self.policies_dir(),
            self.inbox_dir(),
            self.archive_dir(),
            self.conflicts_dir(),
            self.rem_dir(),
            self.cache_dir(),
            self.tx_dir(),
        ] {
            ensure_regular_directory(&path, "vault")?;
        }
        Ok(())
    }

    pub fn init(&self) -> Result<()> {
        self.ensure_layout()?;

        policy::write_default_policies(self)
    }
}

pub(crate) fn ensure_regular_directory(path: &Path, label: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(eyre!(
                "refusing to use symlinked {label} directory {}",
                path.display()
            ));
        }
        Ok(metadata) if metadata.is_dir() => return Ok(()),
        Ok(_) => {
            return Err(eyre!("{label} path {} must be a directory", path.display()));
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).wrap_err_with(|| format!("failed to inspect {}", path.display()));
        }
    }

    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)
                .wrap_err_with(|| format!("failed to inspect {}", path.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                Err(eyre!(
                    "refusing to use non-regular {label} directory {}",
                    path.display()
                ))
            } else {
                Ok(())
            }
        }
        Err(err) => Err(err).wrap_err_with(|| format!("failed to create {}", path.display())),
    }
}
