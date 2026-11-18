/**
 * One MHub tab plays at a time in a browser. A tab announces when it starts
 * wanting playback (a play press, a system command, a new queue), and every
 * other tab that wants playback pauses. Intent stays on across automatic song
 * changes, so a tab moving on to its next song never interrupts another one.
 * When two tabs start together the later start wins, then the larger tab id,
 * so both cannot pause each other.
 */
export function createTabExclusivity({
  onOtherTabStarted = () => {},
  Channel = globalThis.BroadcastChannel,
  now = () => Date.now(),
  name = "mhub-playback",
} = {}) {
  let channel = null;
  try { channel = typeof Channel === "function" ? new Channel(name) : null; } catch { channel = null; }
  const id = globalThis.crypto?.randomUUID?.() || `${now()}-${Math.random().toString(16).slice(2)}`;
  let wanted = false;
  let startedAt = 0;

  if (channel) {
    channel.onmessage = (event) => {
      const message = event?.data;
      if (!wanted || message?.type !== "started" || !message.tab || message.tab === id) return;
      const at = Number(message.at) || 0;
      if (at < startedAt || (at === startedAt && message.tab < id)) return;
      try { onOtherTabStarted(message); } catch { /* the other tab keeps playing */ }
    };
  }

  return {
    get id() { return id; },
    /** Report whether this tab wants playback; a rising edge is announced. */
    sync(next) {
      const rising = Boolean(next) && !wanted;
      wanted = Boolean(next);
      if (!rising || !channel) return;
      startedAt = now();
      try { channel.postMessage({ type: "started", tab: id, at: startedAt }); } catch { /* closed channel */ }
    },
    dispose() {
      wanted = false;
      try { channel?.close(); } catch { /* already closed */ }
      channel = null;
    },
  };
}
