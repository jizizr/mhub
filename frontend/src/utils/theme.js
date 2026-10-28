const COLORS = { light: "#ffffff", dark: "#1f1f1f" };

// Keep one resolved theme-color, so an explicit app theme does not depend on
// the browser host selecting the correct OS media query.
export function createThemeController(readTheme, win = window, doc = document) {
  const media = win.matchMedia("(prefers-color-scheme: dark)");
  // A full-screen surface (the now-playing view) may take over the system
  // bars: theme-color tints the status bar, and the root background is what
  // Chromium shows in the gesture-bar area when it does not draw under it.
  let chrome = "";
  function sync() {
    const theme = readTheme();
    const explicit = theme === "light" || theme === "dark";
    const scheme = explicit ? theme : (media.matches ? "dark" : "light");
    const root = doc.documentElement;
    if (explicit) root.dataset.theme = theme;
    else delete root.dataset.theme;
    root.style.colorScheme = scheme;
    root.style.backgroundColor = chrome || COLORS[scheme];
    doc.querySelector('meta[name="theme-color"]').content = chrome || COLORS[scheme];
    doc.querySelector('meta[name="color-scheme"]').content = scheme;
    // The stable manifest id/start_url preserve app identity across variants.
    const manifest = doc.querySelector('link[rel="manifest"]');
    const href = scheme === "dark" ? "/manifest-dark.webmanifest?v=29e8713a10" : "/manifest.webmanifest?v=29e8713a10";
    if (manifest.getAttribute("href") !== href) manifest.setAttribute("href", href);
  }
  function onVisible() { if (doc.visibilityState === "visible") sync(); }
  media.addEventListener("change", sync);
  win.addEventListener("pageshow", sync);
  doc.addEventListener("visibilitychange", onVisible);
  return {
    sync,
    setChrome(color) {
      if ((color || "") === chrome) return;
      chrome = color || "";
      sync();
    },
    dispose() {
      media.removeEventListener("change", sync);
      win.removeEventListener("pageshow", sync);
      doc.removeEventListener("visibilitychange", onVisible);
    },
  };
}
