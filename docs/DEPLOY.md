# CloudNest 部署指南

本文档覆盖 CloudNest 在生产环境中的部署、升级、备份恢复与故障排查，版本以代码与 Releases 为准。命令中的技术标识为 `cloudnest`，产品展示名为 CloudNest。

## 1. 运行环境

| 项目 | 要求 |
| --- | --- |
| 操作系统 | Linux（x86_64 或 aarch64），使用 systemd |
| 权限 | 安装与卸载需要 root；日常服务以系统用户 `cloudnest` 运行 |
| 网络 | 默认监听 `0.0.0.0:8080`；建议仅监听 `127.0.0.1:8080` 并由反向代理对外提供服务 |
| 依赖 | 无动态库依赖（发布包为 musl 静态构建）；安装脚本需要 `curl` 或 `wget`、`tar`、`sha256sum` |

安装后的默认位置：

| 路径 | 用途 |
| --- | --- |
| `/opt/cloudnest/cloudnest` | 主程序 |
| `/etc/cloudnest/config.toml` | 配置文件 |
| `/var/lib/cloudnest/` | 数据目录（SQLite 数据库与用户文件） |
| `/etc/systemd/system/cloudnest.service` | systemd 单元，服务名为 `cloudnest` |

## 2. 发布产物

每次发布提供以下文件：

| 文件 | 说明 |
| --- | --- |
| `cloudnest-linux-x86_64.tar.gz` | x86_64 发布包 |
| `cloudnest-linux-aarch64.tar.gz` | aarch64 发布包 |
| `cloudnest-linux-*.tar.gz.sha256` | 每个发布包对应的 SHA256 校验文件 |
| `SHA256SUMS` | 所有发布包的 SHA256 汇总 |
| `install.sh` | 独立安装脚本 |

发布包内包含：`cloudnest` 二进制、`install.sh`、`cloudnest.service`、`config.toml.example`、`README.md`。

## 3. 安装

### 3.1 联网安装

```bash
curl -fsSL https://github.com/katriyda/cloudnest/releases/latest/download/install.sh | sudo bash
```

指定版本：

```bash
curl -fsSL https://github.com/katriyda/cloudnest/releases/latest/download/install.sh | \
  sudo env CLOUDNEST_VERSION='<版本号>' bash
```

`<版本号>` 取 Releases 页面上的版本（与 `Cargo.toml` 的 `version` 一致）。

脚本会自动选择 `cloudnest-linux-x86_64.tar.gz` 或 `cloudnest-linux-aarch64.tar.gz`，并校验 SHA256。

### 3.2 离线安装与手动校验

```bash
# 根据架构选择文件名
curl -LO https://github.com/katriyda/cloudnest/releases/latest/download/cloudnest-linux-x86_64.tar.gz
curl -LO https://github.com/katriyda/cloudnest/releases/latest/download/cloudnest-linux-x86_64.tar.gz.sha256
curl -LO https://github.com/katriyda/cloudnest/releases/latest/download/SHA256SUMS

# 方式一：使用发布包自带的 .sha256
sha256sum -c cloudnest-linux-x86_64.tar.gz.sha256

# 方式二：使用 SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing

tar -xzf cloudnest-linux-x86_64.tar.gz
sudo bash ./install.sh
```

### 3.3 安装脚本行为

- 校验通过后才解压安装，解压过程拒绝包含绝对路径或 `..` 的归档；
- 创建系统用户 `cloudnest`，目录权限：`/opt/cloudnest` 0770、`/etc/cloudnest` 0770（属主 root、属组 cloudnest，供网页设置与一键更新写回）、`/var/lib/cloudnest` 0750；
- `/etc/cloudnest/config.toml` 为 0640、root:cloudnest；已存在时不会被覆盖，升级时配置得以保留；
- 安装并 `systemctl enable --now cloudnest.service`；若目标环境没有运行中的 systemd，脚本仅安装文件并给出提示；
- 支持 `--version`、`--no-start` 参数，以及 `CLOUDNEST_VERSION`、`CLOUDNEST_REPO` 环境变量。

### 3.4 首次初始化

1. 编辑 `/etc/cloudnest/config.toml`：

```toml
listen = "127.0.0.1:8080"
public_url = ""          # HTTPS 场景填写真实地址，例如 https://cloud.example.com
secure_cookies = false    # HTTPS 场景设为 true
```

2. 重启服务：`sudo systemctl restart cloudnest.service`
3. 访问 `public_url`，按页面提示创建管理员账号。首次初始化页面只应在初始化阶段可访问，请尽快完成。

### 3.5 安装后检查

```bash
sudo systemctl status cloudnest.service
sudo journalctl -u cloudnest.service -n 100 --no-pager
curl -I http://127.0.0.1:8080/
```

## 4. 配置

配置文件默认路径为 `/etc/cloudnest/config.toml`。管理员也可以登录后在网页「站点设置」中修改，保存时写回同一文件：`public_url`、`secure_cookies`、`max_upload_size_mb`、`default_quota_gb`、`session_days` 立即生效；`listen` 保存到文件后需重启服务。环境变量 `CLOUDNEST_*` 覆盖的字段以环境变量为准。

网页保存采用同目录临时文件改名，服务用户需要对 `/etc/cloudnest` 有写权限（安装脚本默认授予 0770 root:cloudnest）。修改后手工编辑仍可用，但网页保存会整份重写文件、不保留注释。

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `listen` | `0.0.0.0:8080` | HTTP 监听地址 |
| `data_dir` | `/var/lib/cloudnest` | 数据目录 |
| `public_url` | 空 | 对外访问地址 |
| `secure_cookies` | `false` | 仅 HTTPS 发送 Cookie；public_url 为 https 时自动启用 |
| `max_upload_size_mb` | `2048` | 源站单次请求体上限（MiB）；超过 16 MiB 的文件自动分片上传 |
| `default_quota_gb` | `10` | 新用户默认配额（GiB），0 为不限 |
| `session_days` | `30` | 会话有效期（天） |

环境变量 `CLOUDNEST_LISTEN`、`CLOUDNEST_DATA_DIR`、`CLOUDNEST_PUBLIC_URL` 会覆盖同名配置项，优先级高于配置文件。

**修改 data_dir 的注意事项**：systemd 单元通过 `ProtectSystem=strict` 与 `ReadWritePaths=/var/lib/cloudnest` 限制写路径。若修改数据目录，必须同步修改单元中的 `ReadWritePaths` 与 `WorkingDirectory`，并对新目录设置 `cloudnest` 属主，否则服务无法写入：

```bash
sudo install -d -m 0750 -o cloudnest -g cloudnest /data/cloudnest
# 然后编辑 /etc/systemd/system/cloudnest.service，调整 ReadWritePaths / WorkingDirectory
sudo systemctl daemon-reload
sudo systemctl restart cloudnest.service
```

## 5. 反向代理

建议将 CloudNest 绑定在回环地址，由反向代理终止 TLS。完整示例见 [README.md](../README.md#反向代理与-https)。

### Caddy

```caddyfile
cloud.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

### Nginx

```nginx
server {
    listen 443 ssl http2;
    server_name cloud.example.com;

    ssl_certificate     /etc/nginx/ssl/fullchain.pem;
    ssl_certificate_key /etc/nginx/ssl/privkey.pem;

    client_max_body_size 2g;   # 不小于 max_upload_size_mb

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_request_buffering off;
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
    }
}
```

大文件上传时，代理的请求体限制、超时与磁盘缓冲策略需要与 `max_upload_size_mb` 匹配。

### Cloudflare

Cloudflare 免费版对单个请求体限制 100 MB，该限制在边缘生效，源站的 `max_upload_size_mb` 无法放宽。前端会对超过 16 MiB 的文件自动切换到分片上传通道（每片 4 MiB，支持断点续传），因此大于 100 MB 的文件也会被拆成远低于限制的请求逐个提交。分片接口同样经过登录、CSRF 与配额校验；单文件总大小受用户配额限制。

## 6. 服务管理与日志

```bash
sudo systemctl status cloudnest.service
sudo systemctl restart cloudnest.service
sudo systemctl stop cloudnest.service
sudo journalctl -u cloudnest.service -f
```

日志由 journald 收集。可调整单元中的 `Environment=RUST_LOG=info` 控制级别；排查问题时临时改为 `info` 或更细粒度（例如 `RUST_LOG=debug`）后重启服务。

## 7. 备份与恢复

### 7.1 备份

为保证 SQLite 数据库与文件一致，建议在停服状态下备份：

```bash
sudo systemctl stop cloudnest.service
sudo /opt/cloudnest/cloudnest backup --output /var/backups/cloudnest-$(date +%F-%H%M)
sudo systemctl start cloudnest.service
```

要求：

- 备份目录权限建议 `0700`，属主 root；
- 备份包含数据库与用户文件，可能含敏感内容，应加密后异地保存；
- 定期验证备份可用性，避免只备份不恢复测试；
- 设置保留策略，避免磁盘被历史备份占满。

### 7.2 恢复

恢复会覆盖当前数据，操作前确认目标备份。恢复期间服务必须停止：

```bash
sudo systemctl stop cloudnest.service
sudo /opt/cloudnest/cloudnest restore --from /var/backups/cloudnest-2026-01-01-0300
sudo systemctl start cloudnest.service
sudo journalctl -u cloudnest.service -n 100 --no-pager
```

恢复后建议登录检查文件列表、分享链接与用户配额；如恢复失败，保留现场数据并先再次备份当前状态。

## 8. 升级

升级前务必：

1. 备份当前数据；
2. 阅读目标版本 Release Notes，确认是否有配置或数据迁移说明；
3. 记录当前版本：`/opt/cloudnest/cloudnest version`。

推荐使用安装脚本升级（保留现有配置并重启服务）：

```bash
sudo /opt/cloudnest/cloudnest backup --output /var/backups/cloudnest-$(date +%F-%H%M)
curl -fsSL https://github.com/katriyda/cloudnest/releases/latest/download/install.sh | \
  sudo env CLOUDNEST_VERSION='<版本号>' bash
```

也可以使用 CLI：

```bash
sudo /opt/cloudnest/cloudnest update
```

### 8.1 网页一键更新

通过安装脚本部署的 Linux 实例，管理员可在「站点设置 → 版本更新」中检查并点击「立即更新」：

1. 服务以服务用户身份从 GitHub Releases 读取最新正式版本；
2. 下载对应架构发布包，按 `SHA256SUMS` 或 `.sha256` 校验 SHA256；
3. 使用 SQLite `VACUUM INTO` 把数据库备份到 `/var/lib/cloudnest/backups/pre-update-<版本>-<时间>.db`；
4. 替换 `/opt/cloudnest/cloudnest`（旧文件保留为 `cloudnest.previous`），随后原地重新执行进程；
5. 页面会自动轮询 `/api/version`，确认新版本上线后刷新。

前提与注意：

- 需要服务用户对 `/opt/cloudnest` 与 `/etc/cloudnest` 有写权限，systemd 单元的 `ReadWritePaths` 已包含这两个目录；
- 更新源为 GitHub HTTPS，网络不可达时会返回错误，可继续使用 CLI 或离线包；
- 替换前会先备份数据库，但不会暂停上传；建议在低峰期操作；
- 非 Linux 平台或未通过安装脚本部署的环境只提供版本检查，请使用发布包/CLI 升级。

升级后检查：

```bash
/opt/cloudnest/cloudnest version
sudo systemctl status cloudnest.service
sudo journalctl -u cloudnest.service -n 100 --no-pager
```

如新版本引入配置项，请对照 [packaging/config.toml.example](../packaging/config.toml.example) 增补配置。回滚时使用旧版本安装包重装并恢复升级前备份。

## 9. 卸载

```bash
sudo /opt/cloudnest/cloudnest uninstall           # 保留配置与数据
sudo /opt/cloudnest/cloudnest uninstall --purge   # 删除配置与数据，不可恢复
```

手动卸载步骤见 [README.md](../README.md#卸载)。

## 10. 从源码构建发布包

发布流程使用 musl 静态构建，产物与安装脚本、systemd 单元、配置示例、README 一起打包。

### 10.1 SQLite 静态链接前提

CloudNest 使用 SQLite。musl 静态目标必须使用 bundled 的 SQLite，否则会残留动态依赖或在交叉编译时链接失败。项目通过 sqlx 的 `sqlite` feature 使用 bundled 的 `libsqlite3-sys`；不要切换到 `sqlite-unbundled`。维护者需确保 `Cargo.toml` 中的相关 feature 保持启用（当前由主维护者负责，本文档只做说明）。

### 10.2 本地构建

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --locked --target x86_64-unknown-linux-musl
file target/x86_64-unknown-linux-musl/release/cloudnest   # 应显示 statically linked
```

aarch64：

```bash
cargo install cross --locked
cross build --release --locked --target aarch64-unknown-linux-musl
```

### 10.3 发布流程

发布工作流 [.github/workflows/release.yml](../.github/workflows/release.yml) 在推送 `v*` tag 时触发：

- 校验 tag 与 `Cargo.toml` 版本一致；
- 构建 `x86_64-unknown-linux-musl` 与 `aarch64-unknown-linux-musl` 静态二进制，并检查静态链接；
- 组装发布包、生成逐文件 `.sha256` 与 `SHA256SUMS`；
- 创建 GitHub Release 并上传全部产物。

如果 tag 与版本不一致，工作流会失败；这是刻意设计，避免发布错误版本。

## 11. 故障排查

| 现象 | 可能原因 | 处理 |
| --- | --- | --- |
| `systemctl start` 失败 | 配置错误、端口占用、权限不足 | `journalctl -u cloudnest.service -n 100 --no-pager` 查看具体错误 |
| 端口被占用 | 8080 已被其他服务使用 | `ss -ltnp` 确认后修改 `listen` |
| 启动报权限错误 | `data_dir` 权限或 systemd 写路径不符 | 修正目录属主，或在单元中调整 `ReadWritePaths` |
| 浏览器无法访问 | 仅监听回环、防火墙、代理未启动 | 检查 `listen`、防火墙规则与反向代理配置 |
| 上传大文件返回 413 | 代理请求体限制过小 | 调整 Nginx `client_max_body_size`（Caddy 默认无限制） |
| 反向代理返回 502 | 上游服务未运行或地址错误 | 确认 `systemctl status cloudnest.service` 与 `proxy_pass` 地址 |
| 登录返回 429 | 该用户名 15 分钟内失败次数超过 10 次，或密码校验并发已满 | 等待响应头 `Retry-After` 指定的秒数后重试；在反向代理层补充限流 |
| 分享解锁返回 429 | 同一分享 10 分钟内密码错误超过 5 次 | 等待 `Retry-After` 秒后重试，或让分享者重置密码 |
| 分享页提示“限制了下载次数”且不显示预览 | 该分享设置了下载次数上限，预览被关闭以免绕过计数 | 属预期行为；需要在线预览时取消下载次数上限 |
| HTTPS 下登录反复跳转 | `public_url` 与 `secure_cookies` 配置不一致 | 设置正确的 https `public_url`，开启 `secure_cookies` |
| 下载校验失败 | 下载不完整或镜像文件不一致 | 重新下载并核对 `.sha256` / `SHA256SUMS` |
| 磁盘写入失败 | 数据盘满或配额已满 | 检查 `df -h` 与应用日志；管理员调整配额 |
| musl 构建链接失败 | SQLite 非 bundled 或缺少交叉工具链 | 确认 sqlx `sqlite` bundled feature，使用 `cross` 构建 |
| `update` / `uninstall` 无效 | 非 root 执行或未通过安装脚本安装 | 使用 root 执行；检查服务与安装路径 |
| 网页保存设置提示无法写入 | 服务用户对 `/etc/cloudnest` 无写权限 | 恢复安装脚本权限：`chown root:cloudnest /etc/cloudnest && chmod 0770 /etc/cloudnest` |
| 网页更新提示无法替换程序 | 服务用户对 `/opt/cloudnest` 无写权限，或单元缺少 `ReadWritePaths` | 恢复 `0770 root:cloudnest` 目录权限并检查 systemd 单元 |
| 网页更新下载/校验失败 | 网络不可达、GitHub 限流或发布资产/校验文件缺失 | 查看 `journalctl -u cloudnest.service`，改用 `cloudnest update` 或发布包 |

### 11.1 收集诊断信息

```bash
/opt/cloudnest/cloudnest version
sudo systemctl status cloudnest.service
sudo journalctl -u cloudnest.service --since "1 hour ago" --no-pager
ss -ltnp | grep 8080
df -h /var/lib/cloudnest
```

## 12. 常用命令速查

```bash
# 状态与日志
sudo systemctl status cloudnest.service
sudo journalctl -u cloudnest.service -f

# 备份 / 恢复
sudo /opt/cloudnest/cloudnest backup --output /var/backups/cloudnest-$(date +%F)
sudo /opt/cloudnest/cloudnest restore --from /var/backups/cloudnest-2026-01-01

# 版本 / 更新 / 卸载
/opt/cloudnest/cloudnest version
sudo /opt/cloudnest/cloudnest update
sudo /opt/cloudnest/cloudnest uninstall
```
