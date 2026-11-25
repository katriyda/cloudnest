#!/usr/bin/env bash
#
# CloudNest 一键安装脚本（Linux x86_64 / aarch64）
#
# 联网安装：
#   curl -fsSL https://raw.githubusercontent.com/katriyda/cloudnest/main/scripts/install.sh | sudo bash
#
# 从发布包安装（无需联网）：
#   tar -xzf cloudnest-linux-x86_64.tar.gz
#   sudo bash ./install.sh
#
# 环境变量：
#   CLOUDNEST_VERSION   指定版本号（2.0.0 或 v2.0.0），默认 latest
#   CLOUDNEST_REPO      覆盖 GitHub 仓库，默认 katriyda/cloudnest
#
# 安装位置：
#   /opt/cloudnest/cloudnest                    主程序
#   /etc/cloudnest/config.toml                 配置文件（已存在时保留）
#   /var/lib/cloudnest/                        数据目录
#   /etc/systemd/system/cloudnest.service     systemd 服务
#
# 依赖：bash、curl 或 wget、tar、sha256sum（或 shasum）、systemd。

set -eo pipefail
umask 077

readonly DEFAULT_REPO=katriyda/cloudnest
readonly SERVICE_NAME=cloudnest.service
readonly SERVICE_USER=cloudnest
readonly INSTALL_DIR=/opt/cloudnest
readonly CONFIG_DIR=/etc/cloudnest
readonly DATA_DIR=/var/lib/cloudnest
readonly CONFIG_FILE=/etc/cloudnest/config.toml
readonly UNIT_FILE=/etc/systemd/system/cloudnest.service

REPO="$DEFAULT_REPO"
if [ -n "${CLOUDNEST_REPO:-}" ]; then
    REPO="$CLOUDNEST_REPO"
fi
VERSION=latest
if [ -n "${CLOUDNEST_VERSION:-}" ]; then
    VERSION="$CLOUDNEST_VERSION"
fi
START_SERVICE=1
ARCH=""
TEMP_DIR=""
SOURCE_DIR=""
INSTALL_MODE=""

log() {
    printf '[cloudnest] %s\n' "$*"
}

warn() {
    printf '[cloudnest] 警告：%s\n' "$*" >&2
}

die() {
    printf '[cloudnest] 错误：%s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'USAGE'
CloudNest 安装脚本

用法：sudo bash install.sh [选项]

选项：
  --version <版本>   指定版本，如 2.0.0（默认 latest）
  --no-start         只安装并 enable 服务，不立即启动
  -h, --help         显示本帮助

环境变量：CLOUDNEST_VERSION、CLOUDNEST_REPO
USAGE
}

cleanup() {
    if [ -n "$TEMP_DIR" ] && [ -d "$TEMP_DIR" ]; then
        rm -rf "$TEMP_DIR"
    fi
}

trap cleanup EXIT

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "缺少必需命令：$1"
}

parse_args() {
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --version)
                [ "$#" -ge 2 ] || die "--version 需要参数"
                VERSION="$2"
                shift 2
                ;;
            --no-start)
                START_SERVICE=0
                shift
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                die "未知参数：$1（使用 --help 查看用法）"
                ;;
        esac
    done
}

ensure_root() {
    if [ "$EUID" -eq 0 ]; then
        return 0
    fi
    if command -v sudo >/dev/null 2>&1 && [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
        log "需要 root 权限，使用 sudo 重新执行本脚本……"
        exec sudo -E bash "${BASH_SOURCE[0]}" "$@"
    fi
    die "请以 root 身份运行：curl -fsSL <脚本地址> | sudo bash"
}

detect_arch() {
    local machine
    machine="$(uname -m)"
    case "$machine" in
        x86_64|amd64) ARCH=x86_64 ;;
        aarch64|arm64) ARCH=aarch64 ;;
        *) die "不支持的 CPU 架构：$machine（仅支持 x86_64 / aarch64）" ;;
    esac
    log "检测到架构：$ARCH"
}

normalize_version() {
    if [ "$VERSION" = latest ] || [ "$VERSION" = LATEST ]; then
        VERSION=latest
        return 0
    fi
    case "$VERSION" in
        v*) VERSION="$(printf '%s' "$VERSION" | cut -c2-)" ;;
    esac
    if ! printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
        die "版本号格式不正确：$VERSION（应形如 2.0.0）"
    fi
}

release_base_url() {
    if [ "$VERSION" = latest ]; then
        printf 'https://github.com/%s/releases/latest/download' "$REPO"
    else
        printf 'https://github.com/%s/releases/download/v%s' "$REPO" "$VERSION"
    fi
}

download() {
    local url="$1"
    local dest="$2"
    if command -v curl >/dev/null 2>&1; then
        curl --fail --silent --show-error --location --retry 3 --retry-delay 2 --connect-timeout 10 --output "$dest" "$url"
    elif command -v wget >/dev/null 2>&1; then
        wget --quiet --tries=3 --timeout=30 --output-document="$dest" "$url"
    else
        die "需要 curl 或 wget 下载发布包"
    fi
}

sha256_of() {
    local file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | awk '{print $1}'
    else
        die "缺少 sha256sum（或 shasum），无法校验下载文件"
    fi
}

file_sha256_expected() {
    awk '{print $1; exit}' "$1"
}

sums_sha256_expected() {
    awk -v asset="$2" '$2 == asset || $2 == "*" asset {print $1; exit}' "$1"
}

use_local_package() {
    local candidate
    if [ -z "${BASH_SOURCE[0]:-}" ] || [ ! -f "${BASH_SOURCE[0]}" ]; then
        return 1
    fi
    candidate="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    if [ -x "$candidate/cloudnest" ] && [ -f "$candidate/cloudnest.service" ] && [ -f "$candidate/config.toml.example" ]; then
        SOURCE_DIR="$candidate"
        INSTALL_MODE=local
        log "检测到发布包目录：$SOURCE_DIR，使用本地文件安装"
        return 0
    fi
    return 1
}

extract_release() {
    local tarball="$1"
    local listing
    listing="$(tar -tzf "$tarball")"
    if printf '%s\n' "$listing" | grep -Eq '(^/|(^|/)\.\.(/|$))'; then
        die "发布包包含不安全路径，拒绝安装"
    fi
    case "$listing" in
        *cloudnest*) ;;
        *) die "发布包中未找到 cloudnest 主程序" ;;
    esac
    case "$listing" in
        *cloudnest.service*) ;;
        *) die "发布包缺少 cloudnest.service" ;;
    esac
    case "$listing" in
        *config.toml.example*) ;;
        *) die "发布包缺少 config.toml.example" ;;
    esac
    mkdir -p "$TEMP_DIR/package"
    tar -xzf "$tarball" -C "$TEMP_DIR/package"
    SOURCE_DIR="$TEMP_DIR/package"
}

download_release() {
    local asset
    local base
    local tarball
    local expected
    local actual
    asset="cloudnest-linux-$ARCH.tar.gz"
    base="$(release_base_url)"
    tarball="$TEMP_DIR/$asset"
    log "下载 $base/$asset"
    download "$base/$asset" "$tarball"
    expected=""
    if download "$base/$asset.sha256" "$tarball.sha256" 2>/dev/null; then
        expected="$(file_sha256_expected "$tarball.sha256")"
    elif download "$base/SHA256SUMS" "$TEMP_DIR/SHA256SUMS" 2>/dev/null; then
        expected="$(sums_sha256_expected "$TEMP_DIR/SHA256SUMS" "$asset")"
    fi
    [ -n "$expected" ] || die "无法获取 $asset 的 SHA256 校验值，拒绝安装"
    actual="$(sha256_of "$tarball")"
    if [ "$actual" != "$expected" ]; then
        die "SHA256 校验失败（期望 $expected，实际 $actual）"
    fi
    log "SHA256 校验通过"
    extract_release "$tarball"
}

resolve_source() {
    if use_local_package; then
        return 0
    fi
    need_cmd tar
    TEMP_DIR="$(mktemp -d)"
    INSTALL_MODE=download
    download_release
}

ensure_user() {
    local nologin
    if ! getent group "$SERVICE_USER" >/dev/null 2>&1; then
        groupadd --system "$SERVICE_USER"
    fi
    if ! id -u "$SERVICE_USER" >/dev/null 2>&1; then
        if [ -x /usr/sbin/nologin ]; then
            nologin=/usr/sbin/nologin
        else
            nologin=/bin/false
        fi
        useradd --system --gid "$SERVICE_USER" --home-dir "$DATA_DIR" --no-create-home --shell "$nologin" --comment "CloudNest service user" "$SERVICE_USER"
        log "已创建系统用户 $SERVICE_USER"
    fi
}

install_layout() {
    install -d -m 0755 -o root -g root "$INSTALL_DIR"
    install -d -m 0750 -o root -g "$SERVICE_USER" "$CONFIG_DIR"
    install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$DATA_DIR"
}

install_binary() {
    install -m 0755 -o root -g root "$SOURCE_DIR/cloudnest" "$INSTALL_DIR/cloudnest"
    log "主程序已安装：$INSTALL_DIR/cloudnest"
}

install_config() {
    if [ -f "$CONFIG_FILE" ]; then
        log "配置文件已存在，保留现有配置：$CONFIG_FILE"
        return 0
    fi
    install -m 0640 -o root -g "$SERVICE_USER" "$SOURCE_DIR/config.toml.example" "$CONFIG_FILE"
    log "配置文件已创建：$CONFIG_FILE（请按需修改 public_url 后重启服务）"
}

systemd_available() {
    command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]
}

install_unit() {
    install -m 0644 -o root -g root "$SOURCE_DIR/cloudnest.service" "$UNIT_FILE"
    log "systemd 服务文件已安装：$UNIT_FILE"
}

start_service() {
    if ! systemd_available; then
        warn "未检测到运行中的 systemd，已跳过服务启动"
        warn "可手动运行：$INSTALL_DIR/cloudnest serve --config $CONFIG_FILE"
        return 0
    fi
    systemctl daemon-reload
    if [ "$START_SERVICE" -eq 0 ]; then
        systemctl enable "$SERVICE_NAME"
        log "已 enable $SERVICE_NAME（--no-start，未立即启动）"
        return 0
    fi
    if systemctl is-active --quiet "$SERVICE_NAME"; then
        systemctl restart "$SERVICE_NAME"
    else
        systemctl enable --now "$SERVICE_NAME"
    fi
    if systemctl is-active --quiet "$SERVICE_NAME"; then
        log "服务已启动（$SERVICE_NAME）"
    else
        warn "服务未处于 active 状态，最近日志："
        journalctl -u "$SERVICE_NAME" -n 40 --no-pager || true
        die "服务启动失败，请根据日志排查（配置：$CONFIG_FILE）"
    fi
}

print_summary() {
    local listen_hint
    listen_hint="$(awk -F'"' '/^listen[[:space:]]*=/ {print $2; exit}' "$CONFIG_FILE" 2>/dev/null || true)"
    if [ -z "$listen_hint" ]; then
        listen_hint=0.0.0.0:8080
    fi
    printf '\n'
    log "安装完成（模式：$INSTALL_MODE，版本：$VERSION，架构：$ARCH）"
    log "配置文件：$CONFIG_FILE"
    log "数据目录：$DATA_DIR"
    log "监听地址：$listen_hint"
    log "如对外提供服务，请编辑 config.toml 中的 public_url 与 secure_cookies 后执行：systemctl restart $SERVICE_NAME"
    log "查看状态：systemctl status $SERVICE_NAME"
    log "查看日志：journalctl -u $SERVICE_NAME -f"
}

main() {
    parse_args "$@"
    ensure_root "$@"
    normalize_version
    [ "$(uname -s)" = Linux ] || die "本脚本仅支持 Linux"
    need_cmd install
    detect_arch
    resolve_source
    ensure_user
    install_layout
    install_binary
    install_config
    install_unit
    start_service
    print_summary
}

main "$@"
