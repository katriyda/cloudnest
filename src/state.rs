use crate::config::Config;
use crate::db::{SetupError, init_pool};
use sqlx::SqlitePool;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub config: Arc<Config>,
    pub data_dir: PathBuf,
    pub files_root: PathBuf,
    pub tmp_dir: PathBuf,
}

impl AppState {
    pub async fn new(config: Config) -> Result<Self, SetupError> {
        let data_dir = config.data_dir.clone();
        tokio::fs::create_dir_all(&data_dir).await?;
        let files_root = data_dir.join("files");
        let tmp_dir = data_dir.join("tmp");
        tokio::fs::create_dir_all(&files_root).await?;
        tokio::fs::create_dir_all(&tmp_dir).await?;
        let pool = init_pool(&data_dir.join("cloudnest.db")).await?;
        Ok(Self {
            pool,
            config: Arc::new(config),
            data_dir,
            files_root,
            tmp_dir,
        })
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
