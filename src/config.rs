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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    /// Add the Secure attribute to cookies. Enable when serving over HTTPS.
    pub secure_cookies: bool,
    /// Maximum size of a single non-chunked HTTP request body, in MiB.
    /// Larger files use the 4 MiB chunked upload channel from the web UI.
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
        let mut config = Self::load_raw(path)?;
        config.apply_env();
        config.normalize();
        Ok(config)
    }

    /// 只读取配置文件并校验，不应用环境变量覆盖。
    /// 网页设置需要区分“文件里写的值”和“实际运行的生效值”，因此单独提供此入口。
    pub fn load_raw(path: &Path) -> Result<Self, ConfigError> {
        let config = if path.exists() {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| ConfigError(format!("无法读取配置文件 {}: {e}", path.display())))?;
            toml::from_str::<Config>(&raw)
                .map_err(|e| ConfigError(format!("配置文件 {} 格式错误: {e}", path.display())))?
        } else {
            Config::default()
        };
        Ok(config)
    }

    /// 原子地写回配置文件。网页保存时整份重写，文件中的手工注释不会保留。
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let body = toml::to_string_pretty(self)
            .map_err(|e| ConfigError(format!("生成配置内容失败: {e}")))?;
        let text = format!(
            "# CloudNest 配置文件
# 由网页“站点设置”保存时会整体重写本文件。

{body}"
        );
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        if !parent.exists() {
            return Err(ConfigError(format!("配置目录不存在：{}", parent.display())));
        }
        let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
        std::fs::write(&tmp, text)
            .map_err(|e| ConfigError(format!("无法写入临时配置文件 {}: {e}", tmp.display())))?;
        match std::fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(first_err) => {
                // Windows 上目标存在时重命名可能失败；退化为先删除再改名。
                if path.exists()
                    && std::fs::remove_file(path).is_ok()
                    && std::fs::rename(&tmp, path).is_ok()
                {
                    return Ok(());
                }
                let _ = std::fs::remove_file(&tmp);
                Err(ConfigError(format!(
                    "无法保存配置文件 {}: {first_err}（请检查服务用户是否有写权限）",
                    path.display()
                )))
            }
        }
    }

    /// 校验用户可编辑字段，返回面向用户的中文错误信息。
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.listen.is_empty() {
            return Err(ConfigError("监听地址不能为空".to_string()));
        }
        if self.listen.parse::<std::net::SocketAddr>().is_err() {
            return Err(ConfigError(
                "监听地址格式不正确，应形如 127.0.0.1:8080".to_string(),
            ));
        }
        if let Some(url) = &self.public_url {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(ConfigError(
                    "对外访问地址必须以 http:// 或 https:// 开头".to_string(),
                ));
            }
            let host = url.split_once("://").map(|(_, rest)| rest).unwrap_or("");
            if host.is_empty() || host.contains(char::is_whitespace) {
                return Err(ConfigError("对外访问地址格式不正确".to_string()));
            }
        }
        if !(1..=1_048_576).contains(&self.max_upload_size_mb) {
            return Err(ConfigError(
                "单次请求体上限应在 1 到 1048576 之间（单位 MiB）".to_string(),
            ));
        }
        if !(0..=1_048_576).contains(&self.default_quota_gb) {
            return Err(ConfigError(
                "默认存储配额应在 0 到 1048576 之间（单位 GiB，0 为不限）".to_string(),
            ));
        }
        if !(1..=365).contains(&self.session_days) {
            return Err(ConfigError("会话有效期应在 1 到 365 天之间".to_string()));
        }
        if self.data_dir.as_os_str().is_empty() {
            return Err(ConfigError("数据目录不能为空".to_string()));
        }
        Ok(())
    }

    /// 列出被环境变量覆盖的字段名，用于在网页上提示“以环境变量为准”。
    pub fn env_overrides() -> Vec<&'static str> {
        let mut fields = Vec::new();
        for (var, field) in [
            ("CLOUDNEST_LISTEN", "listen"),
            ("CLOUDNEST_DATA_DIR", "data_dir"),
            ("CLOUDNEST_PUBLIC_URL", "public_url"),
        ] {
            if std::env::var(var)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
            {
                fields.push(field);
            }
        }
        fields
    }

    pub fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("CLOUDNEST_LISTEN")
            && !v.trim().is_empty()
        {
            self.listen = v;
        }
        if let Ok(v) = std::env::var("CLOUDNEST_DATA_DIR")
            && !v.trim().is_empty()
        {
            self.data_dir = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("CLOUDNEST_PUBLIC_URL")
            && !v.trim().is_empty()
        {
            self.public_url = Some(v);
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
