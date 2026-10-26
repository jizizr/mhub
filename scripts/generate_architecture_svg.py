#!/usr/bin/env python3
"""Draw docs/images/architecture-{light,dark}.svg for the README."""
from pathlib import Path
from xml.sax.saxutils import escape

ROOT = Path(__file__).resolve().parent.parent
W, H = 1200, 800
FONT = "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Microsoft YaHei', Helvetica, Arial, sans-serif"

THEMES = {
    "light": {
        "bg": "#ffffff", "text": "#1d1d1f", "muted": "#6e6e73", "line": "#d2d2d7", "arrow": "#86868b",
        "card": "#ffffff", "card_stroke": "#e5e5ea", "shadow": "rgba(0,0,0,0.06)",
        "client": ("#fff1f3", "#fa233b"), "edge": ("#f5f5f7", "#86868b"),
        "server": ("#eef4ff", "#0a6cff"), "external": ("#f5f5f7", "#86868b"),
    },
    "dark": {
        "bg": "#1c1c1e", "text": "#f5f5f7", "muted": "#a1a1a6", "line": "#3a3a3c", "arrow": "#8e8e93",
        "card": "#2c2c2e", "card_stroke": "#3a3a3c", "shadow": "rgba(0,0,0,0.35)",
        "client": ("#2a1d20", "#fa2d48"), "edge": ("#242426", "#8e8e93"),
        "server": ("#1a2333", "#4c9bff"), "external": ("#242426", "#8e8e93"),
    },
}

# (title, lines) for each card. Lines are short so they never wrap.
CLIENT = [
    ("界面", ["Vue 3 · Pinia · Vite", "首页、搜索、歌单、队列", "Apple Music 风格液态玻璃"]),
    ("播放引擎", ["原生 <audio> 双元素预载", "ALAC WASM → AudioWorklet", "交叉淡化 · 音量平衡"]),
    ("歌词", ["AMLL 逐字歌词", "WebGL 网格渐变背景", "OpenCC 简繁转换"]),
    ("离线与系统", ["IndexedDB 音频 / 歌词缓存", "Service Worker · PWA", "Media Session 系统控制"]),
]
SERVER = [
    ("HTTP API", ["/api/v1 · 长轮询任务状态", "预压缩 .br / .gz 静态资源", "对应源码下载"]),
    ("推荐", ["Apple Music AutoPlay 电台", "个性推荐 · 相似艺人回退", "首页推荐与歌单"]),
    ("账号与同步", ["通行密钥 · Argon2 会话", "yrs CRDT · y-sync WebSocket", "跨设备接力播放"]),
    ("平台适配", ["Apple Music · 网易云 · QQ", "酷狗 · B 站 · 汽水", "YouTube Music · 歌单导入"]),
    ("播放任务", ["按播放时音质选源", "下载 / 解密 · 响度分析", "ALAC 包流 · Range 输出"]),
    ("缓存协调", ["数据卷文件缓存", "可选 MySQL 索引", "可选 Telegram 文件复用"]),
]
EXTERNAL = [
    ("音乐平台接口", "搜索 · 歌单 · 歌词 · 推荐"),
    ("Apple wrapper", "raw / wrapper-lite（自建）"),
    ("MySQL", "账号、资料库 · 可选"),
    ("Telegram Bot API", "文件缓存复用 · 可选"),
]


def text(x, y, value, size, fill, weight=400, anchor="start", family=FONT):
    return (f'<text x="{x}" y="{y}" font-family="{family}" font-size="{size}" font-weight="{weight}" '
            f'fill="{fill}" text-anchor="{anchor}">{escape(value)}</text>')


def rect(x, y, w, h, r, fill, stroke, width=1, extra=""):
    return f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="{fill}" stroke="{stroke}" stroke-width="{width}" {extra}/>'


def card(t, x, y, w, h, title, lines, accent):
    out = [rect(x, y, w, h, 12, t["card"], t["card_stroke"], 1, 'filter="url(#shadow)"')]
    out.append(f'<rect x="{x + 16}" y="{y + 18}" width="4" height="16" rx="2" fill="{accent}"/>')
    out.append(text(x + 28, y + 31, title, 15, t["text"], 600))
    for index, line in enumerate(lines):
        out.append(text(x + 16, y + 58 + index * 20, line, 12.5, t["muted"]))
    return out


def band(t, x, y, w, h, label, sub, colors):
    fill, accent = colors
    return [rect(x, y, w, h, 18, fill, "none"),
            text(x + 22, y + 30, label, 16, accent, 700),
            text(x + 22 + len(label) * 15 + 18, y + 30, sub, 12.5, t["muted"])]


def arrow(t, x1, y1, x2, y2, label=None, dashed=False, lx=None, ly=None):
    dash = ' stroke-dasharray="5 5"' if dashed else ""
    out = [f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke="{t["arrow"]}" stroke-width="1.6"{dash} marker-end="url(#arrow)"/>']
    if label:
        out.append(text(lx if lx is not None else x1 + 10, ly if ly is not None else (y1 + y2) / 2 + 4, label, 12, t["muted"]))
    return out


def draw(name):
    t = THEMES[name]
    parts = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" role="img" aria-label="MHub 架构图">',
             '<defs>',
             f'<filter id="shadow" x="-10%" y="-10%" width="120%" height="140%"><feDropShadow dx="0" dy="2" stdDeviation="3" flood-color="{t["shadow"]}"/></filter>',
             f'<marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0 0L10 5L0 10z" fill="{t["arrow"]}"/></marker>',
             '</defs>',
             rect(0, 0, W, H, 24, t["bg"], "none")]

    # 1. Browser / PWA
    parts += band(t, 24, 24, W - 48, 172, "浏览器 / PWA", "桌面 · Android · iOS，可安装、可离线播放已缓存音乐", t["client"])
    cw = (W - 48 - 44 - 3 * 16) / 4
    for i, (title, lines) in enumerate(CLIENT):
        parts += card(t, 46 + i * (cw + 16), 62, cw, 114, title, lines, t["client"][1])

    # 2. Edge
    parts += arrow(t, 420, 196, 420, 236, "HTTPS · REST / 长轮询 / WebSocket", lx=432, ly=221)
    parts += arrow(t, 1010, 196, 1010, 236, "音频 Range", dashed=True, lx=1022, ly=221)
    parts.append(rect(24, 240, 760, 52, 14, t["edge"][0], "none"))
    parts.append(text(46, 272, "反向代理", 15, t["text"], 600))
    parts.append(text(130, 272, "TLS · 访问控制 · 限流 · WebSocket 升级 · 不缓冲音频", 12.5, t["muted"]))
    parts.append(rect(800, 240, W - 824, 52, 14, t["edge"][0], t["line"], 1, 'stroke-dasharray="5 5"'))
    parts.append(text(822, 272, "媒体域 / CDN", 15, t["text"], 600))
    parts.append(text(932, 272, "可选，只缓存已完成媒体", 12.5, t["muted"]))
    parts += arrow(t, 420, 292, 420, 332)
    parts += arrow(t, 1010, 292, 1010, 332, dashed=True)

    # 3. Server
    parts += band(t, 24, 336, W - 48, 302, "MHub 服务", "Docker 容器 · Rust · Axum · Tokio · 非 root、只读根文件系统", t["server"])
    sw = (W - 48 - 44 - 2 * 16) / 3
    for i, (title, lines) in enumerate(SERVER):
        row, col = divmod(i, 3)
        parts += card(t, 46 + col * (sw + 16), 374 + row * 130, sw, 114, title, lines, t["server"][1])

    # 4. External
    parts += arrow(t, 600, 638, 600, 678)
    parts += band(t, 24, 682, W - 48, 94, "外部服务", "", t["external"])
    ew = (W - 48 - 190 - 22 - 3 * 14) / 4
    for i, (title, sub) in enumerate(EXTERNAL):
        x = 210 + i * (ew + 14)
        parts.append(rect(x, 700, ew, 58, 12, t["card"], t["card_stroke"], 1, 'filter="url(#shadow)"'))
        parts.append(text(x + 16, 724, title, 14, t["text"], 600))
        parts.append(text(x + 16, 744, sub, 12, t["muted"]))

    parts.append("</svg>")
    return "\n".join(parts) + "\n"


def main():
    out = ROOT / "docs/images"
    out.mkdir(parents=True, exist_ok=True)
    for name in THEMES:
        (out / f"architecture-{name}.svg").write_text(draw(name))
    print("Wrote", ", ".join(f"architecture-{name}.svg" for name in THEMES))


if __name__ == "__main__":
    main()
