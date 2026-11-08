import test from "node:test";
import assert from "node:assert/strict";

// A minimal IndexedDB stand-in: WebKit may close an idle connection on its
// own, after which db.transaction() throws InvalidStateError
// ("The database connection is closing").
const data = new Map();
const connections = [];
function closingError() {
  const error = new Error("Failed to execute 'transaction' on 'IDBDatabase': The database connection is closing.");
  error.name = "InvalidStateError";
  return error;
}
function makeDb() {
  const db = {
    closed: false,
    onclose: null,
    onversionchange: null,
    close() { this.closed = true; },
    transaction() {
      if (this.closed) throw closingError();
      const transaction = { oncomplete: null, onerror: null, onabort: null };
      transaction.objectStore = () => ({
        get(key) {
          const request = { result: data.get(key) };
          return request;
        },
        put(value, key) { data.set(key, value); return {}; },
        getAll() {
          const request = { result: [...data.values()], onsuccess: null };
          queueMicrotask(() => request.onsuccess?.());
          return request;
        },
      });
      queueMicrotask(() => queueMicrotask(() => transaction.oncomplete?.()));
      return transaction;
    },
  };
  connections.push(db);
  return db;
}
globalThis.indexedDB = {
  open() {
    const request = { result: makeDb(), onsuccess: null, onerror: null, onupgradeneeded: null };
    queueMicrotask(() => request.onsuccess?.());
    return request;
  },
};

const { getCachedTrack, listCachedTracks } = await import("../src/services/localCache.js");

test("a connection WebKit closed silently is reopened for the next transaction", async () => {
  data.set("k", { key: "k", touched_at: 1 });
  assert.deepEqual(await getCachedTrack("k"), { key: "k", touched_at: 1 });
  assert.equal(connections.length, 1);

  connections[0].closed = true; // closed without a close event
  assert.deepEqual(await getCachedTrack("k"), { key: "k", touched_at: 1 });
  assert.equal(connections.length, 2);
  assert.equal((await listCachedTracks()).length, 1);
  assert.equal(connections.length, 2);
});

test("a close event drops the cached connection", async () => {
  const before = connections.length;
  const current = connections.at(-1);
  current.closed = true;
  current.onclose?.();
  assert.equal((await listCachedTracks()).length, 1);
  assert.equal(connections.length, before + 1);
});
