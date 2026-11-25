use crate::config::Config;
use crate::state::AppState;
use clap::{Parser, Subcommand};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqlitePoolOptions;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use tracing_subscriber::EnvFilter;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPO: &str = "katriyda/cloudnest";
const SERVICE_NAME: &str = "cloudnest.service";
const SERVICE_FILE: &str = "/etc/systemd/system/cloudnest.service";
const INSTALL_DIR: &str = "/opt/cloudnest";

#[derive(Debug)]
pub struct CliError(pub String);

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for CliError {}

fn fail(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(CliError(message.into()))
}

type CliResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Parser)]
#[command(name = "cloudnest", version, about = "CloudNest - 简单可靠的个人网盘")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// 启动 Web 服务
    Serve {
        /// 配置文件路径
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
        /// 覆盖监听地址，例如 0.0.0.0:8080
        #[arg(long)]
        listen: Option<String>,
        /// 覆盖数据目录
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// 显示版本
    Version,
    /// 显示运行状态
    Status {
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },
    /// 检查并更新到最新正式版本
    Update,
    /// 卸载程序（默认保留用户数据）
    Uninstall {
        /// 同时删除配置、数据库和用户文件
        #[arg(long)]
        purge: bool,
        /// 跳过删除数据的交互确认
        #[arg(long)]
        yes: bool,
    },
    /// 备份数据库和用户文件
    Backup {
        /// 备份输出目录
        #[arg(long, short = 'o')]
        output: Option<PathBuf>,
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },
    /// 从备份恢复数据（需先停止服务）
    Restore {
        /// 备份目录
        #[arg(long)]
        from: PathBuf,
        /// 覆盖现有数据（会先移动到 .bak 目录）
        #[arg(long)]
        force: bool,
        #[arg(long, short = 'c')]
        config: Option<PathBuf>,
    },
}

fn default_config_path() -> PathBuf {
    std::env::var("CLOUDNEST_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/etc/cloudnest/config.toml"))
}

fn resolve_config(config: Option<PathBuf>) -> PathBuf {
    config.unwrap_or_else(default_config_path)
}

pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve {
        config: None,
        listen: None,
        data_dir: None,
    }) {
        Command::Serve {
            config,
            listen,
            data_dir,
        } => serve(config, listen, data_dir).await,
        Command::Version => {
            println!("CloudNest cloudnest {VERSION}");
            Ok(())
        }
        Command::Status { config } => status(resolve_config(config)).await,
        Command::Update => update().await,
        Command::Uninstall { purge, yes } => uninstall(purge, yes),
        Command::Backup { output, config } => {
            let output = output.unwrap_or_else(|| PathBuf::from("./cloudnest-backup"));
            backup(resolve_config(config), output).await
        }
        Command::Restore {
            from,
            force,
            config,
        } => restore(resolve_config(config), from, force).await,
    }
}

async fn serve(
    config: Option<PathBuf>,
    listen: Option<String>,
    data_dir: Option<PathBuf>,
) -> CliResult<()> {
    let config_path = resolve_config(config);
    let mut cfg = Config::load(&config_path)?;
    if let Some(listen) = listen {
        cfg.listen = listen;
    }
    if let Some(data_dir) = data_dir {
        cfg.data_dir = data_dir;
    }
    cfg.normalize();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info")),
        )
        .init();

    tracing::info!(
        "CloudNest cloudnest {} 启动，配置 {}，数据目录 {}，监听 {}",
        VERSION,
        config_path.display(),
        cfg.data_dir.display(),
        cfg.listen
    );
    let state = AppState::new(cfg).await?;
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        let _ = crate::storage::cleanup_stale_uploads(
            &cleanup_state,
            crate::util::now_ts() - 7 * 24 * 60 * 60,
        )
        .await;
    });
    let listen = state.config.listen.clone();
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .map_err(|e| fail(format!("无法监听 {listen}: {e}")))?;
    tracing::info!("服务已就绪：http://{listen}");
    let app = crate::routes::router(state.clone());
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| fail(format!("服务异常退出: {e}")))?;
    state.pool.close().await;
    tracing::info!("服务已停止");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(unix)]
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    #[cfg(not(unix))]
    ctrl_c.await;
}

async fn status(config_path: PathBuf) -> CliResult<()> {
    let config = Config::load(&config_path)?;
    println!("CloudNest cloudnest {VERSION}");
    println!("配置文件: {}", config_path.display());
    println!("监听地址: {}", config.listen);
    println!("数据目录: {}", config.data_dir.display());
    println!(
        "数据库:   {}",
        config.data_dir.join("cloudnest.db").display()
    );

    #[cfg(target_os = "linux")]
    {
        let state = ProcessCommand::new("systemctl")
            .args(["is-active", SERVICE_NAME])
            .output();
        match state {
            Ok(output) => println!(
                "服务状态: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            ),
            Err(_) => println!("服务状态: 未知（systemd 不可用）"),
        }
    }
    #[cfg(not(target_os = "linux"))]
    println!("服务状态: 当前平台不支持 systemd");

    let db_path = config.data_dir.join("cloudnest.db");
    if db_path.exists() {
        if let Ok(pool) = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&db_path)
                    .read_only(true),
            )
            .await
        {
            let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
                .fetch_one(&pool)
                .await
                .unwrap_or(0);
            let files: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE deleted_at IS NULL")
                    .fetch_one(&pool)
                    .await
                    .unwrap_or(0);
            let bytes: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(size), 0) FROM files WHERE deleted_at IS NULL AND is_dir = 0",
            )
            .fetch_one(&pool)
            .await
            .unwrap_or(0);
            println!("账户数量: {users}");
            println!("文件数量: {files}");
            println!("已用空间: {}", crate::util::human_size(bytes));
            pool.close().await;
        }
    } else {
        println!("数据库尚未初始化");
    }
    Ok(())
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<ReleaseAsset>,
}

fn version_triplet(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split(['.', '-'])
        .take(3)
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn is_newer(latest: &str, current: &str) -> bool {
    let a = version_triplet(latest);
    let b = version_triplet(current);
    for i in 0..3 {
        let ai = a.get(i).copied().unwrap_or(0);
        let bi = b.get(i).copied().unwrap_or(0);
        if ai != bi {
            return ai > bi;
        }
    }
    false
}

fn target_arch() -> CliResult<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64"),
        "aarch64" => Ok("aarch64"),
        other => Err(fail(format!("不支持的 CPU 架构: {other}"))),
    }
}

async fn update() -> CliResult<()> {
    if !cfg!(target_os = "linux") {
        return Err(fail("自动更新仅支持 Linux"));
    }
    let arch = target_arch()?;
    let asset_name = format!("cloudnest-linux-{arch}.tar.gz");
    println!("当前版本: {VERSION}");
    let release: Release = ureq::get(&format!(
        "https://api.github.com/repos/{REPO}/releases/latest"
    ))
    .header("User-Agent", "cloudnest-updater")
    .header("Accept", "application/vnd.github+json")
    .call()
    .map_err(|e| fail(format!("检查更新失败: {e}")))?
    .body_mut()
    .read_json()
    .map_err(|e| fail(format!("解析版本信息失败: {e}")))?;
    println!("最新版本: {}", release.tag_name);
    if !is_newer(&release.tag_name, VERSION) {
        println!("已经是最新版本。");
        return Ok(());
    }
    let archive = release
        .assets
        .iter()
        .find(|asset| asset.name == asset_name)
        .ok_or_else(|| fail(format!("最新版本中没有找到 {asset_name}")))?
        .browser_download_url
        .clone();
    let checksum_url = release
        .assets
        .iter()
        .find(|asset| asset.name == "SHA256SUMS")
        .map(|asset| asset.browser_download_url.clone())
        .or_else(|| {
            release
                .assets
                .iter()
                .find(|asset| asset.name == format!("{asset_name}.sha256"))
                .map(|asset| asset.browser_download_url.clone())
        })
        .ok_or_else(|| fail("发布中缺少 SHA256 校验文件"))?;
    let checksum_text = ureq::get(&checksum_url)
        .header("User-Agent", "cloudnest-updater")
        .call()
        .map_err(|e| fail(format!("下载校验文件失败: {e}")))?
        .body_mut()
        .read_to_string()
        .map_err(|e| fail(format!("读取校验文件失败: {e}")))?;
    let expected = checksum_text
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            let mut parts = trimmed.split_whitespace();
            let hash = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            if name == asset_name {
                Some(hash.to_ascii_lowercase())
            } else {
                None
            }
        })
        .ok_or_else(|| fail(format!("校验文件中没有 {asset_name}")))?;

    let temp = tempfile::TempDir::new().map_err(|e| fail(format!("创建临时目录失败: {e}")))?;
    let archive_path = temp.path().join(&asset_name);
    println!("下载 {asset_name} …");
    let bytes = ureq::get(&archive)
        .header("User-Agent", "cloudnest-updater")
        .call()
        .map_err(|e| fail(format!("下载新版本失败: {e}")))?
        .body_mut()
        .read_to_vec()
        .map_err(|e| fail(format!("读取下载内容失败: {e}")))?;
    let actual = hex::encode(Sha256::digest(&bytes));
    if actual != expected {
        return Err(fail(format!(
            "SHA256 校验失败：期望 {expected}，实际 {actual}"
        )));
    }
    std::fs::write(&archive_path, &bytes).map_err(|e| fail(format!("写入临时文件失败: {e}")))?;

    let new_exe = temp.path().join("cloudnest.new");
    let mut found = false;
    let file = std::fs::File::open(&archive_path)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path.file_name().and_then(|n| n.to_str()) == Some("cloudnest")
            && entry.header().entry_type().is_file()
        {
            entry
                .unpack(&new_exe)
                .map_err(|e| fail(format!("解压新版本失败: {e}")))?;
            found = true;
            break;
        }
    }
    if !found {
        return Err(fail("发布包中没有找到 cloudnest 可执行文件"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new_exe, std::fs::Permissions::from_mode(0o755))?;
    }

    // Backup the database before touching the running installation.
    let config = Config::load(&default_config_path())?;
    if config.data_dir.join("cloudnest.db").exists() {
        let backup_dir = config.data_dir.join("backups");
        std::fs::create_dir_all(&backup_dir)?;
        let backup_path = backup_dir.join(format!(
            "pre-update-{}-{}.db",
            VERSION,
            chrono::Local::now().format("%Y%m%d%H%M%S")
        ));
        let escaped = backup_path
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('\'', "''");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(config.data_dir.join("cloudnest.db")),
            )
            .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("VACUUM INTO '{escaped}'")))
            .execute(&pool)
            .await
            .map_err(|e| fail(format!("更新前数据库备份失败: {e}")))?;
        pool.close().await;
        println!("已备份数据库到 {}", backup_path.display());
    }

    let current_exe =
        std::env::current_exe().map_err(|e| fail(format!("无法定位当前程序: {e}")))?;
    let previous = current_exe.with_extension("previous");
    std::fs::copy(&current_exe, &previous)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&previous, std::fs::Permissions::from_mode(0o755))?;
    }
    println!("正在替换程序文件 {} …", current_exe.display());
    if let Err(err) = std::fs::rename(&new_exe, &current_exe) {
        let _ = std::fs::copy(&previous, &current_exe);
        return Err(fail(format!("替换程序失败，已回滚: {err}")));
    }

    if Path::new(SERVICE_FILE).exists() {
        println!("正在重启 systemd 服务…");
        let restart = ProcessCommand::new("systemctl")
            .args(["restart", SERVICE_NAME])
            .status();
        if !matches!(restart, Ok(status) if status.success()) {
            let _ = std::fs::copy(&previous, &current_exe);
            let _ = ProcessCommand::new("systemctl")
                .args(["restart", SERVICE_NAME])
                .status();
            return Err(fail("服务重启失败，已回滚到旧版本"));
        }
    }
    println!("更新完成：{}", release.tag_name);
    Ok(())
}

fn uninstall(purge: bool, yes: bool) -> CliResult<()> {
    if !cfg!(target_os = "linux") {
        return Err(fail("卸载命令仅支持 Linux"));
    }
    if purge && !yes {
        println!("警告：--purge 将永久删除配置文件、数据库和全部用户文件。");
        print!("请输入 yes 继续: ");
        use std::io::Write;
        std::io::stdout().flush().ok();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if input.trim() != "yes" {
            return Err(fail("已取消卸载，未删除任何数据"));
        }
    }
    let _ = ProcessCommand::new("systemctl")
        .args(["stop", SERVICE_NAME])
        .status();
    let _ = ProcessCommand::new("systemctl")
        .args(["disable", SERVICE_NAME])
        .status();
    let _ = std::fs::remove_file(SERVICE_FILE);
    let _ = ProcessCommand::new("systemctl")
        .arg("daemon-reload")
        .status();
    let _ = std::fs::remove_dir_all(INSTALL_DIR);
    println!("已停止服务并移除程序文件。");
    if purge {
        let config = Config::load(&default_config_path())?;
        let _ = std::fs::remove_dir_all(&config.data_dir);
        let _ = std::fs::remove_dir_all("/etc/cloudnest");
        println!("已删除配置和用户数据。");
    } else {
        println!("用户数据库和文件已保留在 /var/lib/cloudnest（默认配置路径）。");
        println!("确认不再需要时可手动删除，或使用 cloudnest uninstall --purge。");
    }
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

async fn backup(config_path: PathBuf, output: PathBuf) -> CliResult<()> {
    let config = Config::load(&config_path)?;
    let db_path = config.data_dir.join("cloudnest.db");
    if !db_path.exists() {
        return Err(fail(format!("数据库不存在：{}", db_path.display())));
    }
    if output.exists() && std::fs::read_dir(&output)?.next().is_some() {
        return Err(fail(format!(
            "备份目录 {} 已存在且不为空",
            output.display()
        )));
    }
    std::fs::create_dir_all(&output)?;
    let snapshot = output.join("cloudnest.db");
    let escaped = snapshot
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('\'', "''");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&db_path))
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("VACUUM INTO '{escaped}'")))
        .execute(&pool)
        .await
        .map_err(|e| fail(format!("数据库快照失败: {e}")))?;
    pool.close().await;
    let files_src = config.data_dir.join("files");
    if files_src.exists() {
        copy_dir_all(&files_src, &output.join("files"))
            .map_err(|e| fail(format!("复制用户文件失败: {e}")))?;
    }
    let manifest = serde_json::json!({
        "app": "CloudNest",
        "version": VERSION,
        "created_at": chrono::Local::now().to_rfc3339(),
        "format": 1
    });
    std::fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    println!("备份完成：{}", output.display());
    println!("提示：强烈建议在低峰期或停止服务后执行备份，以获得文件与数据库的完全一致快照。");
    Ok(())
}

async fn restore(config_path: PathBuf, from: PathBuf, force: bool) -> CliResult<()> {
    let config = Config::load(&config_path)?;
    let backup_db = from.join("cloudnest.db");
    let backup_files = from.join("files");
    if !backup_db.exists() || !backup_files.exists() {
        return Err(fail(format!(
            "{} 不是有效的备份目录（需要 cloudnest.db 和 files/）",
            from.display()
        )));
    }
    #[cfg(target_os = "linux")]
    {
        let active = ProcessCommand::new("systemctl")
            .args(["is-active", SERVICE_NAME])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim() == "active")
            .unwrap_or(false);
        if active {
            return Err(fail(
                "服务正在运行，请先执行 systemctl stop cloudnest.service",
            ));
        }
    }
    if config.data_dir.exists() && std::fs::read_dir(&config.data_dir)?.next().is_some() && !force {
        return Err(fail(format!(
            "数据目录 {} 已存在。确认要覆盖时请加 --force（现有数据会移动到 .bak 目录）",
            config.data_dir.display()
        )));
    }
    if config.data_dir.exists() && std::fs::read_dir(&config.data_dir)?.next().is_some() {
        let backup_old = config.data_dir.with_extension(format!(
            "bak-{}",
            chrono::Local::now().format("%Y%m%d%H%M%S")
        ));
        std::fs::rename(&config.data_dir, &backup_old)
            .map_err(|e| fail(format!("移动现有数据目录失败: {e}")))?;
        println!("现有数据已移动到 {}", backup_old.display());
    }
    std::fs::create_dir_all(&config.data_dir)?;
    std::fs::copy(&backup_db, config.data_dir.join("cloudnest.db"))?;
    copy_dir_all(&backup_files, &config.data_dir.join("files"))
        .map_err(|e| fail(format!("恢复用户文件失败: {e}")))?;
    for wal in ["cloudnest.db-wal", "cloudnest.db-shm"] {
        let _ = std::fs::remove_file(config.data_dir.join(wal));
    }
    #[cfg(target_os = "linux")]
    {
        let _ = ProcessCommand::new("chown")
            .args(["-R", "cloudnest:cloudnest"])
            .arg(&config.data_dir)
            .status();
    }
    println!("恢复完成：{}", config.data_dir.display());
    println!("现在可以启动服务：systemctl start cloudnest.service");
    Ok(())
}
