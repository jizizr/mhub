# Docker 部署

## 部署边界

镜像包含前端、Rust 后端、从源码构建的 ALAC WASM、FFmpeg/FFprobe、健康检查及对应源码包。镜像不包含平台账号、wrapper 认证环境、MySQL 或 Telegram 服务。没有凭据也可启动界面和健康检查；完整播放能力需要对应平台的有效配置。

首次部署按根目录 README 复制 `config.example.toml` 为 `config.toml`，复制 `.env.example` 为 `.env`，然后运行 `docker compose up -d --build`。配置文件必须在启动前存在，Compose 不会将缺失文件误建为目录。

容器以 UID/GID `10001:10001` 运行，根文件系统只读，去除 Linux capabilities，临时目录单独挂载，日志轮转。默认仅发布到 `127.0.0.1:8000`。命名卷首次创建会继承镜像的数据目录权限；使用已有绑定目录时，应先为 UID 10001 授予写权限。

## 配置

| 位置 | 用途 | 生效方式 |
| --- | --- | --- |
| `config.toml` | 平台凭据、wrapper 地址、可选数据库、Telegram 和账号 `[auth]` | 修改后重建容器进程 |
| `.env` | 监听地址、端口、日志级别及可选媒体域 | `docker compose up -d` |
| `MUSIC_MEDIA_BASE_URL` | 已完成媒体的独立源；默认同源 | 改动后 `docker compose up -d --build` |
| `MUSIC_LEGACY_MEDIA_BASE_URL` | 迁移后旧队列媒体地址的兼容源 | 改动后重新构建前端 |

媒体域必须是无用户名、密码、路径、查询参数的 HTTP(S) origin。启用独立媒体域时，应配置正确 CORS、Range、版本查询参数与缓存策略。只缓存已完成的成功媒体响应，不能缓存播放任务状态、未就绪流或错误响应。

`config.toml` 中空字符串表示未配置。按需填写平台 Cookie 或 `music.applemusic`，不需要把模板中每个平台都启用。YouTube Music Cookie 可通过后端环境变量 `YOUTUBE_MUSIC_COOKIE` 注入；其他环境覆盖见源码 `settings.rs` 及各平台适配器。

不要把 Cookie、token、数据库密码放入 `Dockerfile`、构建参数或前端环境变量。Vite 变量会公开到浏览器。

## 外部服务

- **Apple Music**：自行部署并认证兼容的 raw wrapper 或 wrapper-lite，在配置中填入容器可访问的地址。`127.0.0.1` 在容器里指容器自身。wrapper-lite 默认关闭；需要时设置 `wrapper_lite_enabled` 和对应 URL，保留旧 wrapper 回退配置。区域、账号授权和上游可用性仍然影响下载。Widevine 兼容路径需要自行提供 `wv_device` 设备文件，或 `wv_client_id` 与 `wv_private_key` 文件对；将文件只读挂载并填写容器内路径，公开版本不再内置第三方测试设备凭据。
- **MySQL**：填写已有兼容 `music_cache` 表的数据库 URL；不启用数据库时仍可运行应用，但不能复用数据库记录的远端缓存。除启用账号后自动创建的 `mhub_` 前缀表外，本项目不会创建或迁移外部数据库。
- **Telegram**：填写 Bot token、缓存 chat ID 和可选 Bot API 地址。大文件通常需要自建 Local Bot API。该服务与本容器必须共享文件存储，并通过 `MUSIC_API_TELEGRAM_LOCAL_ROOT`、`MUSIC_API_TELEGRAM_CONTAINER_ROOT` 对齐路径；可在自己的 Compose override 中添加只读共享卷和这两个环境变量。不要把整个账号数据目录复制进镜像。
- **本地音频缓存**：`data/downloads` 下每首歌一个目录，存放解密后的音频和按需生成的 ALAC 包、浏览器用 FLAC。它只是 Telegram 与各平台之上的加速层：后端每 15 分钟（以及启动时）清理一次，连续 `[audio_cache] idle_hours`（默认 72）小时无人请求的目录被删除，总量超过 `max_gib`（默认 20）时从最久未用的开始删到 90%。正在下载或 30 分钟内写入、请求过的目录不会被删。已上传 Telegram 的歌之后从 Telegram 取；未上传的（未配置 Telegram 或上传失败）会重新下载。Local Bot API 自己的文件目录不归本服务管理。

本仓库不分发 wrapper 二进制或第三方服务的认证数据，也不把任何外部服务的认证操作伪装成镜像首次启动过程。

## HTTPS 反向代理

浏览器 AudioWorklet、Service Worker 等功能需要安全上下文。除本机开发外使用 HTTPS。反向代理对音频请求应关闭响应缓冲、支持长连接和 Range，保留响应的 `Cache-Control`、`ETag`、`Content-Range` 等头，并设置适合大文件的超时。

示例 Nginx location（放入已有 TLS 站点，并配置自己的访问控制）：

```nginx
location / {
    proxy_pass http://127.0.0.1:8000;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_buffering off;
    proxy_read_timeout 600s;
}
```

音乐 API 本身没有用户鉴权，账号只保护个人歌单和接力播放。CORS 不是访问控制，保护静态首页也不能代替保护 `/api/`。对外部署应在代理层统一控制访问并限制请求频率。不要在代理日志里记录 Cookie 或敏感查询参数。

## 账号与同步

账号功能默认关闭。启用时在 `config.toml` 的 `[auth]` 中设置：

- `rp_origin`：用户访问的 HTTPS origin，如 `https://music.example.com`；`rp_id` 为空时取其主机名。通行密钥绑定 RP ID，上线后更换域名会使已注册的通行密钥失效。
- `cookie_secure`：HTTPS 部署保持 `true`；仅本机 HTTP 调试时设为 `false`。
- `turnstile_site_key`、`turnstile_secret`：开放注册时建议配置 Cloudflare Turnstile；留空则不校验。
- `session_days`、`library_limit_bytes`：会话有效期与单账号资料库上限。

账号需要 `db.mysql.url` 指向可写的 MySQL；表在首次使用时创建。反向代理和 CDN 需对 `/api/v1/sync/ws` 放行 WebSocket 升级（Nginx 需 `proxy_set_header Upgrade $http_upgrade; proxy_set_header Connection "upgrade";`），并且不缓存 `/api/v1/auth/`、`/api/v1/sync/`。不支持 WebSocket 时客户端会退回 HTTP 同步，但失去实时推送。若有多个后端实例，同一账号的连接须落到同一实例（当前广播在进程内）。备份数据库时一并保留 `mhub_` 表。

本机 macOS 编译 `webauthn-rs` 依赖的 OpenSSL 时，可设置 `OPENSSL_DIR=/opt/homebrew/opt/openssl@3`；Docker 构建镜像已包含所需库。

## 升级、备份与恢复

升级前备份配置与数据，固定需要部署的 Git commit/tag：

```sh
git pull --ff-only
docker compose up -d --build
docker compose ps
docker compose logs --tail=100 music
```

配置更新后可执行 `docker compose up -d --force-recreate music`。应用收到 SIGTERM 时停止接受新连接并等待活动请求，Compose 最长等待 30 秒；升级期间长音频连接仍可能中断。

备份需要同时保留本机 `config.toml`、`.env`、缓存卷以及自行管理的数据库/TG 存储。备份包含敏感信息，应单独保护，不能提交 Git。

下面使用当前项目的 Compose 挂载定位缓存卷，停机后以只读方式导出：

```sh
mkdir -p backups
chmod 700 backups
docker compose stop music
container=$(docker compose ps -aq music)
volume=$(docker inspect "$container" --format '{{range .Mounts}}{{if eq .Destination "/app/data"}}{{.Name}}{{end}}{{end}}')
docker run --rm -v "$volume:/data:ro" -v "$PWD/backups:/backup" debian:bookworm-slim tar czf /backup/music-data.tar.gz -C /data .
docker compose start music
```

恢复前停止服务，将备份解压到目标数据卷并确认 UID 10001 可写。`docker compose down` 保留数据卷；只有明确需要丢弃缓存时才使用 `down -v`。

## 故障检查

若构建网络无法访问 `index.crates.io`，可在 `.env` 中将 `CARGO_INDEX_URL` 设为 Rust 官方 crates.io 索引的 GitHub 源 `https://raw.githubusercontent.com/rust-lang/crates.io-index/master/`，然后重新构建。默认仍使用 crates.io；不关闭 TLS 校验，也不更换 lockfile 中的包校验和。

- 容器不健康：查看 `docker compose logs music`，验证 `/health`。
- 配置挂载失败：确认文件存在且容器 UID 可读。
- 缓存写入失败：检查数据卷权限、磁盘空间及外部存储路径映射。
- 上游播放失败：检查配置、wrapper 网络和上游授权；健康检查成功不代表第三方账号可用。
- 新音质首播慢：检查真实源格式、wrapper 和网络。WASM 与原生兼容路径的首播条件不同。
- 移动端后台暂停：浏览器和系统省电策略仍可能暂停网页，Media Session 不能保证永久后台运行。
