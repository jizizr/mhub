// Liquid glass refraction for the floating bars.
//
// Chromium can run an SVG filter as a `backdrop-filter`, so the backdrop is
// blurred, then bent by a displacement map shaped like a rounded glass slab:
// no shift in the flat middle, and a growing inward shift across the curved
// bezel, which magnifies and bends what passes under the edge. The map only
// depends on the element's size, so it is rebuilt on resize and never while
// scrolling. Safari and Firefox ignore SVG backdrop filters; they keep the
// plain CSS blur (styles.css).

const SVG_NS = "http://www.w3.org/2000/svg";
const ENABLED_CLASS = "has-liquid-glass";
let defs = null;
let nextId = 0;

/** Chromium-based browsers render SVG filters in backdrop-filter. */
export function supportsLiquidGlass(nav = globalThis.navigator) {
  const brands = nav?.userAgentData?.brands;
  if (Array.isArray(brands)) return brands.some((item) => /Chromium/i.test(item.brand));
  return /Chrome\/\d+/.test(nav?.userAgent || "") && !/Firefox|FxiOS|CriOS|EdgiOS/.test(nav?.userAgent || "");
}

/**
 * RGBA displacement map for a rounded rectangle of `width`×`height` CSS px.
 * Red/green encode the x/y shift around 128; `scale` is the feDisplacementMap
 * scale that maps the full 0–255 range to ±scale/2 px.
 */
export function displacementMap(width, height, { radius = height / 2, bezel = 18, shift = 12 } = {}) {
  const w = Math.max(1, Math.round(width));
  const h = Math.max(1, Math.round(height));
  const r = Math.min(radius, w / 2, h / 2);
  const scale = shift * 2;
  const data = new Uint8ClampedArray(w * h * 4);
  const hw = w / 2;
  const hh = h / 2;
  for (let y = 0; y < h; y += 1) {
    const py = y + 0.5 - hh;
    for (let x = 0; x < w; x += 1) {
      const px = x + 0.5 - hw;
      // Signed distance to the rounded rectangle and its outward normal.
      const qx = Math.abs(px) - (hw - r);
      const qy = Math.abs(py) - (hh - r);
      let nx = 0;
      let ny = 0;
      let distance;
      if (qx > 0 && qy > 0) {
        const length = Math.hypot(qx, qy);
        distance = r - length;
        nx = (qx / length) * Math.sign(px);
        ny = (qy / length) * Math.sign(py);
      } else if (qx > qy) {
        distance = r - qx;
        nx = Math.sign(px);
      } else {
        distance = r - qy;
        ny = Math.sign(py);
      }
      // Refraction grows towards the rim of the curved bezel.
      const t = Math.min(1, Math.max(0, distance / bezel));
      const amount = (1 - t) ** 2 * shift;
      const index = (y * w + x) * 4;
      data[index] = Math.round((0.5 - (nx * amount) / scale) * 255);
      data[index + 1] = Math.round((0.5 - (ny * amount) / scale) * 255);
      data[index + 2] = 128;
      data[index + 3] = 255;
    }
  }
  return { width: w, height: h, scale, data };
}

function ensureDefs(doc) {
  if (defs?.isConnected) return defs;
  const svg = doc.createElementNS(SVG_NS, "svg");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("width", "0");
  svg.setAttribute("height", "0");
  svg.style.cssText = "position:absolute;width:0;height:0;overflow:hidden;pointer-events:none";
  defs = doc.createElementNS(SVG_NS, "defs");
  svg.append(defs);
  doc.body.append(svg);
  return defs;
}

function element(doc, name, attributes) {
  const node = doc.createElementNS(SVG_NS, name);
  for (const [key, value] of Object.entries(attributes)) node.setAttribute(key, String(value));
  return node;
}

function mapUrl(doc, map) {
  const canvas = doc.createElement("canvas");
  canvas.width = map.width;
  canvas.height = map.height;
  canvas.getContext("2d").putImageData(new ImageData(map.data, map.width, map.height), 0, 0);
  return canvas.toDataURL("image/png");
}

/**
 * Give `target` a refraction filter sized to it. The element opts in with the
 * CSS custom property `--liquid-glass: 1` (so the desktop sidebar, which is
 * not glass, never builds a map) and reads the filter from
 * `--glass-refraction`. Returns a cleanup function.
 */
export function attachLiquidGlass(target, { bezel = 30, shift = 28, blur = 1.6, saturate = 1.8 } = {}) {
  const doc = target?.ownerDocument;
  if (!doc || !supportsLiquidGlass(doc.defaultView?.navigator)) return () => {};
  doc.documentElement.classList.add(ENABLED_CLASS);
  const id = `mhub-glass-${(nextId += 1)}`;
  const filter = element(doc, "filter", {
    id, x: 0, y: 0, width: 1, height: 1, filterUnits: "userSpaceOnUse", "color-interpolation-filters": "sRGB",
  });
  const image = element(doc, "feImage", { x: 0, y: 0, preserveAspectRatio: "none", result: "map" });
  // Tone mapping from the theme (CSS --glass-transfer: "slope intercept"):
  // dark glass lifts dark backdrops and dims bright ones, so light glyphs stay
  // legible over white artwork, as Apple's vibrancy does.
  const transfer = element(doc, "feComponentTransfer", { in: "vivid" });
  const channels = ["feFuncR", "feFuncG", "feFuncB"].map((name) => element(doc, name, { type: "linear", slope: 1, intercept: 0 }));
  transfer.append(...channels);
  const displace = element(doc, "feDisplacementMap", {
    in: "soft", in2: "map", scale: shift * 2, xChannelSelector: "R", yChannelSelector: "G", result: "bent",
  });
  filter.append(
    element(doc, "feGaussianBlur", { in: "SourceGraphic", stdDeviation: blur, edgeMode: "duplicate", result: "soft" }),
    image,
    displace,
    element(doc, "feColorMatrix", { in: "bent", type: "saturate", values: saturate, result: "vivid" }),
    transfer,
  );
  ensureDefs(doc).append(filter);

  let size = "";
  let timer = 0;
  const rebuild = () => {
    const style = doc.defaultView.getComputedStyle(target);
    const active = style.getPropertyValue("--liquid-glass").trim() === "1";
    const rect = target.getBoundingClientRect();
    if (!active || rect.width < 2 || rect.height < 2) {
      target.style.removeProperty("--glass-refraction");
      size = "";
      return;
    }
    const radius = parseFloat(style.borderTopLeftRadius) || 0;
    const [slope = 1, intercept = 0] = style.getPropertyValue("--glass-transfer").trim().split(/\s+/).map(Number);
    for (const node of channels) {
      node.setAttribute("slope", Number.isFinite(slope) ? slope : 1);
      node.setAttribute("intercept", Number.isFinite(intercept) ? intercept : 0);
    }
    const key = `${Math.round(rect.width)}x${Math.round(rect.height)}r${radius}`;
    if (key === size) return;
    size = key;
    // Short bars (the phone mini player) keep a flat middle band.
    const fit = Math.min(1, (rect.height * 0.46) / bezel);
    const map = displacementMap(rect.width, rect.height, { radius, bezel: bezel * fit, shift: shift * fit });
    for (const node of [filter, image]) {
      node.setAttribute("width", map.width);
      node.setAttribute("height", map.height);
    }
    image.setAttribute("href", mapUrl(doc, map));
    displace.setAttribute("scale", map.scale);
    target.style.setProperty("--glass-refraction", `url(#${id})`);
  };
  const schedule = () => {
    doc.defaultView.clearTimeout(timer);
    timer = doc.defaultView.setTimeout(rebuild, 120);
  };
  rebuild();
  const observer = new doc.defaultView.ResizeObserver(schedule);
  observer.observe(target);
  // The tone mapping follows the app or system theme.
  const themeObserver = new doc.defaultView.MutationObserver(schedule);
  themeObserver.observe(doc.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  const scheme = doc.defaultView.matchMedia?.("(prefers-color-scheme: dark)");
  scheme?.addEventListener?.("change", schedule);
  return () => {
    doc.defaultView.clearTimeout(timer);
    observer.disconnect();
    themeObserver.disconnect();
    scheme?.removeEventListener?.("change", schedule);
    filter.remove();
    target.style.removeProperty("--glass-refraction");
  };
}
