use crate::config::Config;
use crate::db::{SetupError, init_pool};
use crate::ratelimit::RateLimiter;
use crate::update::UpdateService;
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub data_dir: PathBuf,
    pub files_root: PathBuf,
    pub tmp_dir: PathBuf,
    /// 在线更新服务：检查版本、下载校验与互斥锁。
    pub updates: UpdateService,
    /// 登录与分享密码的失败计数（进程内）。
    pub limits: RateLimiter,
    /// 限制同时进行的 Argon2 校验数量，避免未认证请求耗尽 CPU。
    pub password_gate: Arc<Semaphore>,
    config: Arc<RwLock<Arc<Config>>>,
    config_path: Option<Arc<PathBuf>>,
}

impl AppState {
    pub async fn new(config: Config) -> Result<Self, SetupError> {
        Self::build(config, None).await
    }

    /// 启动时带上配置文件路径，网页设置与在线更新才能保存/替换。
    pub async fn with_config_file(
        config: Config,
        config_path: impl Into<PathBuf>,
    ) -> Result<Self, SetupError> {
        Self::build(config, Some(config_path.into())).await
    }

    async fn build(config: Config, config_path: Option<PathBuf>) -> Result<Self, SetupError> {
        let data_dir = config.data_dir.clone();
        tokio::fs::create_dir_all(&data_dir).await?;
        let files_root = data_dir.join("files");
        let tmp_dir = data_dir.join("tmp");
        tokio::fs::create_dir_all(&files_root).await?;
        tokio::fs::create_dir_all(&tmp_dir).await?;
        let pool = init_pool(&data_dir.join("cloudnest.db")).await?;
        crate::auth::warm_password_dummy();
        let password_slots = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(2)
            .clamp(2, 8);
        Ok(Self {
            pool,
            data_dir,
            files_root,
            tmp_dir,
            updates: UpdateService::new(),
            limits: RateLimiter::new(),
            password_gate: Arc::new(Semaphore::new(password_slots)),
            config: Arc::new(RwLock::new(Arc::new(config))),
            config_path: config_path.map(Arc::new),
        })
    }

    /// 当前生效配置的快照。读取后不再随热更新变化，保证单次请求内行为一致。
    pub fn config(&self) -> Arc<Config> {
        self.config
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 热更新生效配置（不修改磁盘文件）。
    pub fn replace_config(&self, config: Config) {
        let mut guard = self
            .config
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Arc::new(config);
    }

    /// 占用一个密码校验槽位；最多等待 2 秒，仍然繁忙时返回 None（调用方返回 429）。
    pub async fn password_slot(&self) -> Option<tokio::sync::SemaphorePermit<'_>> {
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.password_gate.acquire(),
        )
        .await
        .ok()?
        .ok()
    }

    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref().map(PathBuf::as_path)
    }

    pub fn user_dir(&self, owner_id: i64) -> PathBuf {
        self.files_root.join(owner_id.to_string())
    }

    pub fn stored_file_path(&self, owner_id: i64, storage_key: &str) -> PathBuf {
        self.user_dir(owner_id).join(storage_key)
    }

    pub fn temp_path(&self, token: &str) -> PathBuf {
        self.tmp_dir.join(token)
    }
}
