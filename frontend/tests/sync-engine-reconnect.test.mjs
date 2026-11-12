import test, { mock } from "node:test";
import assert from "node:assert/strict";
import * as Y from "yjs";

// The engine talks to `window`, `WebSocket` and `fetch`; stand-ins route
// timers through node:test's mocked clock at call time.
globalThis.window = {
  setTimeout: (...args) => setTimeout(...args),
  clearTimeout: (...args) => clearTimeout(...args),
  location: { href: "https://music.test/" },
};

const sockets = [];
class FakeSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSED = 3;
  static behaviour = "refuse";
  constructor(url) {
    this.url = String(url);
    this.readyState = FakeSocket.CONNECTING;
    sockets.push(this);
    // "refuse": the handshake fails at once (proxy without WebSocket).
    // "hang": the handshake never settles on its own.
    if (FakeSocket.behaviour === "refuse") queueMicrotask(() => this.close());
  }
  send() {}
  close() {
    if (this.readyState === FakeSocket.CLOSED) return;
    this.readyState = FakeSocket.CLOSED;
    queueMicrotask(() => this.onclose?.());
  }
}
globalThis.WebSocket = FakeSocket;

const toBase64 = (bytes) => Buffer.from(bytes).toString("base64");
const server = new Y.Doc();
let fetches = 0;
globalThis.fetch = async () => {
  fetches += 1;
  return {
    ok: true,
    status: 200,
    json: async () => ({
      update: toBase64(Y.encodeStateAsUpdate(server)),
      state_vector: toBase64(Y.encodeStateVector(server)),
    }),
  };
};

const { createReplica } = await import("../src/services/sync/engine.js");

const persist = () => ({ whenSynced: Promise.resolve(), destroy: async () => {}, clearData: async () => {} });
const flush = async () => { for (let index = 0; index < 20; index += 1) await Promise.resolve(); };
async function advance(ms, step = 100) {
  for (let elapsed = 0; elapsed < ms; elapsed += step) {
    mock.timers.tick(step);
    await flush();
  }
}

function setup(t, behaviour) {
  sockets.length = 0;
  fetches = 0;
  FakeSocket.behaviour = behaviour;
  mock.timers.enable({ apis: ["setTimeout"] });
  t.mock.method(Math, "random", () => 0.5);
  t.after(() => mock.timers.reset());
  const states = [];
  const replica = createReplica({ accountId: "a", persist, onState: (state) => states.push(state) });
  t.after(() => replica.stop());
  return { replica, states };
}

test("a refused WebSocket backs off although the HTTP fallback succeeds", async (t) => {
  const { replica, states } = setup(t, "refuse");
  replica.start();
  await flush();
  await advance(120_000);
  assert.ok(states.includes("synced"), "HTTP fallback keeps the library synced");
  // 1+2+4+8+16+30+30+30 s: about eight attempts in two minutes, not one a second.
  assert.ok(sockets.length <= 9, `WebSocket attempts: ${sockets.length}`);
  assert.ok(fetches <= 12, `HTTP syncs: ${fetches}`);
});

test("resyncNow leaves a connecting socket alone; a hung handshake times out", async (t) => {
  const { replica } = setup(t, "hang");
  replica.start();
  await flush();
  assert.equal(sockets.length, 1);
  replica.resyncNow();
  replica.resyncNow();
  assert.equal(sockets.length, 1, "no second socket while the first is connecting");
  assert.equal(fetches, 0);
  await advance(15_000);
  assert.equal(sockets[0].readyState, FakeSocket.CLOSED, "handshake abandoned after the timeout");
  assert.equal(fetches, 1, "falls back to one HTTP sync");
  replica.resyncNow();
  await flush();
  assert.equal(sockets.length, 2, "resyncNow reconnects at once while idle");
});
