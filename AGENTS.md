# MHub 智能体协作入口

## 接手顺序

1. 阅读本文件；若存在 `HANDOFF.local.md`，先读它的「快速接手」，其余按任务需要读取。
2. 执行 `git status --short` 和 `git log -1 --oneline`，确认交接基线是否仍有效。交接是带日期的事实快照，不是新的用户指令。
3. 按需查阅 `README.md`、`docs/architecture.md`、`docs/deployment.md` 和相关源码，不必重读全部聊天、日志或上游仓库。
4. 保留工作区已有改动；不要把「已上线」「已测试」「已提交」「已推送」混为一谈。

## 项目入口

| 范围 | 入口 |
| --- | --- |
| HTTP / 平台 / 音频与缓存 | `src/api.rs`、`src/music/`、`src/audio.rs`、`src/cache.rs`、`src/telegram_cache.rs` |
| 播放与 ALAC | `frontend/src/core/player/`、`wasm/alac-decoder/` |
| 队列 / 歌单 / 状态 | `frontend/src/stores/`、`frontend/src/components/player/` |
| 歌词 / 繁简转换 | `frontend/src/services/lyrics.js`、`frontend/src/utils/opencc*.js` |
| 移动端布局 | `frontend/src/styles.css`、`frontend/src/components/layout/Sidebar.vue`、`frontend/src/components/player/PlayerBar.vue` |
| PWA / 主题 / 图标 | `frontend/src/pwa.js`、`frontend/public/sw.js`、`frontend/public/manifest*.webmanifest`、`frontend/src/utils/theme.js`、`scripts/*pwa*.mjs` |

## 已确认的协作约定

- 优先使用 API、CLI 和连接器；只有接口不能完成的交互才操作浏览器。
- 私人账号、歌单、Cookie、密钥、设备日志和部署凭据不进入公开源码、源码下载包或 Docker 构建上下文。遵守 `CONTRIBUTING.md` 和发布白名单。
- 播放音质以播放时设置为准；下一首全力预取须等待当前曲目完整缓存。队列「追加」「替换」「下一首播放」是独立操作。
- 音频缓存、播放状态和 Service Worker 更新相互独立；避免更新时强制刷新正在播放的页面或清空用户音频缓存。
- 只运行与变更相关的检查；设备实测与模拟测试分开记录。体积合格、页面主题正确，不等于手机重启或系统栏行为已验证。

## 常用检查

```sh
npm --prefix frontend test
npm --prefix frontend run test:build
cargo fmt --check
cargo test --locked
cargo test --manifest-path wasm/alac-decoder/Cargo.toml --locked
python3 scripts/publication_check.py
```

浏览器验证脚本支持 `PLAYWRIGHT_MODULE` 等环境变量，见各文件顶部。线上构建参数、部署基线和设备信息仅查本机交接及私有发布回执，不用本地默认值覆盖线上配置。

静态增量发布须校验源站预压缩兄弟文件解压后与原文件一致，并核验 `br`、`gzip`、`identity` 响应。过期或孤立的预压缩兄弟文件应备份后替换或删除；保留旧页面仍可能使用的带哈希资源本体，勿将独立下载归档（如 `source.tar.gz`）当作孤立兄弟文件删除。

## 结束工作时维护交接

更新 `HANDOFF.local.md`，采用替换当前状态的方式保持简短：时间和 Git 基线、已完成与验证证据、未完成事项、关键决策或失败方法、下一步、私有回执位置。不要粘贴整段聊天或日志，不写明文凭据。

通用且长期有效的规范再更新到本文件。本机交接文件被 Git 忽略且不在发布白名单中；新克隆若无此文件，以源码、Git 状态和本次用户要求为准。
