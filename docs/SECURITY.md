# CloudNest 安全说明

本文档描述 CloudNest 的安全设计、部署加固建议与漏洞报告方式，具体行为以代码与当前发布版本为准。产品展示名为 CloudNest，命令与路径中的技术标识为 `cloudnest`。

## 1. 安全模型与边界

CloudNest 是自托管的个人云盘，默认假设：

- 服务运行在受信任的 Linux 主机上，由 root 完成安装，之后以专用系统用户 `cloudnest` 运行；
- TLS 由同机或前置反向代理终止，CloudNest 本身不直接提供 HTTPS；
- 数据目录、配置文件与备份目录仅对 root 及服务用户开放；
- 管理员对主机与用户账号负最终责任，包括及时升级、备份与访问控制。

CloudNest 不试图防御拥有主机 root 权限的攻击者，也不提供多租户硬隔离或静态数据加密。

## 2. 身份认证与会话

- 首次启动通过初始化页面创建管理员账号；初始化完成后应尽快通过反向代理限制或关闭该页面（按部署环境实际行为处理），避免被他人抢先初始化。
- 密码使用 Argon2 哈希存储，不保存明文密码。
- 登录成功后签发会话 Cookie；启用 `secure_cookies` 后仅通过 HTTPS 发送。`public_url` 为 https 时会自动启用该选项。
- 会话具有有效期（`session_days`，默认 30 天，范围 1-365），过期后需重新登录。
- 修改类表单使用 CSRF token 校验，降低跨站请求伪造风险。
- 登录与分享提取密码都有失败次数限制：同一用户名 15 分钟内连续失败 10 次后锁定 15 分钟，分享密码连续失败 5 次后锁定 10 分钟；超限返回 429 与 `Retry-After`。
- 同时进行的 Argon2 校验数量有上限（约等于 CPU 核数，最多 8 个）；繁忙时请求最多等待 2 秒，之后返回 429，避免未认证请求耗尽 CPU。
- 不存在的用户名同样执行一次等价 Argon2 校验，登录失败文案统一，降低用户名枚举与时序侧信道。
- 分享提取密码至少 6 个字符、最多 128 个字符；会话 Cookie 同名多值会被拒绝（防 Cookie tossing）。
- 管理员与普通用户权限分离；管理员可管理用户、配额与系统统计。

建议：

- 为管理员设置强密码，避免使用弱口令；
- 对外部署必须启用 HTTPS 并设置 `secure_cookies = true`；
- 不将服务直接暴露在公网明文 HTTP；
- 定期检查用户列表，禁用或删除不再使用的账号。

## 3. 传输安全

默认配置监听 `0.0.0.0:8080`。生产环境建议：

```toml
listen = "127.0.0.1:8080"
public_url = "https://cloud.example.com"
secure_cookies = true
```

由 Caddy/Nginx 终止 TLS，CloudNest 只接受本机回环流量。反向代理应：

- 使用有效的 TLS 证书，禁用过时协议与弱套件；
- 只转发必要的请求头，不信任客户端伪造的转发头；
- 设置合理的请求体上限与超时，并在代理层补充登录/分享解锁的速率限制（应用内限流只能按用户名，无法区分代理后的客户端 IP）；
- 限制管理入口的来源 IP（如条件允许）。

CloudNest 自身的安全响应头：启用 `secure_cookies`（HTTPS 部署）时返回 `Strict-Transport-Security`；动态 HTML/JSON 响应默认 `Cache-Control: no-store, no-cache, max-age=0, must-revalidate`，并附带 `Pragma: no-cache`、`Expires: 0` 与 `CDN-Cache-Control: no-store`，避免旧浏览器、企业代理或 CDN 复用旧页面；静态资源地址带内容哈希（`/static/v/<version>/…`）并使用 `immutable` 长缓存，旧固定地址仅做不可缓存的临时跳转。CSP 为 `script-src 'self'`（页面脚本全部外置，无内联脚本），仅样式保留 `'unsafe-inline'`。

Nginx/Caddy 示例见 [README.md](../README.md#反向代理与-https) 与 [docs/DEPLOY.md](DEPLOY.md)。

## 4. 授权与数据隔离

- 文件元数据记录所有者；普通用户只能访问自己的文件、回收站与分享；
- 分享链接使用随机 token，可设置访问密码、有效期和下载次数上限，降低链接泄露后的风险；
- 设置了下载次数上限的分享，下载与 `/raw` 内联取用都计入次数；分享页不再自动内联预览，改为显式下载，避免预览消耗次数或绕过限制；
- 未配置 `public_url` 时分享链接由 `Host` 头拼接，但会校验字符集；非法 Host 回退为相对路径。对外部署应配置 `public_url`，以获得稳定且可信的绝对链接；
- 公开分享仅暴露被分享的文件/文件夹，不暴露其他用户数据；
- 管理员可查看全站统计与用户配额；管理员账号应严格限制数量。

## 5. 进程与文件系统加固

systemd 单元 [packaging/cloudnest.service](../packaging/cloudnest.service) 默认启用：

| 指令 | 作用 |
| --- | --- |
| `User=cloudnest` / `Group=cloudnest` | 以非 root 专用用户运行 |
| `NoNewPrivileges=true` | 禁止进程提权 |
| `ProtectSystem=strict` | 文件系统只读，仅放行 `ReadWritePaths` |
| `ProtectHome=true` | 隐藏用户家目录 |
| `PrivateTmp=true` / `PrivateDevices=true` | 隔离临时目录与设备节点 |
| `ProtectKernelTunables/Modules/ControlGroups=true` | 限制内核攻击面 |
| `RestrictNamespaces`、`RestrictSUIDSGID`、`RestrictRealtime` | 限制危险能力 |
| `MemoryDenyWriteExecute=true` | 禁止可写可执行内存 |
| `SystemCallFilter=@system-service` | 限制系统调用集合 |
| `CapabilityBoundingSet=` / `AmbientCapabilities=` | 清空 Linux capabilities |
| `UMask=0077` | 新建文件默认私有权限 |
| `LimitNOFILE=65536` | 限制文件描述符上限 |

安装后的目录权限：

| 路径 | 权限 | 属主 |
| --- | --- | --- |
| `/opt/cloudnest` | 0770 | root:cloudnest |
| `/etc/cloudnest` | 0770 | root:cloudnest |
| `/etc/cloudnest/config.toml` | 0640 | root:cloudnest |
| `/var/lib/cloudnest` | 0750 | cloudnest:cloudnest |

为了支持网页修改配置与一键更新，systemd 单元在 `ReadWritePaths` 中额外放行了 `/etc/cloudnest` 与 `/opt/cloudnest`。这意味着服务账号可以写回自己的配置文件，并替换自己的可执行文件；这是网页自助运维所需的信任边界，仍远小于 root：

- 配置保存只改 `/etc/cloudnest/config.toml`，不会执行任意命令；
- 在线更新只能从固定的 GitHub Releases 下载、强制校验 SHA256 后替换程序，并原地重启进程；
- 若不需要网页设置/更新，可执行 `chmod 0750 /etc/cloudnest /opt/cloudnest` 收回写权限，网页操作会明确报错，CLI/root 操作不受影响。

注意：修改 `data_dir` 时必须同步调整 systemd 的 `ReadWritePaths` 与目录属主，否则服务无法写入；详见 [docs/DEPLOY.md](DEPLOY.md#4-配置)。

## 6. 数据与备份安全

- 数据库与用户文件均位于 `data_dir`，其安全性依赖主机文件权限与磁盘保护；
- 备份目录包含数据库与文件内容，可能包含密码哈希、分享链接与会话相关数据，应按敏感数据对待；
- 备份建议使用 root 权限创建（`0700`），并加密后异地保存；
- 恢复操作会覆盖现有数据，必须在停服状态下执行，并先确认备份来源可信；
- 如磁盘或云主机支持，建议启用磁盘加密或加密卷，以缓解物理介质丢失风险；
- 定期测试恢复流程，避免备份损坏在需要时才发现。

## 7. 发布与供应链

- 发布包由 GitHub Actions 在 `v*` tag 上构建，构建前校验 tag 与 `Cargo.toml` 版本一致；
- 产物为 musl 静态二进制，减少运行时依赖与动态库攻击面；
- 每位发布包都提供逐文件 `.sha256`，同时发布 `SHA256SUMS`；
- 安装脚本在解压前强制校验 SHA256，并拒绝包含绝对路径或 `..` 的归档；
- 发布包与校验文件通过 GitHub HTTPS 分发；
- 网页一键更新与 CLI 更新使用相同的发布资产与 SHA256 校验流程，更新前自动备份数据库；只有管理员会话且通过 CSRF 校验才能触发；
- 更新下载地址必须在允许的更新源内（配置的更新源主机，或默认源下的 `github.com` / `*.githubusercontent.com`，且强制 HTTPS；本机回环允许 http 以便离线/测试），发布包下载大小上限 256 MiB，校验值必须是合法的 64 位十六进制 SHA256。

建议：

- 下载后先核对 SHA256，再执行安装；
- 执行 `curl | bash` 前可先下载 `install.sh` 审阅；
- 仅从官方仓库的 Release 页面下载；
- 关注仓库的 Release 与安全公告，及时升级。

当前发布未提供独立于 GitHub 的二进制签名；如需更高保证，可在组织内部建立额外的校验与镜像流程。

## 8. 日志与隐私

- 服务日志通过 journald 收集，仅 root 与授权用户可读；
- 日志可能包含路径、用户名、错误上下文，但不应记录密码、会话 token 或分享密码明文；
- 排查问题时避免将完整日志或备份公开粘贴到公共渠道；
- 对外分享日志前先脱敏。

## 9. 资源限制与滥用防护

- 表单/JSON 等普通接口的请求体上限固定为 1 MiB，只有 multipart 上传接口放宽到 `max_upload_size_mb`，分片接口单请求上限 16 MiB；未认证接口（登录、初始化、分享解锁）同样受 1 MiB 限制；
- `default_quota_gb` 为新用户默认配额，管理员可为单个用户调整；配额在写入前校验；
- 未完成的分片上传按声明大小计入配额，避免通过并发上传绕过配额占满磁盘；超过 24 小时的残留临时文件由后台任务每 15 分钟清理一次；
- `/api/version` 仅管理员可用，`/healthz` 只返回服务状态、不返回内部细节；
- 分享链接可设置有效期与下载次数，降低公开链接被滥用的影响；
- 反向代理侧应设置请求体限制、超时与速率限制，作为应用层限制的补充。

## 10. 安全加固清单

部署后逐项确认：

- [ ] 使用 HTTPS，`public_url` 为真实 https 地址，`secure_cookies = true`
- [ ] 服务仅监听 `127.0.0.1:8080` 或受防火墙保护的内网地址
- [ ] 完成首次初始化并设置强管理员密码
- [ ] 定期检查用户列表与分享链接，清理不用的账号和过期分享
- [ ] `/etc/cloudnest/config.toml` 权限为 0640，数据目录权限为 0750
- [ ] systemd 单元保持默认加固项，不随意放宽
- [ ] 备份目录权限 0700，备份加密并异地保存
- [ ] 保持版本更新；升级前备份并阅读 Release Notes
- [ ] 确认 `/etc/cloudnest`、`/opt/cloudnest` 的写权限仅授予服务用户组；不需要网页运维时收回
- [ ] 监控磁盘、内存、日志与异常登录
- [ ] 在反向代理层为 `/login`、`/s/*/unlock` 配置速率限制
- [ ] 限制管理入口的网络来源

## 11. 已知限制

- CloudNest 不内置 TLS，必须依赖反向代理或前置负载均衡提供 HTTPS；
- 不提供静态数据加密；磁盘级加密需由主机或云平台提供；
- 当前为单节点 SQLite 架构，不支持多节点共享存储；
- 应用内登录失败锁定按用户名记录在进程内，重启后清零；知道用户名的人可以故意触发锁定，造成该账号最多 15 分钟无法登录（缓解手段是反向代理层限流与监控告警）；
- 应用无法在反向代理后识别真实客户端 IP，因此不按 IP 限流；
- 安全能力以代码实现与最新发布为准，本文档不固定版本号。

## 12. 漏洞报告

如发现安全问题，请通过 GitHub 仓库的私有漏洞报告渠道（Security Advisory）联系维护者，避免在公开 Issue 中披露可被利用的细节：

<https://github.com/katriyda/cloudnest/security/advisories>

报告时请尽量提供：受影响版本、复现步骤、影响范围与可能的修复建议。
