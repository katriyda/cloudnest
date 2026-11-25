use crate::cli::VERSION;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::sqlite::SqlitePoolOptions;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// 默认从 GitHub Releases 读取正式版本。
pub const DEFAULT_API_BASE: &str = "https://api.github.com";
pub const REPO: &str = "katriyda/cloudnest";
const USER_AGENT: &str = "cloudnest-updater";
/// 发布包大小上限，防止更新源返回超大响应拖垮内存。
const MAX_ASSET_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CHECKSUM_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
pub struct UpdateError(pub String);

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for UpdateError {}

pub type UpdateResult<T> = Result<T, UpdateError>;

fn fail(message: impl Into<String>) -> UpdateError {
    UpdateError(message.into())
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateStatus {
    pub current: String,
    pub latest: String,
    pub has_update: bool,
}

#[derive(Debug, Clone)]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<ReleaseAsset>,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&ReleaseAsset> {
        self.assets.iter().find(|asset| asset.name == name)
    }

    fn checksum_asset(&self, asset_name: &str) -> Option<&ReleaseAsset> {
        self.asset("SHA256SUMS")
            .or_else(|| self.asset(&format!("{asset_name}.sha256")))
    }
}

#[derive(Debug, Clone)]
pub struct StagedUpdate {
    pub version: String,
    pub path: PathBuf,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    assets: Vec<ApiAsset>,
}

/// 在线更新服务：保存更新源地址，并用互斥锁避免并发安装。
#[derive(Clone)]
pub struct UpdateService {
    inner: Arc<Inner>,
}

struct Inner {
    api_base: RwLock<String>,
    busy: AtomicBool,
}

impl UpdateService {
    pub fn new() -> Self {
        let api_base = std::env::var("CLOUDNEST_UPDATE_API")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_API_BASE.to_string());
        Self {
            inner: Arc::new(Inner {
                api_base: RwLock::new(api_base),
                busy: AtomicBool::new(false),
            }),
        }
    }

    pub fn api_base(&self) -> String {
        self.inner
            .api_base
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn set_api_base(&self, base: impl Into<String>) {
        let base = base.into().trim().trim_end_matches('/').to_string();
        if base.is_empty() {
            return;
        }
        let mut guard = self
            .inner
            .api_base
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = base;
    }

    /// 占用更新锁：同一时间只允许一个安装任务。
    pub fn begin(&self) -> bool {
        !self.inner.busy.swap(true, Ordering::SeqCst)
    }

    pub fn finish(&self) {
        self.inner.busy.store(false, Ordering::SeqCst);
    }

    pub fn latest(&self) -> UpdateResult<Release> {
        fetch_latest(&self.api_base())
    }

    pub fn check(&self) -> UpdateResult<UpdateStatus> {
        let release = self.latest()?;
        Ok(status_for(&release, VERSION))
    }

    /// 检查、下载、校验并解压新版本到 work_dir（该目录应已存在且为空）。
    pub fn stage_latest(&self, work_dir: &Path) -> UpdateResult<StagedUpdate> {
        let release = self.latest()?;
        let api_base = self.api_base();
        stage_release(&release, work_dir, &api_base)
    }
}

impl Default for UpdateService {
    fn default() -> Self {
        Self::new()
    }
}

pub fn status_for(release: &Release, current: &str) -> UpdateStatus {
    UpdateStatus {
        current: current.to_string(),
        latest: release.tag_name.clone(),
        has_update: is_newer(&release.tag_name, current),
    }
}

fn http_get(url: &str, accept: &str) -> UpdateResult<ureq::Body> {
    Ok(ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", accept)
        .call()
        .map_err(|e| fail(format!("请求 {url} 失败: {e}")))?
        .into_body())
}

fn fetch_latest(api_base: &str) -> UpdateResult<Release> {
    let url = format!(
        "{}/repos/{REPO}/releases/latest",
        api_base.trim_end_matches('/')
    );
    let api: ApiRelease = http_get(&url, "application/vnd.github+json")?
        .read_json()
        .map_err(|e| fail(format!("解析版本信息失败: {e}")))?;
    Ok(Release {
        tag_name: api.tag_name,
        assets: api
            .assets
            .into_iter()
            .map(|asset| ReleaseAsset {
                name: asset.name,
                url: asset.browser_download_url,
            })
            .collect(),
    })
}

pub fn target_arch() -> UpdateResult<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64"),
        "aarch64" => Ok("aarch64"),
        other => Err(fail(format!("不支持的 CPU 架构: {other}"))),
    }
}

/// 网页一键更新目前只在 Linux（静态 musl 发布包）上自动安装并原地重启。
pub fn web_update_supported() -> bool {
    cfg!(target_os = "linux")
}

pub fn release_asset_name() -> UpdateResult<String> {
    Ok(format!("cloudnest-linux-{}.tar.gz", target_arch()?))
}

pub fn version_triplet(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split(['.', '-'])
        .take(3)
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

pub fn is_newer(latest: &str, current: &str) -> bool {
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

/// 拆出 URL 的 (scheme, host)，只用于下载地址白名单校验。
fn split_url(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let host = if let Some(rest) = authority.strip_prefix('[') {
        let (addr, _) = rest.split_once(']')?;
        format!("[{}]", addr.to_ascii_lowercase())
    } else {
        authority
            .split(':')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    };
    if host.is_empty() {
        return None;
    }
    Some((scheme.to_ascii_lowercase(), host))
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost" || host.starts_with("127.") || host == "[::1]"
}

/// 更新下载地址必须来自配置的更新源或官方 GitHub 资产域名（HTTPS）。
/// 允许 http 仅用于本机回环地址（测试/离线镜像）。
pub fn is_allowed_download_url(url: &str, api_base: &str) -> bool {
    let Some((scheme, host)) = split_url(url) else {
        return false;
    };
    let Some((api_scheme, api_host)) = split_url(api_base) else {
        return false;
    };
    if host == api_host {
        return scheme == "https"
            || (scheme == "http" && api_scheme == "http" && is_loopback_host(&host));
    }
    (api_host == "api.github.com" || api_host == "github.com")
        && scheme == "https"
        && (host == "github.com" || host.ends_with(".githubusercontent.com"))
}

/// 从 SHA256SUMS 或 asset.sha256 中取出对应文件的校验值。
pub fn checksum_for(text: &str, asset_name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let trimmed = line.trim();
        let mut parts = trimmed.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == asset_name).then(|| hash.to_ascii_lowercase())
    })
}

/// 下载发布包、校验 SHA256 并解出 cloudnest 可执行文件。
pub fn stage_release(
    release: &Release,
    work_dir: &Path,
    api_base: &str,
) -> UpdateResult<StagedUpdate> {
    let asset_name = release_asset_name()?;
    let asset_url = release
        .asset(&asset_name)
        .ok_or_else(|| fail(format!("最新版本中没有找到 {asset_name}")))?
        .url
        .clone();
    let checksum_url = release
        .checksum_asset(&asset_name)
        .ok_or_else(|| fail("发布中缺少 SHA256 校验文件"))?
        .url
        .clone();
    // 只允许从配置的更新源或官方 GitHub 资产域下载，避免被篡改的 API 响应把下载引到任意主机。
    if !is_allowed_download_url(&asset_url, api_base) {
        return Err(fail(format!(
            "下载地址不在允许的更新源内，拒绝安装：{asset_url}"
        )));
    }
    if !is_allowed_download_url(&checksum_url, api_base) {
        return Err(fail(format!(
            "校验文件地址不在允许的更新源内，拒绝安装：{checksum_url}"
        )));
    }
    let checksum_text = http_get(&checksum_url, "text/plain")?
        .into_with_config()
        .limit(MAX_CHECKSUM_BYTES)
        .read_to_string()
        .map_err(|e| fail(format!("读取校验文件失败: {e}")))?;
    let expected = checksum_for(&checksum_text, &asset_name)
        .ok_or_else(|| fail(format!("校验文件中没有 {asset_name}")))?;
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(fail("校验文件中的 SHA256 格式不正确，拒绝安装"));
    }

    let bytes = http_get(&asset_url, "application/octet-stream")?
        .into_with_config()
        .limit(MAX_ASSET_BYTES)
        .read_to_vec()
        .map_err(|e| fail(format!("下载新版本失败: {e}")))?;
    let actual = hex::encode(Sha256::digest(&bytes));
    if actual != expected {
        return Err(fail(format!(
            "SHA256 校验失败：期望 {expected}，实际 {actual}"
        )));
    }

    std::fs::create_dir_all(work_dir).map_err(|e| fail(format!("创建更新目录失败: {e}")))?;
    let archive_path = work_dir.join(&asset_name);
    std::fs::write(&archive_path, &bytes).map_err(|e| fail(format!("写入安装包失败: {e}")))?;
    let binary_path = work_dir.join("cloudnest");
    let file =
        std::fs::File::open(&archive_path).map_err(|e| fail(format!("打开安装包失败: {e}")))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut found = false;
    for entry in archive
        .entries()
        .map_err(|e| fail(format!("读取安装包失败: {e}")))?
    {
        let mut entry = entry.map_err(|e| fail(format!("读取安装包条目失败: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| fail(format!("读取安装包路径失败: {e}")))?;
        if path.file_name().and_then(|name| name.to_str()) == Some("cloudnest")
            && entry.header().entry_type().is_file()
        {
            entry
                .unpack(&binary_path)
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
        std::fs::set_permissions(&binary_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| fail(format!("设置执行权限失败: {e}")))?;
    }
    Ok(StagedUpdate {
        version: release.tag_name.clone(),
        path: binary_path,
    })
}

/// 替换前把数据库快照到数据目录下的 backups，返回备份文件路径。
pub async fn backup_database(data_dir: &Path) -> UpdateResult<Option<PathBuf>> {
    let db_path = data_dir.join("cloudnest.db");
    if !db_path.exists() {
        return Ok(None);
    }
    let backup_dir = data_dir.join("backups");
    tokio::fs::create_dir_all(&backup_dir)
        .await
        .map_err(|e| fail(format!("创建备份目录失败: {e}")))?;
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
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&db_path))
        .await
        .map_err(|e| fail(format!("连接数据库失败: {e}")))?;
    let result = sqlx::query(sqlx::AssertSqlSafe(format!("VACUUM INTO '{escaped}'")))
        .execute(&pool)
        .await;
    pool.close().await;
    result.map_err(|e| fail(format!("更新前数据库备份失败: {e}")))?;
    Ok(Some(backup_path))
}

/// 用下载好的新程序替换当前可执行文件，返回旧程序备份路径。
pub fn replace_exe(staged: &Path) -> UpdateResult<PathBuf> {
    if !cfg!(unix) {
        return Err(fail("当前平台不支持自动安装，请使用发布包或命令行更新"));
    }
    let current_exe =
        std::env::current_exe().map_err(|e| fail(format!("无法定位当前程序: {e}")))?;
    let previous = current_exe.with_extension("previous");
    std::fs::copy(&current_exe, &previous).map_err(|e| fail(format!("备份当前程序失败: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&previous, std::fs::Permissions::from_mode(0o755));
    }
    let file_name = current_exe
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cloudnest");
    let incoming = current_exe.with_file_name(format!(".{file_name}.new-{}", std::process::id()));
    let copied = std::fs::copy(staged, &incoming).map_err(|e| {
        fail(format!(
            "无法替换程序文件 {}: {e}（请确认服务用户对该目录有写权限）",
            current_exe.display()
        ))
    });
    if let Err(err) = copied {
        let _ = std::fs::remove_file(&incoming);
        return Err(err);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&incoming, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| fail(format!("设置执行权限失败: {e}")))?;
    }
    if let Err(err) = std::fs::rename(&incoming, &current_exe) {
        let _ = std::fs::remove_file(&incoming);
        return Err(fail(format!("替换程序失败，已保留旧版本: {err}")));
    }
    Ok(previous)
}

/// 把旧程序复制回去，用于 CLI 重启失败后的回滚。
pub fn restore_previous(previous: &Path) -> UpdateResult<()> {
    let current_exe =
        std::env::current_exe().map_err(|e| fail(format!("无法定位当前程序: {e}")))?;
    std::fs::copy(previous, &current_exe)
        .map_err(|e| fail(format!("回滚失败，请手动恢复 {}: {e}", previous.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&current_exe, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

/// 延迟后用同一个可执行文件原地重新执行进程，保留原有命令行参数与环境变量。
/// 网页更新先返回响应，再执行这一步；新进程会重新监听端口。
pub fn schedule_restart(delay: Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        let err = exec_self();
        tracing::error!("重新启动服务失败: {err}");
        std::process::exit(1);
    });
}

#[cfg(unix)]
fn exec_self() -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => return err,
    };
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let mut command = std::process::Command::new(exe);
    command.args(args);
    command.exec()
}

#[cfg(not(unix))]
fn exec_self() -> std::io::Error {
    std::io::Error::other("当前平台不支持原地重启")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering() {
        assert!(is_newer("v2.3.1", "2.3.0"));
        assert!(is_newer("v2.10.0", "2.9.9"));
        assert!(!is_newer("v2.3.0", "2.3.0"));
        assert!(!is_newer("v2.2.9", "2.3.0"));
        assert!(!is_newer("v3.0.0-beta", "3.0.0"));
    }

    #[test]
    fn download_urls_are_restricted() {
        // 默认更新源：只接受 GitHub 资产域名。
        assert!(is_allowed_download_url(
            "https://github.com/katriyda/cloudnest/releases/download/v1/x.tar.gz",
            DEFAULT_API_BASE
        ));
        assert!(is_allowed_download_url(
            "https://release-assets.githubusercontent.com/x.tar.gz",
            DEFAULT_API_BASE
        ));
        assert!(!is_allowed_download_url(
            "https://evil.example/x.tar.gz",
            DEFAULT_API_BASE
        ));
        assert!(!is_allowed_download_url(
            "http://github.com/x.tar.gz",
            DEFAULT_API_BASE
        ));
        assert!(!is_allowed_download_url(
            "https://github.com@evil.example/x.tar.gz",
            DEFAULT_API_BASE
        ));
        // 自建更新源：只接受同一个主机，且 http 仅限本机回环。
        let base = "http://127.0.0.1:9";
        assert!(is_allowed_download_url("http://127.0.0.1:9/asset", base));
        assert!(!is_allowed_download_url("http://10.0.0.5/asset", base));
        assert!(!is_allowed_download_url("https://evil.example/asset", base));
    }

    #[test]
    fn checksum_lines() {
        let text =
            "abc123  cloudnest-linux-x86_64.tar.gz\ndef456 *cloudnest-linux-aarch64.tar.gz\n";
        assert_eq!(
            checksum_for(text, "cloudnest-linux-x86_64.tar.gz").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            checksum_for(text, "cloudnest-linux-aarch64.tar.gz").as_deref(),
            Some("def456")
        );
        assert_eq!(checksum_for(text, "missing.tar.gz"), None);
    }
}
