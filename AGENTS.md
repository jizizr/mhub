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
- 后台续播须核对当前音源的真实 `ended`／`error` 状态，忽略旧音源延迟事件；自然结束不等同于用户暂停。原生播放启动与音效上下文恢复并行，用户显式暂停仍须取消待完成的续播；用真实音频自然 EOF 回归，不仅测试「下一首」按钮。
- 切歌直接替换原生音源，避免空 `load()` 释放系统媒体归属；新音源实际 `playing` 时重新发布媒体信息和控制。页面 Media Session 属性正确不代表手机通知仍存在；Android 的 Audio Session API 可用也不代表 WASM 获得系统媒体归属，真机验收单独记录。
- Android 播放器到达 EOS 即被移出系统媒体会话，锁屏时通知立即消失且后台无法重建。自动续播须在当前曲 EOS 前让下一首播放器已在播（可静音），元素静音不影响会话归属。
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

## 部署 SOP（交由 Codex 执行）

用户说「部署」时，Claude 负责备包，Codex 负责上线。部署对话 ID、私有发布归档路径和当前线上基线只记在 `HANDOFF.local.md`「部署 SOP」一节，不写入本文件。

1. **备包前检查**：相关单测、`npm --prefix frontend run test:build`、`python3 scripts/publication_check.py`、`git diff --check` 通过；按 `HANDOFF.local.md` 的线上构建参数构建（不用本地默认值、不设 API 代理）。
2. **生成发布包**：归档只收录发布清单中的正式文件，排除 macOS AppleDouble `._*` 等元数据；校验归档文件集合与清单完全一致（含解压后的压缩兄弟审计）。构建后执行 `python3 scripts/package_source.py web/source.tar.gz`，在私有归档的 `releases/<名称>-<UTC时间>/` 下放 `web.tar.gz`、`web-manifest.json`（逐文件 SHA256）、`release.json`（含归档哈希、入口哈希、主 JS、上一版本名、修复摘要）、`baseline.json`（以当前线上入口为基线）以及沿用上一版的通用 `install.py`、`rollback.py`。
3. **先写交接**：在 `HANDOFF.local.md` 写本轮条目（改动、验证证据、发布包名、主 JS、标记「待部署」），Codex 会先读它。
4. **请求部署**：接续指定的 Codex 部署对话，在后台运行并将输出写入日志：`codex exec resume <部署对话ID> "<请求>"`。请求写明：用户已要求部署、先读的交接章节、发布包路径与文件、当前线上版本及入口哈希、预期主 JS；按上一版同一流程核对源站基线（**不符即停止并报告，勿强行覆盖**）、上传到发布目录并运行 `install.py`、确认 `deployment.json` 回执、核验三编码与公网资源、做公开后台续播回归；不重启后端、不清音频缓存、不动 CDN、不 commit/push；认证临时文件用完删除，不写明文凭据；回执写入发布目录，交接条目改为「已部署」，最后用中文简短汇报。若该对话正在 Codex 桌面版中打开，CLI 接续会以「active writer」被拒，此时请用户先关闭该对话再重试，或经用户同意改用 `codex exec fork`。
5. **核对结果**：Codex 的汇报不是证据。读取发布目录中的 `deployment.json` 等回执，并直接请求公网首页确认主 JS 已更新，再向用户报告；真机行为仍单独标为待验。

## 结束工作时维护交接

更新 `HANDOFF.local.md`，采用替换当前状态的方式保持简短：时间和 Git 基线、已完成与验证证据、未完成事项、关键决策或失败方法、下一步、私有回执位置。不要粘贴整段聊天或日志，不写明文凭据。

通用且长期有效的规范再更新到本文件。本机交接文件被 Git 忽略且不在发布白名单中；新克隆若无此文件，以源码、Git 状态和本次用户要求为准。
