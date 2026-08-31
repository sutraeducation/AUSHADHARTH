use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use directories::ProjectDirs;

#[derive(Debug, Clone)]
pub struct RuntimePaths {
    pub database: PathBuf,
    pub logs: PathBuf,
    pub backups: PathBuf,
    pub attachments: PathBuf,
    pub configuration: PathBuf,
}

impl RuntimePaths {
    pub fn resolve(repository_root: &Path) -> anyhow::Result<Self> {
        let project_dirs = ProjectDirs::from("in", "Bizarth Technologies", "AUSHADHARTH")
            .context("Windows application-data directories are unavailable")?;
        let data_root = project_dirs.data_local_dir();
        let paths = Self {
            database: data_root.join("database"),
            logs: data_root.join("logs"),
            backups: data_root.join("backups"),
            attachments: data_root.join("attachments"),
            configuration: data_root.join("configuration"),
        };
        paths.validate(repository_root)?;
        Ok(paths)
    }

    pub fn database_file(&self) -> PathBuf {
        self.database.join("aushadharth.sqlite3")
    }

    pub fn validate(&self, repository_root: &Path) -> anyhow::Result<()> {
        validate_database_path(&self.database_file(), repository_root)
    }

    pub fn create_required_directories(&self) -> anyhow::Result<()> {
        for path in [
            &self.database,
            &self.logs,
            &self.backups,
            &self.attachments,
            &self.configuration,
        ] {
            std::fs::create_dir_all(path).context("failed to create runtime directory category")?;
        }
        Ok(())
    }
}

pub fn validate_database_path(database_path: &Path, repository_root: &Path) -> anyhow::Result<()> {
    let normalized_database = normalized_text(database_path);
    let normalized_repository = normalized_text(repository_root);

    let repository_prefix = format!("{}\\", normalized_repository);
    if normalized_database == normalized_repository
        || normalized_database.starts_with(&repository_prefix)
    {
        bail!("runtime database path must be outside the source repository");
    }
    if normalized_database.contains("\\onedrive\\") || normalized_database.contains("/onedrive/") {
        bail!("runtime database path must not be inside a OneDrive-synchronized location");
    }
    Ok(())
}

fn normalized_text(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_database_resolves_outside_repository() {
        let repository = Path::new(r"D:\AUSHADHARTH");
        let paths = RuntimePaths::resolve(repository).unwrap();
        assert!(!normalized_text(&paths.database_file()).starts_with(&normalized_text(repository)));
    }

    #[test]
    fn repository_database_path_is_rejected() {
        let repository = Path::new(r"D:\AUSHADHARTH");
        let database = repository.join("runtime").join("data.sqlite3");
        assert!(validate_database_path(&database, repository).is_err());
    }

    #[test]
    fn onedrive_database_path_is_rejected() {
        let repository = Path::new(r"D:\AUSHADHARTH");
        let database = Path::new(r"C:\Users\person\OneDrive\AUSHADHARTH\data.sqlite3");
        assert!(validate_database_path(database, repository).is_err());
    }
}
