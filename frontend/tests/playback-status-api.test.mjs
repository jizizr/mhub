import test from "node:test";
import assert from "node:assert/strict";

let fixtureId = 0;
async function fixture(t) {
  const requests = [];
  t.mock.method(globalThis, "fetch", (url, options) => new Promise((resolve, reject) => {
    requests.push({ url, options, resolve, reject });
  }));
  const { getPlayback } = await import(`../src/apis/music.js?status-test=${++fixtureId}`);
  const respond = (index, value, status = 200) => requests[index].resolve(new Response(JSON.stringify(value), {
    status, headers: { "content-type": "application/json" },
  }));
  return { getPlayback, requests, respond };
}

const task = { id: "task/hires", status: "streaming", streaming_mode: "wasm_alac", details: { bytes: 12 } };
const flush = () => new Promise((resolve) => setImmediate(resolve));

test("three concurrent status observers share one request and receive independent JSON", async (t) => {
  const h = await fixture(t);
  const results = [h.getPlayback(task.id), h.getPlayback(task.id), h.getPlayback(task.id)];
  assert.equal(h.requests.length, 1);
  assert.equal(h.requests[0].url, "/api/v1/playbacks/task%2Fhires");
  assert.equal(h.requests[0].options.cache, "no-store");
  h.respond(0, task);
  const [first, second, third] = await Promise.all(results);
  assert.deepEqual(first, task);
  first.streaming_mode = null;
  first.details.bytes = 0;
  assert.deepEqual(second, task);
  assert.deepEqual(third, task);
});

test("different playback IDs have independent status requests", async (t) => {
  const h = await fixture(t);
  const first = h.getPlayback("first");
  const second = h.getPlayback("second");
  assert.equal(h.requests.length, 2);
  h.respond(1, { id: "second" });
  assert.deepEqual(await second, { id: "second" });
  h.respond(0, { id: "first" });
  assert.deepEqual(await first, { id: "first" });
});

test("sequential polls fetch current status instead of retaining a settled snapshot", async (t) => {
  const h = await fixture(t);
  for (const [index, status] of ["queued", "streaming", "completed", "queued"].entries()) {
    const result = h.getPlayback(task.id);
    assert.equal(h.requests.length, index + 1);
    h.respond(h.requests.length - 1, { ...task, status });
    assert.equal((await result).status, status);
  }
  assert.equal(h.requests.length, 4);
});

test("cancelling one subscriber leaves the shared fetch and other subscriber active", async (t) => {
  const h = await fixture(t);
  const controller = new AbortController();
  const cancelled = h.getPlayback(task.id, { signal: controller.signal });
  const active = h.getPlayback(task.id);
  const reason = new DOMException("Cancelled by caller", "AbortError");
  const rejection = assert.rejects(cancelled, (error) => error === reason);
  controller.abort(reason);
  await rejection;
  assert.equal(h.requests[0].options.signal.aborted, false);
  assert.notEqual(h.requests[0].options.signal, controller.signal);
  h.respond(0, task);
  assert.deepEqual(await active, task);
});

for (const lateResult of ["success", "failure"]) {
  test(`all subscribers cancelling aborts and evicts the fetch; late ${lateResult} cannot evict its replacement`, async (t) => {
    const h = await fixture(t);
    const firstController = new AbortController();
    const secondController = new AbortController();
    const first = h.getPlayback(task.id, { signal: firstController.signal });
    const second = h.getPlayback(task.id, { signal: secondController.signal });
    const rejections = Promise.all([
      assert.rejects(first, { name: "AbortError" }),
      assert.rejects(second, { name: "AbortError" }),
    ]);
    firstController.abort();
    assert.equal(h.requests[0].options.signal.aborted, false);
    secondController.abort();
    assert.equal(h.requests[0].options.signal.aborted, true);
    await rejections;

    const replacement = h.getPlayback(task.id);
    assert.equal(h.requests.length, 2, "cancelled requests are evicted before fetch settles");
    if (lateResult === "success") h.respond(0, task);
    else h.requests[0].reject(new Error("Late network failure"));
    await flush();
    const joinedReplacement = h.getPlayback(task.id);
    assert.equal(h.requests.length, 2, "old completion must not delete the replacement request");
    h.respond(1, { ...task, status: "completed" });
    assert.deepEqual(await Promise.all([replacement, joinedReplacement]), [
      { ...task, status: "completed" }, { ...task, status: "completed" },
    ]);
  });
}

test("a signal already aborted does not start or join a request", async (t) => {
  const h = await fixture(t);
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(h.getPlayback(task.id, { signal: controller.signal }), { name: "AbortError" });
  assert.equal(h.requests.length, 0);
  const active = h.getPlayback(task.id);
  await assert.rejects(h.getPlayback(task.id, { signal: controller.signal }), { name: "AbortError" });
  assert.equal(h.requests.length, 1);
  assert.equal(h.requests[0].options.signal.aborted, false);
  h.respond(0, task);
  await active;
});

for (const errorType of ["http", "network", "json"]) {
  test(`${errorType} failure reaches all observers and allows a fresh retry`, async (t) => {
    const h = await fixture(t);
    const first = h.getPlayback(task.id);
    const second = h.getPlayback(task.id);
    const rejections = Promise.all([
      assert.rejects(first), assert.rejects(second),
    ]);
    assert.equal(h.requests.length, 1);
    if (errorType === "http") h.respond(0, { detail: "Temporary error" }, 503);
    if (errorType === "network") h.requests[0].reject(new TypeError("Fetch failed"));
    if (errorType === "json") h.requests[0].resolve(new Response("invalid JSON"));
    await rejections;
    const retry = h.getPlayback(task.id);
    assert.equal(h.requests.length, 2);
    h.respond(1, task);
    assert.deepEqual(await retry, task);
  });
}

for (const succeeds of [true, false]) {
  test(`status ${succeeds ? "success" : "failure"} removes caller abort listeners`, async (t) => {
    const h = await fixture(t);
    const controller = new AbortController();
    const remove = t.mock.method(controller.signal, "removeEventListener");
    const result = h.getPlayback(task.id, { signal: controller.signal });
    const observed = succeeds ? result : assert.rejects(result);
    if (succeeds) h.respond(0, task);
    else h.requests[0].reject(new Error("Temporary network error"));
    await observed;
    assert.equal(remove.mock.callCount(), 1);
    assert.equal(remove.mock.calls[0].arguments[0], "abort");
    controller.abort();
    assert.equal(h.requests[0].options.signal.aborted, false, "settled caller no longer controls the shared request");
  });
}

test("long-poll reads name the known status and never share a plain status read", async (t) => {
  const h = await fixture(t);
  const held = h.getPlayback(task.id, { since: "processing", waitMs: 8000 });
  const plain = h.getPlayback(task.id);
  assert.equal(h.requests.length, 2);
  assert.equal(h.requests[0].url, "/api/v1/playbacks/task%2Fhires?since=processing&wait_ms=8000");
  assert.equal(h.requests[1].url, "/api/v1/playbacks/task%2Fhires");
  h.respond(1, { ...task, status: "processing" });
  h.respond(0, task);
  assert.equal((await plain).status, "processing");
  assert.equal((await held).status, "streaming");
});
