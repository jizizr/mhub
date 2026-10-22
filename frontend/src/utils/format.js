export function formatTime(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return "0:00";
  const seconds = Math.floor(ms / 1000);
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

export function formatSize(bytes) {
  if (!bytes) return "未知大小";
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

export function splitArtists(text) {
  const value = String(text || "").trim();
  if (!value) return [];
  return value
    .split(/[\/,&、，]+/)
    .map((name) => name.trim())
    .filter(Boolean);
}
