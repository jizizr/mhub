const APP_CACHE = "music-web-app-v1";
const APP_SHELL = ["/", "/manifest.webmanifest"];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(APP_CACHE)
      .then((cache) => cache.addAll(APP_SHELL))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    removeCachedDocumentation()
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET") return;

  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;
  // Authentication and offline failures must reach the network unchanged.
  // CacheStorage does not honor Cache-Control: no-store automatically.
  if (isDocumentationUrl(url)) return;
  if (url.pathname.startsWith("/api/") || url.pathname === "/health") return;

  if (request.mode === "navigate") {
    event.respondWith(networkFirst(request, "/"));
    return;
  }

  if (["script", "style", "font", "image", "manifest"].includes(request.destination)) {
    event.respondWith(staleWhileRevalidate(request));
  }
});

function isDocumentationUrl(url) {
  if (url.origin !== self.location.origin) return false;
  let path = url.pathname;
  while (true) {
    // Also recognize encoded separators and dot segments normalized by proxies.
    const segments = [];
    for (const segment of path.replaceAll("\\", "/").split("/")) {
      if (segment === "..") segments.pop();
      else if (segment && segment !== ".") segments.push(segment);
    }
    if (segments[0] === "docs") return true;
    try {
      const decoded = decodeURIComponent(path);
      if (decoded === path) return false;
      path = decoded;
    } catch {
      return false;
    }
  }
}

async function removeCachedDocumentation() {
  const names = await caches.keys();
  for (const name of names.filter((key) => key.startsWith("music-web-app-v"))) {
    const cache = await caches.open(name);
    const requests = await cache.keys();
    await Promise.all(requests
      .filter((request) => isDocumentationUrl(new URL(request.url)))
      .map((request) => cache.delete(request)));
  }
}

async function networkFirst(request, fallbackUrl) {
  const cache = await caches.open(APP_CACHE);
  try {
    const response = await fetch(request);
    if (response.ok) await cache.put(request, response.clone());
    return response;
  } catch {
    return (await cache.match(request)) || cache.match(fallbackUrl);
  }
}

async function staleWhileRevalidate(request) {
  const cache = await caches.open(APP_CACHE);
  const cached = await cache.match(request);
  const refresh = fetch(request)
    .then((response) => {
      if (response.ok) cache.put(request, response.clone());
      return response;
    })
    .catch(() => null);
  return cached || refresh;
}
