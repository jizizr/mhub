/**
 * Two-device account and sync regression against a running dev server whose
 * backend has `[auth]` enabled (rp_origin = MHUB_ORIGIN) and a MySQL database.
 * Uses Chromium's virtual WebAuthn authenticator; no real account data.
 *
 * MHUB_ORIGIN=http://localhost:5173 PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs \
 *   node scripts/verify_account_sync_browser.mjs
 */
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

const origin = process.env.MHUB_ORIGIN || "http://localhost:5173";
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE ? pathToFileURL(process.env.PLAYWRIGHT_MODULE) : "playwright");
const browser = await chromium.launch({ headless: true, ...(process.env.BROWSER_EXECUTABLE ? { executablePath: process.env.BROWSER_EXECUTABLE } : {}) });
const username = `e2e${Date.now().toString(36)}`;
const password = "correct horse battery";
const results = {};
const track = (id, title) => ({ platform: "applemusic", source_id: String(id), selected_id: String(id), id: `applemusic--${id}--high`, title, artist: "测试歌手", cover: "", quality: "high" });

async function device(name, { guestPlaylists = [], liked = [] } = {}) {
  const context = await browser.newContext({ viewport: { width: 1280, height: 860 } });
  await context.addInitScript(({ guestPlaylists, liked, name }) => {
    if (sessionStorage.getItem("seeded")) return;
    sessionStorage.setItem("seeded", "1");
    localStorage.setItem("music-web-device-v1", JSON.stringify({ id: `${name}-${Math.random().toString(16).slice(2)}`, name }));
    if (guestPlaylists.length) localStorage.setItem("music-web-splayer-playlists-v1", JSON.stringify(guestPlaylists));
    if (liked.length) localStorage.setItem("music-web-splayer-recommendation-liked-v1", JSON.stringify(liked));
  }, { guestPlaylists, liked, name });
  const page = await context.newPage();
  const cdp = await context.newCDPSession(page);
  await cdp.send("WebAuthn.enable");
  await cdp.send("WebAuthn.addVirtualAuthenticator", { options: {
    protocol: "ctap2", transport: "internal", hasResidentKey: true, hasUserVerification: true, isUserVerified: true,
  } });
  await page.goto(origin);
  await page.waitForFunction(() => document.querySelector(".sidebar"));
  return { context, page };
}

const store = (page, fn, arg) => page.evaluate(async ({ fn, arg }) => {
  const [{ usePlaylistStore }, { useAccountStore }, { useQueueStore }, { useStatusStore }] = await Promise.all([
    import("/src/stores/playlists.js"), import("/src/stores/account.js"), import("/src/stores/queue.js"), import("/src/stores/status.js"),
  ]);
  const stores = { playlists: usePlaylistStore(), account: useAccountStore(), queue: useQueueStore(), status: useStatusStore() };
  return new Function("stores", "arg", `return (${fn})(stores, arg);`)(stores, arg);
}, { fn: fn.toString(), arg });

const playlistSummary = (page) => store(page, ({ playlists }) => playlists.items
  .map((item) => `${item.name}:${item.tracks.map((t) => t.title).join("|")}`).sort());
const until = async (page, check, label, timeout = 10_000) => {
  const started = Date.now();
  for (;;) {
    const value = await check();
    if (value) return value;
    if (Date.now() - started > timeout) throw new Error(`timed out: ${label}`);
    await page.waitForTimeout(250);
  }
};

try {
  // 1. Guest with existing data on device A signs up and merges it.
  const a = await device("设备A", {
    guestPlaylists: [{ id: "guest-1", name: "游客歌单", tracks: [track(1, "晴天"), track(2, "稻香")], created_at: 1, updated_at: 1 }],
    liked: ["applemusic:1"],
  });
  await a.page.getByRole("button", { name: /登录以同步/ }).click();
  await a.page.getByRole("button", { name: "创建账号" }).click();
  await a.page.getByRole("button", { name: /改用密码/ }).click();
  await a.page.getByLabel("用户名").fill(username);
  await a.page.getByLabel("密码").fill(password);
  await a.page.getByRole("button", { name: "创建账号", exact: true }).click();
  await a.page.getByText("保存恢复码").waitFor();
  const codes = await a.page.locator(".account__codes code").allTextContents();
  assert.equal(codes.length, 8);
  await a.page.getByRole("button", { name: "我已保存" }).click();
  await a.page.getByText("合并此设备上的资料").waitFor();
  await a.page.getByRole("button", { name: "合并到账号" }).click();
  await until(a.page, async () => (await playlistSummary(a.page)).includes("游客歌单:晴天|稻香"), "guest playlist merged");
  results.guestMerge = await playlistSummary(a.page);
  assert.deepEqual(await store(a.page, ({ playlists }) => playlists.liked), ["applemusic:1"]);

  // 2. Add a passkey, sign out (keeping nothing), sign back in with it.
  await a.page.getByRole("button", { name: new RegExp(username) }).first().click();
  await a.page.getByRole("button", { name: "在此设备添加通行密钥" }).click();
  await until(a.page, () => store(a.page, ({ account }) => account.user?.passkeys >= 1), "passkey added");
  await a.page.getByRole("button", { name: "退出登录" }).click();
  await until(a.page, () => store(a.page, ({ account }) => account.mode === "guest"), "signed out");
  assert.deepEqual(await playlistSummary(a.page), ["游客歌单:晴天|稻香"], "guest data is back after sign-out");
  await a.page.getByRole("button", { name: /登录以同步/ }).click();
  await a.page.getByRole("button", { name: "使用通行密钥登录" }).click();
  await until(a.page, () => store(a.page, ({ account }) => account.mode === "linked" && account.sync === "synced"), "passkey sign-in");
  await a.page.waitForTimeout(500);
  assert.equal(await a.page.getByText("合并此设备上的资料").count(), 0, "already imported guest data is not offered again");
  results.passkeyLogin = true;

  // 3. Device B signs in with the password and receives live changes.
  const b = await device("设备B");
  await b.page.getByRole("button", { name: /登录以同步/ }).click();
  await b.page.getByLabel("用户名").fill(username);
  await b.page.getByLabel("密码").fill(password);
  await b.page.getByRole("button", { name: "登录", exact: true }).click();
  await until(b.page, async () => (await playlistSummary(b.page)).includes("游客歌单:晴天|稻香"), "device B sees the library");
  const liveStarted = Date.now();
  await store(a.page, ({ playlists }, t) => playlists.create({ name: "实时新建", tracks: [t] }), track(3, "七里香"));
  await until(b.page, async () => (await playlistSummary(b.page)).includes("实时新建:七里香"), "live update on B");
  results.liveMs = Date.now() - liveStarted;

  // 4. B's session disappears (revoked from A, like an expired login); B keeps
  //    editing its local replica. A edits the same playlist meanwhile.
  await b.page.keyboard.press("Escape");
  await a.page.evaluate(async () => {
    const { listSessions, revokeSession } = await import("/src/services/account.js");
    const { sessions } = await listSessions();
    for (const session of sessions.filter((item) => item.device_name === "设备B")) await revokeSession(session.id);
  });
  await until(b.page, () => store(b.page, ({ account }) => account.mode === "expired"), "B is told its session ended");
  const playlistId = await store(b.page, ({ playlists }) => playlists.items.find((p) => p.name === "游客歌单").id);
  await store(b.page, ({ playlists }, { id, t }) => {
    playlists.rename(id, "离线改名");
    playlists.merge(id, [t]);
    playlists.removeTrack(id, { platform: "applemusic", source_id: "2" });
  }, { id: playlistId, t: track(4, "夜曲") });
  await store(a.page, ({ playlists }, { id, t }) => playlists.merge(id, [t]), { id: playlistId, t: track(5, "简单爱") });
  await b.page.reload();
  await until(b.page, () => store(b.page, ({ account }) => account.mode === "expired"), "B shows expired after reload");
  assert.ok((await playlistSummary(b.page)).some((item) => item.startsWith("离线改名:")), "offline edits survived reload in IndexedDB");
  await b.page.getByRole("button", { name: /登录已过期/ }).click();
  await b.page.getByLabel("用户名").fill(username);
  await b.page.getByLabel("密码").fill(password);
  await b.page.getByRole("button", { name: "登录", exact: true }).click();
  const expected = "离线改名:晴天|夜曲|简单爱";
  const sortTitles = (value) => value.split(":")[0] + ":" + value.split(":")[1].split("|").sort().join("|");
  await until(b.page, async () => (await playlistSummary(b.page)).map(sortTitles).includes(sortTitles(expected)), "B merged after re-login");
  await until(a.page, async () => (await playlistSummary(a.page)).map(sortTitles).includes(sortTitles(expected)), "A received B's offline edits");
  assert.deepEqual(await playlistSummary(a.page), await playlistSummary(b.page), "both devices converge");
  results.afterReloginMerge = await playlistSummary(a.page);

  // 5. Hand-off: A reports its queue, B offers to continue it.
  await store(a.page, ({ queue, status, account }, tracks) => {
    queue.setQueue(tracks);
    status.playIndex = 1;
    status.position = 42_000;
    account.publishPlayback({ flush: true });
  }, [track(1, "晴天"), track(3, "七里香")]);
  await b.page.getByRole("button", { name: /在线音乐/ }).first().click();
  await b.page.locator(".handoff").getByText("七里香").waitFor({ timeout: 10_000 });
  results.handoff = await b.page.locator(".handoff").innerText();

  console.log(JSON.stringify({ passed: true, username, results }, null, 2));
} finally {
  await browser.close();
}
