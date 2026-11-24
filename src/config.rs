use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Listen address, e.g. "0.0.0.0:8080".
    pub listen: String,
    /// Directory holding the database and user files.
    pub data_dir: PathBuf,
    /// Public base URL, e.g. "https://drive.example.com".
    pub public_url: Option<String>,
    /// Add the Secure attribute to cookies. Enable when serving over HTTPS.
    pub secure_cookies: bool,
    /// Maximum size of a single non-chunked HTTP request body, in MiB.
    pub max_upload_size_mb: u64,
    /// Default quota for newly created users, in GiB. 0 means unlimited.
    pub default_quota_gb: i64,
    /// Session lifetime in days.
    pub session_days: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:8080".to_string(),
            data_dir: PathBuf::from("/var/lib/cloudnest"),
            public_url: None,
            secure_cookies: false,
            max_upload_size_mb: 2048,
            default_quota_gb: 10,
            session_days: 30,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let mut config = if path.exists() {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| ConfigError(format!("无法读取配置文件 {}: {e}", path.display())))?;
            toml::from_str::<Config>(&raw)
                .map_err(|e| ConfigError(format!("配置文件 {} 格式错误: {e}", path.display())))?
        } else {
            Config::default()
        };
        config.apply_env();
        config.normalize();
        Ok(config)
    }

    pub fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("CLOUDNEST_LISTEN") {
            if !v.trim().is_empty() {
                self.listen = v;
            }
        }
        if let Ok(v) = std::env::var("CLOUDNEST_DATA_DIR") {
            if !v.trim().is_empty() {
                self.data_dir = PathBuf::from(v);
            }
        }
        if let Ok(v) = std::env::var("CLOUDNEST_PUBLIC_URL") {
            if !v.trim().is_empty() {
                self.public_url = Some(v);
            }
        }
    }

    pub fn normalize(&mut self) {
        self.listen = self.listen.trim().to_string();
        if let Some(url) = &mut self.public_url {
            let trimmed = url.trim().trim_end_matches('/').to_string();
            if trimmed.is_empty() {
                self.public_url = None;
            } else {
                *url = trimmed;
            }
        }
        if self
            .public_url
            .as_deref()
            .is_some_and(|u| u.starts_with("https://"))
        {
            self.secure_cookies = true;
        }
        self.max_upload_size_mb = self.max_upload_size_mb.max(1);
        self.session_days = self.session_days.clamp(1, 365);
        self.default_quota_gb = self.default_quota_gb.max(0);
    }

    pub fn max_upload_bytes(&self) -> u64 {
        self.max_upload_size_mb.saturating_mul(1024 * 1024)
    }

    pub fn default_quota_bytes(&self) -> i64 {
        self.default_quota_gb.saturating_mul(1024 * 1024 * 1024)
    }

    pub fn session_seconds(&self) -> i64 {
        self.session_days.saturating_mul(24 * 60 * 60)
    }

    pub fn base_url(&self) -> String {
        self.public_url
            .clone()
            .unwrap_or_else(|| format!("http://{}", self.listen))
    }
}
