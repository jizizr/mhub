import test from "node:test";
import assert from "node:assert/strict";
import { createTabExclusivity } from "../src/core/player/tab-exclusivity.js";

// A same-origin BroadcastChannel: every other member with the name receives.
function channelHub() {
  const members = new Set();
  const sent = [];
  class Channel {
    constructor(name) { this.name = name; this.onmessage = null; members.add(this); }
    postMessage(data) {
      sent.push(data);
      for (const member of [...members]) if (member !== this && member.name === this.name) member.onmessage?.({ data: structuredClone(data) });
    }
    close() { members.delete(this); }
  }
  return { Channel, sent };
}

function tab(Channel, clock) {
  const paused = [];
  const tab = createTabExclusivity({ Channel, now: () => clock.now, onOtherTabStarted: (message) => paused.push(message.tab) });
  return { tab, paused };
}

test("a tab that starts playing pauses another playing tab", () => {
  const { Channel } = channelHub();
  const clock = { now: 1000 };
  const a = tab(Channel, clock), b = tab(Channel, clock);
  a.tab.sync(true);
  clock.now = 2000;
  b.tab.sync(true);
  assert.deepEqual(a.paused, [b.tab.id]);
  assert.deepEqual(b.paused, []);
});

test("automatic song changes keep the intent and announce nothing", () => {
  const { Channel, sent } = channelHub();
  const clock = { now: 1000 };
  const a = tab(Channel, clock), b = tab(Channel, clock);
  b.tab.sync(true);
  clock.now = 2000;
  a.tab.sync(true);
  assert.equal(b.paused.length, 1);
  b.tab.sync(false);
  for (let index = 0; index < 5; index++) { clock.now += 1000; a.tab.sync(true); }
  assert.equal(sent.length, 2, "one announcement per start");
});

test("a tab that does not want playback ignores other tabs", () => {
  const { Channel } = channelHub();
  const clock = { now: 1000 };
  const a = tab(Channel, clock), b = tab(Channel, clock);
  a.tab.sync(false);
  b.tab.sync(true);
  assert.deepEqual(a.paused, []);
});

test("a start announced before this tab's own start does not pause it", () => {
  let channel;
  class Channel { constructor() { channel = this; } postMessage() {} close() {} }
  const clock = { now: 5000 };
  const a = tab(Channel, clock);
  a.tab.sync(true);
  channel.onmessage({ data: { type: "started", tab: "other", at: 4000 } });
  assert.deepEqual(a.paused, [], "an older start (delivered late) loses");
  // Tab ids are UUIDs: "0" sorts before every one of them, "~" after.
  channel.onmessage({ data: { type: "started", tab: "0", at: 5000 } });
  assert.deepEqual(a.paused, [], "a tie with a smaller id loses");
  channel.onmessage({ data: { type: "started", tab: "~", at: 5000 } });
  assert.deepEqual(a.paused, ["~"], "a tie goes to the larger tab id");
  channel.onmessage({ data: { type: "started", tab: a.tab.id, at: 9000 } });
  channel.onmessage({ data: { type: "other", tab: "x", at: 9000 } });
  assert.deepEqual(a.paused, ["~"], "own and unknown messages are ignored");
});

test("browsers without BroadcastChannel play as before", () => {
  const a = createTabExclusivity({ Channel: undefined, onOtherTabStarted: () => assert.fail("never called") });
  a.sync(true); a.sync(false); a.dispose();
  class Broken { constructor() { throw new Error("blocked"); } }
  const b = createTabExclusivity({ Channel: Broken });
  b.sync(true); b.dispose();
});

test("dispose leaves the channel", () => {
  const { Channel } = channelHub();
  const clock = { now: 1000 };
  const a = tab(Channel, clock), b = tab(Channel, clock);
  a.tab.sync(true);
  a.tab.dispose();
  clock.now = 2000;
  b.tab.sync(true);
  assert.deepEqual(a.paused, []);
});
