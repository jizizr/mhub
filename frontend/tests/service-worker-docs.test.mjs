import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const source = await readFile(new URL("../public/sw.js", import.meta.url), "utf8");
const origin = "https://music.example.test";
const appCache = "music-web-app-v1";
const documentationPaths = [
  "/docs", "/docs/", "/docs/api", "/docs/api/", "/docs/api/index.html",
  "/docs/api/api.md", "/docs/api/openapi.json", "/docs/api/download_song.py",
  "/docs/api/?old=1", "/%64ocs/api/", "/docs%2fapi/index.html",
  "/%2564ocs%252fapi/openapi.json", "/docs//api/index.html",
  "/other%2f..%2fdocs/api/", "/other/../docs/api/", "/docs%5capi/index.html",
];

function request(path, options = {}) {
  return {
    url: new URL(path, origin).href,
    method: "GET",
    mode: "navigate",
    destination: "document",
    headers: new Headers(),
    ...options,
  };
}

function harness({ entries = {}, network = async () => new Response("network") } = {}) {
  const listeners = new Map();
  const cacheData = new Map(Object.entries(entries).map(([name, records]) => [
    name, new Map(Object.entries(records).map(([url, body]) => [new URL(url, origin).href, new Response(body)])),
  ]));
  const calls = { put: [], match: [], delete: [], open: [], fetch: [], claim: 0 };
  const key = (value) => typeof value === "string" ? new URL(value, origin).href : value.url;
  const fetchNetwork = (value) => {
    calls.fetch.push(value.url);
    return network(value);
  };
  const caches = {
    keys: async () => [...cacheData.keys()],
    open: async (name) => {
      calls.open.push(name);
      if (!cacheData.has(name)) cacheData.set(name, new Map());
      const cache = cacheData.get(name);
      return {
        keys: async () => [...cache.keys()].map((url) => request(url)),
        put: async (value, response) => {
          calls.put.push({ name, url: key(value) });
          cache.set(key(value), response);
        },
        match: async (value) => {
          calls.match.push({ name, url: key(value) });
          return cache.get(key(value))?.clone();
        },
        delete: async (value) => {
          calls.delete.push({ name, url: key(value) });
          return cache.delete(key(value));
        },
        addAll: async () => {},
      };
    },
  };
  vm.runInNewContext(source, {
    URL,
    caches,
    fetch: fetchNetwork,
    self: {
      location: { origin },
      addEventListener: (name, handler) => listeners.set(name, handler),
      skipWaiting: async () => {},
      clients: { claim: async () => { calls.claim += 1; } },
    },
  });
  return {
    calls,
    cacheData,
    dispatch(value) {
      let response;
      listeners.get("fetch")({ request: value, respondWith: (result) => { response = result; } });
      return {
        intercepted: response !== undefined,
        // Not calling respondWith hands the request to the browser's network.
        response: response ?? fetchNetwork(value),
      };
    },
    async activate() {
      let completion;
      listeners.get("activate")({ waitUntil: (result) => { completion = result; } });
      await completion;
    },
  };
}

test("documentation navigations and downloadable files always use the network", async () => {
  const worker = harness();
  for (const path of documentationPaths) {
    for (const mode of ["navigate", "cors"]) {
      const result = worker.dispatch(request(path, { mode }));
      assert.equal(result.intercepted, false, `${path} (${mode})`);
      assert.equal(await (await result.response).text(), "network");
    }
  }
  assert.deepEqual(worker.calls.open, []);
  assert.deepEqual(worker.calls.match, []);
  assert.deepEqual(worker.calls.put, []);
});

test("offline documentation never falls back to a cached guide or player shell", async () => {
  const worker = harness({
    entries: { [appCache]: { "/": "player shell", "/docs/api/": "previously public guide" } },
    network: async () => { throw new TypeError("offline"); },
  });
  const result = worker.dispatch(request("/docs/api/"));
  assert.equal(result.intercepted, false);
  await assert.rejects(result.response, /offline/);
  assert.deepEqual(worker.calls.match, []);
  assert.deepEqual(worker.calls.put, []);
});

test("successful authentication does not persist documents or replace a later 401", async () => {
  const worker = harness({
    network: async (value) => value.headers.has("Authorization")
      ? new Response("private guide", { headers: { "Cache-Control": "private, no-store" } })
      : new Response("Authentication required", { status: 401 }),
  });
  const authenticated = worker.dispatch(request("/docs/api/", {
    headers: new Headers({ Authorization: "Basic synthetic-fixture" }),
  }));
  assert.equal(authenticated.intercepted, false);
  assert.equal(await (await authenticated.response).text(), "private guide");
  const anonymous = worker.dispatch(request("/docs/api/"));
  assert.equal(anonymous.intercepted, false);
  assert.equal((await anonymous.response).status, 401);
  assert.equal(worker.cacheData.size, 0);
  assert.deepEqual(worker.calls.put, []);
});

test("activation removes old documentation from every owned app cache only", async () => {
  const owned = ["music-web-app-v0", appCache, "music-web-app-v2"];
  const entries = Object.fromEntries(owned.map((name) => [name, {
    ...Object.fromEntries(documentationPaths.map((path) => [path, "old guide"])),
    "/": "player shell",
    "/assets/player.js": "player code",
    "/api/v1/playbacks/fixture/audio": "audio",
    "https://other.example.test/docs/api/": "other origin",
  }]));
  entries["music-audio-cache"] = { "/api/v1/playbacks/fixture/audio": "audio" };
  entries["unrelated-app"] = { "/docs/api/": "not owned by this worker" };
  const worker = harness({ entries });
  await worker.activate();
  assert.equal(worker.calls.claim, 1);
  assert.deepEqual(worker.calls.open, owned);
  for (const name of owned) {
    const records = worker.cacheData.get(name);
    assert.deepEqual([...records.keys()], [
      `${origin}/`, `${origin}/assets/player.js`, `${origin}/api/v1/playbacks/fixture/audio`,
      "https://other.example.test/docs/api/",
    ]);
  }
  assert.equal(worker.cacheData.get("music-audio-cache").size, 1);
  assert.equal(worker.cacheData.get("unrelated-app").size, 1);
});

test("ordinary player navigation retains its existing offline behavior", async () => {
  const worker = harness({
    entries: { [appCache]: { "/": "offline player shell" } },
    network: async () => { throw new TypeError("offline"); },
  });
  const result = worker.dispatch(request("/"));
  assert.equal(result.intercepted, true);
  assert.equal(await (await result.response).text(), "offline player shell");
});

test("similar non-document paths remain cacheable and APIs are still passed through", async () => {
  const worker = harness();
  const player = worker.dispatch(request("/docs-player"));
  assert.equal(player.intercepted, true);
  assert.equal(await (await player.response).text(), "network");
  assert.deepEqual(worker.calls.put, [{ name: appCache, url: `${origin}/docs-player` }]);
  const api = worker.dispatch(request("/api/v1/platforms", { mode: "cors" }));
  assert.equal(api.intercepted, false);
  await api.response;
});
