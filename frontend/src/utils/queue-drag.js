// Queue rows remain in place until the gesture commits. Ignoring the source
// row turns the pointer position into the final splice index in either direction.
export function queueDropIndex(rows, sourceIndex, centerY) {
  return rows.reduce((index, row, rowIndex) => index + (
    rowIndex !== sourceIndex && centerY > row.top + row.height / 2 ? 1 : 0
  ), 0);
}

export function queueScrollStep(pointerY, top, bottom, elapsedMs = 16) {
  if (bottom <= top) return 0;
  const edge = Math.min(64, (bottom - top) / 3);
  const strength = pointerY < top + edge
    ? -Math.min(1, Math.max(0, (top + edge - pointerY) / edge))
    : Math.min(1, Math.max(0, (pointerY - (bottom - edge)) / edge));
  return strength * 800 * Math.min(32, Math.max(0, elapsedMs)) / 1000;
}
