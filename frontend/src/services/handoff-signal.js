// Pausing the device whose playback another device continued ("在此设备继续").
// The continuing device names the other one in its own playback session for
// a short while; the named device pauses once when it sees that session.

export const HANDOFF_SIGNAL_MS = 2 * 60 * 1000;

/** A mark for this device's sessions while it continues `fromDeviceId`. */
export function createHandoffMark(fromDeviceId, id, now = Date.now()) {
  if (!fromDeviceId || !id) return null;
  return { from: String(fromDeviceId), id: String(id), until: now + HANDOFF_SIGNAL_MS };
}

/** The `handoff` field of this device's playback session, if any. */
export function handoffField(mark, now = Date.now()) {
  return mark && now < mark.until ? { handoff: { from: mark.from, id: mark.id } } : {};
}

/**
 * Whether `session` (another device's) took over this device's playback and
 * this device must stop now. Each signal is spent the first time it is seen,
 * playing or not, so playing here again later is never interrupted. A signal
 * read long after it was sent (this page slept through it) is ignored.
 */
export function takeoverFrom(session, { ownDeviceId, handled, wantsPlay, now = Date.now() }) {
  const signal = session?.handoff;
  if (!signal?.id || !ownDeviceId || signal.from !== ownDeviceId || handled.has(signal.id)) return null;
  handled.add(signal.id);
  const age = Math.abs(now / 1000 - (Number(session.updated_at) || 0)) * 1000;
  if (age > HANDOFF_SIGNAL_MS || !wantsPlay) return null;
  return { id: signal.id, device_name: session.device_name || "另一台设备" };
}
