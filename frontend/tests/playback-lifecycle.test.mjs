import test from "node:test";
import assert from "node:assert/strict";
import { createPlaybackLifecycle } from "../src/core/player/playback-lifecycle.js";

const tick = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };
class Context extends EventTarget {
  state = "running";
  calls = 0;
  request = null;
  setState(state) { this.state = state; this.dispatchEvent(new Event("statechange")); }
  resume() {
    this.calls += 1;
    if (this.request) return this.request.promise.then(() => this.setState("running"));
    this.setState("running");
    return Promise.resolve();
  }
}
function harness(options = {}) {
  const page = new EventTarget(), browser = new EventTarget(), session = new EventTarget();
  session.state = "active";
  globalThis.document = page;
  globalThis.window = browser;
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: { audioSession: session } });
  const context = new Context(), events = [];
  const value = { context, wantsPlay: true, token: 1 };
  const lifecycle = createPlaybackLifecycle({ getSnapshot: () => value,
    onInterrupted() { events.push("interrupted"); options.onInterrupted?.(value); },
    onRecovered() { events.push("recovered"); },
    ...(options.resume ? { resume: options.resume } : {}),
  });
  lifecycle.update();
  return { page, browser, session, context, events, value, lifecycle };
}

test("an active context interruption resumes once and reports real recovery", async () => {
  const h = harness(); h.context.setState("suspended"); await tick();
  assert.equal(h.context.calls, 1);
  assert.equal(h.context.state, "running");
  assert.deepEqual(h.events, ["interrupted", "recovered"]);
  h.lifecycle.dispose();
});

test("unchanged updates do not loop after a rejected policy request", async () => {
  const h = harness(); h.context.request = deferred();
  h.context.setState("interrupted");
  h.context.request.reject(new DOMException("Denied", "NotAllowedError")); await tick();
  for (let i = 0; i < 20; i++) h.lifecycle.update();
  assert.equal(h.context.calls, 1);
  assert.deepEqual(h.events, ["interrupted"]);
  h.context.request = null;
  h.browser.dispatchEvent(new Event("pageshow")); await tick();
  assert.equal(h.context.calls, 2);
  assert.deepEqual(h.events, ["interrupted", "recovered"]);
  h.lifecycle.dispose();
});

test("visibility and page restore share one in-flight resume request", async () => {
  const h = harness(); h.context.request = deferred();
  h.context.setState("suspended");
  h.page.dispatchEvent(new Event("visibilitychange")); h.browser.dispatchEvent(new Event("pageshow"));
  h.page.dispatchEvent(new Event("resume")); h.lifecycle.update();
  assert.equal(h.context.calls, 1);
  h.context.request.resolve(); await tick();
  assert.deepEqual(h.events, ["interrupted", "recovered"]);
  h.lifecycle.dispose();
});

test("explicit user pause is never resumed by page or context lifecycle", async () => {
  const h = harness(); h.value.wantsPlay = false; h.lifecycle.update();
  h.context.setState("suspended");
  h.page.dispatchEvent(new Event("visibilitychange")); h.browser.dispatchEvent(new Event("pageshow"));
  h.session.dispatchEvent(new Event("statechange")); await tick();
  assert.equal(h.context.calls, 0);
  assert.deepEqual(h.events, []);
  h.lifecycle.dispose();
});

test("a late resume result cannot override a pause or announce recovery", async () => {
  const h = harness(); h.context.request = deferred();
  h.context.setState("suspended");
  h.value.wantsPlay = false; h.lifecycle.update();
  h.context.request.resolve(); await tick();
  assert.deepEqual(h.events, ["interrupted"]);
  h.lifecycle.dispose();
});

test("external audio-session interruption blocks resume until the owner releases it", async () => {
  const h = harness(); h.session.state = "interrupted";
  h.context.setState("interrupted"); h.browser.dispatchEvent(new Event("pageshow"));
  assert.equal(h.context.calls, 0);
  assert.deepEqual(h.events, ["interrupted"]);
  h.session.state = "active"; h.session.dispatchEvent(new Event("statechange")); await tick();
  assert.equal(h.context.calls, 1);
  assert.deepEqual(h.events, ["interrupted", "recovered"]);
  h.lifecycle.dispose();
});

test("switching tracks detaches old contexts and ignores their pending completion", async () => {
  const h = harness(); const old = h.context; old.request = deferred(); old.setState("suspended");
  const replacement = new Context(); h.value.context = replacement; h.value.token = 2; h.lifecycle.update();
  old.request.resolve(); await tick();
  assert.deepEqual(h.events, ["interrupted"]);
  old.setState("suspended"); assert.equal(old.calls, 1);
  replacement.setState("suspended"); await tick();
  assert.equal(replacement.calls, 1);
  assert.deepEqual(h.events, ["interrupted", "interrupted", "recovered"]);
  h.lifecycle.dispose();
});

test("an OS interruption is respected even before the context changes state", async () => {
  const h = harness(); h.session.state = "interrupted"; h.session.dispatchEvent(new Event("statechange"));
  assert.deepEqual(h.events, ["interrupted"]); assert.equal(h.context.calls, 0);
  h.session.state = "active"; h.session.dispatchEvent(new Event("statechange")); await tick();
  assert.deepEqual(h.events, ["interrupted", "recovered"]); assert.equal(h.context.calls, 0);
  h.lifecycle.dispose();
});

test("a pause during the interruption callback wins before resume starts", async () => {
  const h = harness({ onInterrupted: value => { value.wantsPlay = false; } });
  h.context.setState("suspended"); await tick();
  assert.equal(h.context.calls, 0);
  assert.deepEqual(h.events, ["interrupted"]);
  h.lifecycle.dispose();
});

test("advancing track generations on the same context retains one resume request", async () => {
  const h = harness(); h.context.request = deferred(); h.context.setState("suspended");
  h.value.token = 2; h.lifecycle.update(); h.page.dispatchEvent(new Event("visibilitychange"));
  assert.equal(h.context.calls, 1);
  h.context.request.resolve(); await tick();
  assert.equal(h.events.filter(event => event === "recovered").length, 1);
  h.lifecycle.dispose();
});

test("explicit play can retry the same paused context", async () => {
  const h = harness(); h.value.wantsPlay = false; h.lifecycle.update();
  h.context.setState("suspended");
  h.value.wantsPlay = true; h.lifecycle.update(); await tick();
  assert.equal(h.context.calls, 1);
  assert.deepEqual(h.events, ["interrupted", "recovered"]);
  h.lifecycle.dispose();
});

test("closed contexts are not resumed and optional browser APIs are harmless", async () => {
  const h = harness(); h.context.setState("closed"); h.lifecycle.update();
  h.page.dispatchEvent(new Event("visibilitychange")); await tick();
  assert.equal(h.context.calls, 0); assert.deepEqual(h.events, []); h.lifecycle.dispose();
  delete globalThis.document; delete globalThis.window;
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: {} });
  const lifecycle = createPlaybackLifecycle({ getSnapshot: () => ({ wantsPlay: true }) });
  lifecycle.update(); lifecycle.dispose();
});

test("dispose removes listeners and invalidates late recovery", async () => {
  const h = harness(); h.context.request = deferred(); h.context.setState("suspended");
  h.lifecycle.dispose(); h.lifecycle.dispose(); h.context.request.resolve(); await tick();
  h.context.setState("suspended"); h.browser.dispatchEvent(new Event("pageshow"));
  assert.equal(h.context.calls, 1); assert.deepEqual(h.events, ["interrupted"]);
});

test("an inaccessible optional audioSession API cannot break playback setup", () => {
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: { get audioSession() { throw new Error("Unavailable"); } } });
  const lifecycle = createPlaybackLifecycle({ getSnapshot: () => ({ context: null, wantsPlay: false }) });
  lifecycle.update(); lifecycle.dispose();
});
