// The now-playing backdrop is a dimmed, flowing blur of the artwork, so its
// overall tone is close to the cover's average colour, darkened. That tone
// tints the system bars while the full player is open. DIM was matched to
// rendered backdrops: their top edge sits near 0.46 of the average, the
// bottom near 0.31.
export const COVER_TONE_FALLBACK = "#0f1116";
const DIM = 0.42;
const SIZE = 8;

const hex = (value) => Math.round(value).toString(16).padStart(2, "0");

export function toneFromPixels(data) {
  let r = 0, g = 0, b = 0, weight = 0;
  for (let i = 0; i < data.length; i += 4) {
    const alpha = data[i + 3] / 255;
    r += data[i] * alpha;
    g += data[i + 1] * alpha;
    b += data[i + 2] * alpha;
    weight += alpha;
  }
  if (!weight) return COVER_TONE_FALLBACK;
  return `#${hex((r / weight) * DIM)}${hex((g / weight) * DIM)}${hex((b / weight) * DIM)}`;
}

const cache = new Map();

// Resolves to the fallback when the image is not CORS-readable or fails.
export function coverTone(url) {
  if (!url) return Promise.resolve(COVER_TONE_FALLBACK);
  if (cache.has(url)) return cache.get(url);
  const result = new Promise((resolve) => {
    const image = new Image();
    image.crossOrigin = "anonymous";
    image.decoding = "async";
    image.onload = () => {
      try {
        const canvas = document.createElement("canvas");
        canvas.width = canvas.height = SIZE;
        const context = canvas.getContext("2d", { willReadFrequently: true });
        context.drawImage(image, 0, 0, SIZE, SIZE);
        resolve(toneFromPixels(context.getImageData(0, 0, SIZE, SIZE).data));
      } catch {
        resolve(COVER_TONE_FALLBACK);
      }
    };
    image.onerror = () => resolve(COVER_TONE_FALLBACK);
    image.src = url;
  });
  cache.set(url, result);
  if (cache.size > 32) cache.delete(cache.keys().next().value);
  return result;
}
