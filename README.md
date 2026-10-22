# MHub

[![Verify](https://github.com/jizizr/mhub/actions/workflows/ci.yml/badge.svg)](https://github.com/jizizr/mhub/actions/workflows/ci.yml)

自托管音乐播放器。Rust 后端统一处理多平台音乐来源、播放任务和缓存，Vue 前端提供推荐、歌单、队列、歌词和浏览器离线缓存。

[部署指南](docs/deployment.md) · [架构说明](docs/architecture.md) · [参与贡献](CONTRIBUTING.md) · [许可证](LICENSE)

## 功能

- 多平台搜索、歌曲匹配、歌单导入和推荐歌单浏览。
- 播放队列追加、替换、下一首播放、拖动排序及随机播放。
- 支持 ALAC 的 WASM 解码与 AudioWorklet 流式播放，兼容浏览器原生音频路径。
- 按播放时音质选择文件；区分音质和音频表示，兼容已有缓存。
- 歌词预取、OpenCC 简繁转换、Media Session 系统媒体控制。
- 可选交叉淡化与音量平衡；当前曲目完整缓存后预取下一首。
- 可选 MySQL 索引和 Telegram 文件缓存复用。

不同平台的可用能力取决于账号、地区、上游接口和浏览器支持。Apple Music 播放需要自行配置有效账号凭据和外部 wrapper 服务；本仓库不提供账号、会话或媒体文件。

## Docker 启动

需要 Docker Engine 和 Docker Compose v2。首次构建会编译 Rust、ALAC WASM 和前端，耗时取决于机器与网络。

```sh
git clone https://github.com/jizizr/mhub.git
cd mhub
cp config.example.toml config.toml
cp .env.example .env
# 按需要编辑 config.toml；初次可保留空凭据验证界面和健康检查。
docker compose up -d --build
```

打开 <http://localhost:8000>。服务默认只绑定本机回环地址，生产环境通过带访问控制的 HTTPS 反向代理提供访问。应用目前不提供用户隔离或内置 API 鉴权，不应把配置了共享账号的服务直接匿名暴露到公网。

```sh
docker compose ps
docker compose logs --tail=100 music
curl --fail http://localhost:8000/health
docker compose down
```

配置文件以只读方式挂载，缓存保存在 `music-data` 命名卷中。普通 `down` 不删除缓存；`down -v` 会删除命名卷。完整的升级、备份、外部服务和媒体域配置见[部署指南](docs/deployment.md)。

## 本地开发

需要 Rust 1.96+、Node.js 22.12+、FFmpeg/FFprobe。后端默认监听 `127.0.0.1:8000`，前端开发服务代理 API 到该地址。

```sh
cp config.example.toml config.toml
MUSIC_API_CONFIG=config.toml cargo run
```

另一终端：

```sh
cd frontend
npm ci
npm run dev
```

前端保留小体积的生成版 ALAC 绑定，常规开发无需重新编译 WASM。修改解码器后按[解码器说明](wasm/alac-decoder/README.md)重新构建；Docker 总是从源码重建。

```sh
cargo fmt --check
cargo test --locked
cargo test --manifest-path wasm/alac-decoder/Cargo.toml --locked
npm --prefix frontend test
npm --prefix frontend run test:build
python3 scripts/publication_check.py
```

真实平台下载测试默认忽略，须自行提供有效配置后显式运行。发布检查和自动化测试不能替代各平台及真实移动设备的播放验证。

## 目录

```text
src/                    Rust 服务、平台适配、音频和缓存
frontend/src/           Vue 界面、播放器、队列和浏览器缓存
frontend/tests/         前端行为和音频管线回归测试
wasm/alac-decoder/      独立 Rust ALAC 解码器
scripts/                回归验证与发布边界检查
docs/                   通用架构与部署说明
Dockerfile              多阶段构建；非 root 运行
compose.yaml            持久化、健康检查和运行时约束
config.example.toml     不含凭据的配置模板
```

## 开源与隐私

本仓库不包含线上 API 文档、访问密码、个人歌单、截图、生产域名配置或历史部署记录。运行数据、账号配置和构建目录不进入 Git，也不进入 Docker 构建上下文。清理文档不影响应用的 API 功能。

应用按 **AGPL-3.0-only** 发布，保留 SPlayer-Next 和 Apple Music-like Lyrics 的来源与许可说明。独立 ALAC 解码器保留其宽松许可证。详见 [NOTICE](NOTICE) 和[第三方说明](THIRD_PARTY_NOTICES.txt)。Docker 版界面提供当前版本源码下载。
