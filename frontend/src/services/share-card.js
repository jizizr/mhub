// Renders the share images: a song card (the cover over a blurred backdrop of
// itself, title, artist and a QR code for the link) and a lyric card (a few
// chosen lines set large, with the song and QR code below).
// Loaded on demand from the share dialog, together with the QR encoder.
import qrcode from "qrcode-generator";

const WIDTH = 1080;
const HEIGHT = 1440;
const FONT = '-apple-system, BlinkMacSystemFont, "SF Pro Display", "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", "Noto Sans CJK SC", sans-serif';

function loadImage(src) {
  return new Promise((resolve, reject) => {
    const image = new Image();
    // Without CORS a foreign cover would taint the canvas and block export;
    // with it, a cover that refuses CORS fails here and the next one is tried.
    image.crossOrigin = "anonymous";
    image.decoding = "async";
    image.onload = () => resolve(image);
    image.onerror = () => reject(new Error("cover unavailable"));
    image.src = src;
  });
}

async function firstImage(sources) {
  for (const src of sources) {
    if (!src) continue;
    try {
      return await loadImage(src);
    } catch {
      // Try the next candidate.
    }
  }
  return null;
}

function roundedRect(ctx, x, y, w, h, r) {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}

// Draws the image to cover the box, cropping the longer side.
function drawCover(ctx, image, x, y, w, h) {
  const scale = Math.max(w / image.naturalWidth, h / image.naturalHeight);
  const sw = w / scale;
  const sh = h / scale;
  ctx.drawImage(image, (image.naturalWidth - sw) / 2, (image.naturalHeight - sh) / 2, sw, sh, x, y, w, h);
}

// Canvas `filter` is missing in some Safari versions; drawing the cover tiny
// and scaling it back up gives the same soft backdrop everywhere.
function paintBackdrop(ctx, image, width, height) {
  if (image) {
    const small = document.createElement("canvas");
    small.width = 18;
    small.height = Math.max(12, Math.round((18 * height) / width));
    const sctx = small.getContext("2d");
    drawCover(sctx, image, 0, 0, small.width, small.height);
    ctx.imageSmoothingEnabled = true;
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(small, -60, -60, width + 120, height + 120);
  } else {
    const gradient = ctx.createLinearGradient(0, 0, width, height);
    gradient.addColorStop(0, "#3a2f5c");
    gradient.addColorStop(1, "#14121c");
    ctx.fillStyle = gradient;
    ctx.fillRect(0, 0, width, height);
  }
  const shade = ctx.createLinearGradient(0, 0, 0, height);
  shade.addColorStop(0, "rgba(10, 10, 14, 0.28)");
  shade.addColorStop(0.55, "rgba(10, 10, 14, 0.48)");
  shade.addColorStop(1, "rgba(10, 10, 14, 0.78)");
  ctx.fillStyle = shade;
  ctx.fillRect(0, 0, width, height);
}

function toPng(canvas) {
  return new Promise((resolve, reject) => {
    canvas.toBlob((blob) => (blob ? resolve(blob) : reject(new Error("图片生成失败"))), "image/png");
  });
}

// A lyric quote card. Its height follows the excerpt so short quotes are not
// lost in empty space and long ones are not squeezed.
export async function renderLyricCard({ title, artist, covers = [], lines = [], link, siteName }) {
  const image = await firstImage(covers);
  const measure = document.createElement("canvas").getContext("2d");
  const padX = 110;
  const textWidth = WIDTH - padX * 2;
  const count = lines.length;
  const size = count <= 2 ? 64 : count <= 4 ? 56 : 48;
  const lineGap = Math.round(size * 1.42);
  const transSize = Math.round(size * 0.56);
  const transGap = Math.round(transSize * 1.5);

  // Lay the quote out first to know the canvas height.
  const blocks = lines.map((line) => {
    measure.font = `700 ${size}px ${FONT}`;
    const main = wrapLines(measure, line.text, textWidth, 3);
    measure.font = `500 ${transSize}px ${FONT}`;
    const translation = line.translation ? wrapLines(measure, line.translation, textWidth, 2) : [];
    return { main, translation };
  });
  const quoteHeight = blocks.reduce((sum, block, index) =>
    sum + block.main.length * lineGap + block.translation.length * transGap + (index ? Math.round(size * 0.55) : 0), 0);
  const top = 150;
  const quoteMarkHeight = 120;
  const footer = 380;
  const height = Math.max(1080, Math.min(2400, top + quoteMarkHeight + quoteHeight + 120 + footer));

  const canvas = document.createElement("canvas");
  canvas.width = WIDTH;
  canvas.height = height;
  const ctx = canvas.getContext("2d");
  paintBackdrop(ctx, image, WIDTH, height);

  // Opening quotation mark.
  ctx.fillStyle = "rgba(255, 255, 255, 0.34)";
  ctx.font = `800 180px Georgia, "Times New Roman", serif`;
  ctx.textAlign = "left";
  ctx.textBaseline = "alphabetic";
  ctx.fillText("\u201C", padX - 12, top + 120);

  // The quote, vertically centred in the space above the footer.
  const available = height - footer - (top + quoteMarkHeight);
  let y = top + quoteMarkHeight + Math.max(0, (available - quoteHeight) / 2) + size;
  blocks.forEach((block, index) => {
    if (index) y += Math.round(size * 0.55);
    ctx.fillStyle = "#ffffff";
    ctx.font = `700 ${size}px ${FONT}`;
    for (const row of block.main) {
      ctx.fillText(row, padX, y);
      y += lineGap;
    }
    ctx.fillStyle = "rgba(255, 255, 255, 0.66)";
    ctx.font = `500 ${transSize}px ${FONT}`;
    for (const row of block.translation) {
      ctx.fillText(row, padX, y - lineGap + transGap + Math.round(size * 0.2));
      y += transGap;
    }
  });

  // Footer: a hairline, the song with its cover, and the QR code.
  const footerTop = height - footer + 60;
  ctx.fillStyle = "rgba(255, 255, 255, 0.22)";
  ctx.fillRect(padX, footerTop, textWidth, 2);
  const qrSize = 200;
  const rowTop = footerTop + 60;
  const thumb = 150;
  if (image) {
    ctx.save();
    roundedRect(ctx, padX, rowTop + (qrSize - thumb) / 2, thumb, thumb, 18);
    ctx.clip();
    drawCover(ctx, image, padX, rowTop + (qrSize - thumb) / 2, thumb, thumb);
    ctx.restore();
  }
  const metaX = image ? padX + thumb + 32 : padX;
  const metaWidth = WIDTH - padX - qrSize - 40 - metaX;
  ctx.fillStyle = "#ffffff";
  ctx.font = `700 42px ${FONT}`;
  ctx.fillText(wrapLines(ctx, title || "未知歌曲", metaWidth, 1)[0] || "", metaX, rowTop + 72);
  ctx.fillStyle = "rgba(255, 255, 255, 0.7)";
  ctx.font = `500 32px ${FONT}`;
  ctx.fillText(wrapLines(ctx, artist || "未知艺人", metaWidth, 1)[0] || "", metaX, rowTop + 122);
  ctx.fillStyle = "rgba(255, 255, 255, 0.5)";
  ctx.font = `500 26px ${FONT}`;
  ctx.fillText(`${siteName || "MHub"} · 扫码听完整歌曲`, metaX, rowTop + 172);
  drawQr(ctx, link, WIDTH - padX - qrSize, rowTop, qrSize);

  return toPng(canvas);
}

// Breaks text into at most `maxLines` lines, character by character so CJK
// titles without spaces wrap too, ending a cut line with an ellipsis.
function wrapLines(ctx, text, maxWidth, maxLines) {
  const chars = [...String(text || "")];
  const lines = [];
  let line = "";
  for (let index = 0; index < chars.length; index += 1) {
    const next = line + chars[index];
    if (ctx.measureText(next).width <= maxWidth || !line) {
      line = next;
      continue;
    }
    if (lines.length === maxLines - 1) {
      let cut = line;
      while (cut && ctx.measureText(`${cut}…`).width > maxWidth) cut = cut.slice(0, -1);
      lines.push(`${cut.trimEnd()}…`);
      return lines;
    }
    lines.push(line.trimEnd());
    line = chars[index].trimStart();
  }
  if (line) lines.push(line);
  return lines;
}

function drawQr(ctx, text, x, y, size) {
  const qr = qrcode(0, "M");
  qr.addData(text);
  qr.make();
  const count = qr.getModuleCount();
  const quiet = 2;
  const cell = size / (count + quiet * 2);
  ctx.fillStyle = "#ffffff";
  roundedRect(ctx, x, y, size, size, 22);
  ctx.fill();
  ctx.fillStyle = "#111114";
  for (let row = 0; row < count; row += 1) {
    for (let col = 0; col < count; col += 1) {
      if (!qr.isDark(row, col)) continue;
      // Overlap by a hair so neighbouring modules leave no seams.
      ctx.fillRect(x + (col + quiet) * cell, y + (row + quiet) * cell, cell + 0.6, cell + 0.6);
    }
  }
}

export async function renderShareCard({ title, artist, album, covers = [], link, siteName }) {
  const canvas = document.createElement("canvas");
  canvas.width = WIDTH;
  canvas.height = HEIGHT;
  const ctx = canvas.getContext("2d");
  const image = await firstImage(covers);

  paintBackdrop(ctx, image, WIDTH, HEIGHT);

  // Cover art with a soft drop shadow.
  const coverSize = 720;
  const coverX = (WIDTH - coverSize) / 2;
  const coverY = 120;
  ctx.save();
  ctx.shadowColor = "rgba(0, 0, 0, 0.45)";
  ctx.shadowBlur = 60;
  ctx.shadowOffsetY = 24;
  roundedRect(ctx, coverX, coverY, coverSize, coverSize, 36);
  ctx.fillStyle = "rgba(255, 255, 255, 0.12)";
  ctx.fill();
  ctx.restore();
  if (image) {
    ctx.save();
    roundedRect(ctx, coverX, coverY, coverSize, coverSize, 36);
    ctx.clip();
    drawCover(ctx, image, coverX, coverY, coverSize, coverSize);
    ctx.restore();
  } else {
    ctx.fillStyle = "rgba(255, 255, 255, 0.6)";
    ctx.font = `300 220px ${FONT}`;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.fillText("♪", WIDTH / 2, coverY + coverSize / 2);
  }

  // Title and artist, left-aligned under the cover.
  const textX = coverX;
  const textWidth = coverSize;
  let y = coverY + coverSize + 92;
  ctx.textAlign = "left";
  ctx.textBaseline = "alphabetic";
  ctx.fillStyle = "#ffffff";
  ctx.font = `700 60px ${FONT}`;
  for (const line of wrapLines(ctx, title || "未知歌曲", textWidth, 2)) {
    ctx.fillText(line, textX, y);
    y += 74;
  }
  ctx.fillStyle = "rgba(255, 255, 255, 0.72)";
  ctx.font = `500 38px ${FONT}`;
  const byline = [artist || "未知艺人", album].filter(Boolean).join(" — ");
  ctx.fillText(wrapLines(ctx, byline, textWidth, 1)[0] || "", textX, y - 6);

  // Footer: site name and hint on the left, QR code on the right.
  const qrSize = 200;
  const footerY = HEIGHT - 70 - qrSize;
  drawQr(ctx, link, coverX + coverSize - qrSize, footerY, qrSize);
  ctx.fillStyle = "#ffffff";
  ctx.font = `700 44px ${FONT}`;
  ctx.fillText(siteName || "MHub", textX, footerY + 88);
  ctx.fillStyle = "rgba(255, 255, 255, 0.64)";
  ctx.font = `500 30px ${FONT}`;
  ctx.fillText("扫码收听这首歌", textX, footerY + 142);

  return toPng(canvas);
}
