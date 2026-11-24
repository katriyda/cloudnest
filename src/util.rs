use crate::error::AppError;
use chrono::{Local, NaiveDateTime, TimeZone};
use mime_guess::MimeGuess;
use std::collections::HashSet;
use std::path::PathBuf;

pub const KIB: i64 = 1024;
pub const MIB: i64 = 1024 * 1024;
pub const GIB: i64 = 1024 * 1024 * 1024;

pub fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn human_size(bytes: i64) -> String {
    let bytes = bytes.max(0);
    if bytes < KIB {
        return format!("{bytes} B");
    }
    let units = ["KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = -1i32;
    while value >= 1024.0 && unit < 3 {
        value /= 1024.0;
        unit += 1;
    }
    let label = units[unit.max(0) as usize];
    if value >= 100.0 {
        format!("{value:.0} {label}")
    } else if value >= 10.0 {
        format!("{value:.1} {label}")
    } else {
        format!("{value:.2} {label}")
    }
}

pub fn format_ts(ts: i64) -> String {
    match Local.timestamp_opt(ts, 0).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        None => "-".to_string(),
    }
}

pub fn format_date(ts: i64) -> String {
    match Local.timestamp_opt(ts, 0).single() {
        Some(dt) => dt.format("%Y-%m-%d").to_string(),
        None => "-".to_string(),
    }
}

pub fn format_datetime_local(ts: i64) -> String {
    match Local.timestamp_opt(ts, 0).single() {
        Some(dt) => dt.format("%Y-%m-%dT%H:%M").to_string(),
        None => String::new(),
    }
}

/// Parse a value from an <input type="datetime-local"> in the server's local timezone.
pub fn parse_local_datetime(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M").ok()?;
    let local = Local.from_local_datetime(&naive).single()?;
    Some(local.timestamp())
}

pub fn sanitize_filename(raw: &str) -> Result<String, AppError> {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .trim()
        .to_string();
    if base.is_empty() {
        return Err(AppError::BadRequest("文件名不能为空".to_string()));
    }
    if base == "." || base == ".." {
        return Err(AppError::BadRequest("文件名不合法".to_string()));
    }
    let mut out = String::with_capacity(base.len());
    for ch in base.chars() {
        if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    let out = out.trim_end_matches(['.', ' ']).trim().to_string();
    if out.is_empty() {
        return Err(AppError::BadRequest("文件名不合法".to_string()));
    }
    if out.chars().count() > 200 {
        return Err(AppError::BadRequest(
            "文件名过长（最多 200 个字符）".to_string(),
        ));
    }
    Ok(out)
}

/// Split a browser supplied relative folder path into safe components.
pub fn sanitize_relative_components(raw: &str) -> Result<Vec<String>, AppError> {
    let mut parts = Vec::new();
    for part in raw.split(['/', '\\']) {
        let part = part.trim();
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err(AppError::BadRequest("路径不合法".to_string()));
        }
        if parts.len() >= 32 {
            return Err(AppError::BadRequest("文件夹层级过深".to_string()));
        }
        parts.push(sanitize_filename(part)?);
    }
    Ok(parts)
}

pub fn unique_name(desired: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(desired) {
        return desired.to_string();
    }
    let (stem, ext) = match desired.rfind('.') {
        Some(pos) if pos > 0 => (&desired[..pos], &desired[pos..]),
        _ => (desired, ""),
    };
    for n in 1..10_000 {
        let candidate = format!("{stem} ({n}){ext}");
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    format!("{stem} (copy){ext}")
}

pub fn preview_kind(name: &str, mime: Option<&str>) -> &'static str {
    let ext = PathBuf::from(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = mime.unwrap_or("").to_ascii_lowercase();
    if mime.starts_with("image/") {
        if mime == "image/svg+xml" || mime == "image/svg" {
            return "none";
        }
        return "image";
    }
    if mime.starts_with("video/") {
        return "video";
    }
    if mime.starts_with("audio/") {
        return "audio";
    }
    if mime == "application/pdf" || ext == "pdf" {
        return "pdf";
    }
    if mime.starts_with("text/")
        || matches!(
            ext.as_str(),
            "txt"
                | "md"
                | "markdown"
                | "log"
                | "csv"
                | "json"
                | "xml"
                | "yaml"
                | "yml"
                | "toml"
                | "rs"
                | "js"
                | "ts"
                | "css"
                | "sql"
                | "sh"
                | "py"
                | "go"
                | "java"
                | "c"
                | "h"
                | "cpp"
                | "hpp"
                | "ini"
                | "conf"
                | "env"
                | "diff"
                | "patch"
        )
    {
        return "text";
    }
    "none"
}

pub fn guess_mime(name: &str) -> String {
    MimeGuess::from_path(name)
        .first_or_octet_stream()
        .essence_str()
        .to_string()
}

pub fn extension_label(name: &str) -> String {
    let ext = PathBuf::from(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_uppercase();
    if ext.is_empty() | (ext.len() > 3) {
        "FILE".to_string()
    } else {
        ext
    }
}

pub fn hue_for(name: &str) -> u32 {
    // Stable, muted color selection per extension.
    let mut hash: u32 = 0;
    for b in name.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(b as u32);
    }
    hash % 360
}

pub fn content_disposition(kind: &str, name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if c.is_ascii() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded = urlencoding::encode(name);
    format!(
        "{kind}; filename=\"{fallback}\"; filename*=UTF-8''{}",
        encoded.replace('+', "%20")
    )
}

/// Parse a single HTTP byte range. Returns inclusive (start,end) clamped to the resource.
pub fn parse_range(header: &str, size: u64) -> Option<(u64, u64)> {
    let rest = header.trim().strip_prefix("bytes=")?;
    let range = rest.split(',').next()?.trim();
    let (start_s, end_s) = range.split_once('-')?;
    if size == 0 {
        return None;
    }
    if start_s.is_empty() {
        let suffix: u64 = end_s.trim().parse().ok()?;
        if suffix == 0 {
            return None;
        }
        let start = size.saturating_sub(suffix);
        return Some((start, size - 1));
    }
    let start: u64 = start_s.trim().parse().ok()?;
    if start >= size {
        return None;
    }
    let end = if end_s.trim().is_empty() {
        size - 1
    } else {
        end_s.trim().parse::<u64>().ok()?.min(size - 1)
    };
    if end < start {
        return None;
    }
    Some((start, end))
}

pub fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(value) if value.starts_with('/') && !value.starts_with("//") => value.to_string(),
        _ => "/files".to_string(),
    }
}

pub fn percent_decode_query(value: &str) -> String {
    urlencoding::decode(value)
        .map(|v| v.into_owned())
        .unwrap_or_else(|_| value.to_string())
}

pub fn username_valid(name: &str) -> Result<(), AppError> {
    let name = name.trim();
    if !(3..=32).contains(&name.chars().count()) {
        return Err(AppError::BadRequest(
            "用户名长度需为 3-32 个字符".to_string(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(AppError::BadRequest(
            "用户名只能包含字母、数字、下划线、点和短横线".to_string(),
        ));
    }
    Ok(())
}

pub fn password_valid(password: &str) -> Result<(), AppError> {
    let len = password.chars().count();
    if len < 8 {
        return Err(AppError::BadRequest("密码至少需要 8 个字符".to_string()));
    }
    if len > 128 {
        return Err(AppError::BadRequest("密码不能超过 128 个字符".to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_safety() {
        assert_eq!(sanitize_filename("a/b.txt").unwrap(), "b.txt");
        assert!(sanitize_filename("..").is_err());
        assert_eq!(sanitize_filename("..\\evil.txt").unwrap(), "evil.txt");
        assert_eq!(sanitize_filename("a:b?.txt").unwrap(), "a_b_.txt");
    }

    #[test]
    fn relative_paths() {
        assert_eq!(
            sanitize_relative_components("photos/2024").unwrap(),
            vec!["photos", "2024"]
        );
        assert!(sanitize_relative_components("../etc").is_err());
    }

    #[test]
    fn unique_names() {
        let mut taken = HashSet::new();
        taken.insert("report.pdf".to_string());
        taken.insert("report (1).pdf".to_string());
        assert_eq!(unique_name("report.pdf", &taken), "report (2).pdf");
        assert_eq!(unique_name("fresh.txt", &taken), "fresh.txt");
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-9", 100), Some((0, 9)));
        assert_eq!(parse_range("bytes=90-", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=5-3", 100), None);
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(1024), "1.00 KB");
        assert_eq!(human_size(1536), "1.50 KB");
        assert_eq!(human_size(0), "0 B");
    }
}
