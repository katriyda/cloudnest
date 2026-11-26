//! 构建期静态资源指纹。
//!
//! 前端资源（CSS/JS/字体/图标）通过 `include_str!` / `include_bytes!` 嵌入二进制，
//! URL 原本是固定路径。发布新版本后，某些浏览器、企业代理或 CDN 仍会按旧 URL
//! 返回旧副本，于是页面 HTML 是新的、样式与脚本却是旧的，表现为“部分机器上
//! 界面还是更新前的样子”。
//!
//! 这里在编译时为每个资源计算内容哈希，生成 `/static/v/<version>/…` 形式的地址：
//! 只要资源内容变化，URL 必然变化，旧的长期缓存不会再被任何页面引用。

use std::env;
use std::fs;
use std::path::PathBuf;

/// FNV-1a 64 位哈希。这里只用于区分缓存副本，不需要密码学强度。
fn fingerprint(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // 把长度也混入，避免长度不同的内容落在同一哈希上。
    hash ^= bytes.len() as u64;
    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    format!("{hash:016x}")
}

/// 资源清单：常量前缀、源文件相对路径、URL 中的文件名（字体带 `fonts/` 前缀）。
const ASSETS: &[(&str, &str, &str)] = &[
    ("APP_CSS", "static/app.css", "app.css"),
    ("APP_JS", "static/app.js", "app.js"),
    ("THEME_JS", "static/theme.js", "theme.js"),
    ("HTMX_JS", "static/htmx.min.js", "htmx.min.js"),
    ("ALPINE_JS", "static/alpine.min.js", "alpine.min.js"),
    ("FAVICON", "static/favicon.svg", "favicon.svg"),
    (
        "FONT_UI_400",
        "static/fonts/space-grotesk-latin-400-normal.woff2",
        "fonts/space-grotesk-latin-400-normal.woff2",
    ),
    (
        "FONT_UI_500",
        "static/fonts/space-grotesk-latin-500-normal.woff2",
        "fonts/space-grotesk-latin-500-normal.woff2",
    ),
    (
        "FONT_UI_600",
        "static/fonts/space-grotesk-latin-600-normal.woff2",
        "fonts/space-grotesk-latin-600-normal.woff2",
    ),
    (
        "FONT_UI_700",
        "static/fonts/space-grotesk-latin-700-normal.woff2",
        "fonts/space-grotesk-latin-700-normal.woff2",
    ),
    (
        "FONT_MONO_400",
        "static/fonts/ibm-plex-mono-latin-400-normal.woff2",
        "fonts/ibm-plex-mono-latin-400-normal.woff2",
    ),
    (
        "FONT_MONO_500",
        "static/fonts/ibm-plex-mono-latin-500-normal.woff2",
        "fonts/ibm-plex-mono-latin-500-normal.woff2",
    ),
];

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("缺少 CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("缺少 OUT_DIR"));

    let mut bundle = String::new();
    let mut entries: Vec<(&str, &str, String)> = Vec::with_capacity(ASSETS.len());
    for (name, relative, url_name) in ASSETS {
        // 目录级 rerun-if-changed 依赖 Cargo 的递归监听，这里逐文件声明更可靠。
        println!("cargo:rerun-if-changed={relative}");
        let path = manifest_dir.join(relative);
        let bytes = fs::read(&path)
            .unwrap_or_else(|err| panic!("读取静态资源失败 {}: {err}", path.display()));
        let hash = fingerprint(&bytes);
        bundle.push_str(name);
        bundle.push(':');
        bundle.push_str(&hash);
        bundle.push('\n');
        entries.push((*name, *url_name, hash));
    }

    // 所有资源合起来决定一个版本号：任意资源变化都会让整组 URL 一起变。
    let version = fingerprint(bundle.as_bytes());
    let version = &version[..12];

    let mut generated =
        String::from("// 由 build.rs 生成：静态资源 URL 与 ETag，请勿手工编辑。\n\n");
    generated.push_str(&format!("pub const ASSET_VERSION: &str = \"{version}\";\n"));
    for (name, url_name, hash) in &entries {
        let url = format!("/static/v/{version}/{url_name}");
        generated.push_str(&format!("pub const {name}_URL: &str = \"{url}\";\n"));
        // 用 Debug 格式把 ETag 连同引号写成合法的 Rust 字符串字面量。
        let etag = format!("\"{hash}\"");
        generated.push_str(&format!("pub const {name}_ETAG: &str = {etag:?};\n"));
    }

    let target = out_dir.join("asset_urls.rs");
    fs::write(&target, generated)
        .unwrap_or_else(|err| panic!("写入 {} 失败: {err}", target.display()));
}
