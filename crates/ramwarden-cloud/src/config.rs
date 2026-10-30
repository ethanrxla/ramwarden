//! Where the server finds its database.
//!
//! The connection string carries the database user's password, so it follows
//! the same rule as the model keys: never in a tracked file. It is read, in
//! order, from
//!
//! 1. `$MONGODB_URI`, which is how a host such as Fly.io supplies it;
//! 2. a `.env` file in the working directory, for local development;
//! 3. `mongodb_uri` in `~/.config/ramwarden/secrets.toml`.
//!
//! Both files are gitignored. The value is held in a [`Secret`] so a debug log
//! of the config prints a fingerprint, not the password.

use std::path::{Path, PathBuf};

use ramwarden_ai::secret::Secret;
use serde::Deserialize;

/// The database name when none is configured. One cluster can hold a
/// `ramwarden-dev` beside `ramwarden` without a second cluster.
pub const DEFAULT_DATABASE: &str = "ramwarden";

#[derive(Clone, Debug)]
pub struct CloudConfig {
    pub mongodb_uri: Secret,
    pub database: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(
        "no MongoDB connection string: set MONGODB_URI, or mongodb_uri in {0}"
    )]
    Missing(String),
    #[error("{0}: {1}")]
    Read(String, String),
    #[error("the MongoDB connection string must start with mongodb+srv:// or mongodb://")]
    BadScheme,
}

#[derive(Default, Deserialize)]
struct SecretsFile {
    #[serde(default)]
    mongodb_uri: Option<String>,
    #[serde(default)]
    mongodb_database: Option<String>,
}

impl CloudConfig {
    /// Load from the environment, `.env`, then `secrets.toml`.
    pub fn load() -> Result<Self, ConfigError> {
        // A missing .env is the normal case in production.
        let _ = dotenvy::dotenv();
        Self::from_sources(
            std::env::var("MONGODB_URI").ok(),
            std::env::var("MONGODB_DATABASE").ok(),
            &secrets_path(),
        )
    }

    /// The environment wins over the file, matching the daemon's own config.
    pub fn from_sources(
        env_uri: Option<String>,
        env_db: Option<String>,
        secrets: &Path,
    ) -> Result<Self, ConfigError> {
        let file = if secrets.exists() {
            let text = std::fs::read_to_string(secrets)
                .map_err(|e| ConfigError::Read(secrets.display().to_string(), e.to_string()))?;
            toml::from_str::<SecretsFile>(&text)
                .map_err(|e| ConfigError::Read(secrets.display().to_string(), e.to_string()))?
        } else {
            SecretsFile::default()
        };

        let uri = nonblank(env_uri)
            .or(nonblank(file.mongodb_uri))
            .ok_or_else(|| ConfigError::Missing(secrets.display().to_string()))?;
        if !(uri.starts_with("mongodb+srv://") || uri.starts_with("mongodb://")) {
            return Err(ConfigError::BadScheme);
        }
        let database = nonblank(env_db)
            .or(nonblank(file.mongodb_database))
            .unwrap_or_else(|| DEFAULT_DATABASE.to_string());

        Ok(CloudConfig { mongodb_uri: Secret::new(uri), database })
    }
}

fn nonblank(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn secrets_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".config/ramwarden/secrets.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    const URI: &str = "mongodb+srv://ramwarden-server:hunter2hunter2@cluster0.abcde.mongodb.net/";

    #[test]
    fn the_environment_wins_over_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secrets.toml");
        std::fs::write(&p, "mongodb_uri = \"mongodb://file\"\n").unwrap();
        let c = CloudConfig::from_sources(Some(URI.into()), None, &p).unwrap();
        assert_eq!(c.mongodb_uri.expose(), URI);
        assert_eq!(c.database, DEFAULT_DATABASE);
    }

    #[test]
    fn the_file_is_used_when_the_environment_is_blank() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("secrets.toml");
        std::fs::write(&p, format!("mongodb_uri = \"{URI}\"\nmongodb_database = \"ramwarden-dev\"\n"))
            .unwrap();
        let c = CloudConfig::from_sources(Some("  ".into()), None, &p).unwrap();
        assert_eq!(c.mongodb_uri.expose(), URI);
        assert_eq!(c.database, "ramwarden-dev");
    }

    #[test]
    fn nothing_configured_says_where_to_put_it() {
        let dir = tempfile::tempdir().unwrap();
        let err = CloudConfig::from_sources(None, None, &dir.path().join("secrets.toml"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("MONGODB_URI"), "{err}");
    }

    #[test]
    fn a_pasted_hostname_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let err = CloudConfig::from_sources(
            Some("cluster0.abcde.mongodb.net".into()),
            None,
            &dir.path().join("x"),
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::BadScheme));
    }

    #[test]
    fn the_password_never_reaches_a_debug_log() {
        let dir = tempfile::tempdir().unwrap();
        let c = CloudConfig::from_sources(Some(URI.into()), None, &dir.path().join("x")).unwrap();
        let printed = format!("{c:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
    }
}
