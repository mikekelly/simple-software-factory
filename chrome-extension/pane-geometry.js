// Where a floating terminal window sits (#491): a content script before
// pane-overlay.js, and plain enough to test in Node (`test/pane-geometry`).
// A rect is `{ x, y, width, height }` in CSS pixels of the viewport.
(() => {
  /// The smallest a window is resized to, unless the viewport is smaller.
  const MIN_WIDTH = 320;
  const MIN_HEIGHT = 200;
  /// How far each further window is set from the one before it.
  const CASCADE = 24;

  const finite = (value, fallback) => (Number.isFinite(value) ? value : fallback);

  /// `rect` kept inside a `viewWidth` x `viewHeight` viewport: no smaller than
  /// the minimum, no larger than the viewport, and wholly on screen.
  function clampRect(rect, viewWidth, viewHeight) {
    const maxWidth = Math.max(0, viewWidth);
    const maxHeight = Math.max(0, viewHeight);
    const width = Math.min(maxWidth, Math.max(MIN_WIDTH, finite(rect?.width, MIN_WIDTH)));
    const height = Math.min(maxHeight, Math.max(MIN_HEIGHT, finite(rect?.height, MIN_HEIGHT)));
    const x = Math.min(maxWidth - width, Math.max(0, finite(rect?.x, 0)));
    const y = Math.min(maxHeight - height, Math.max(0, finite(rect?.y, 0)));
    return { x: Math.round(x), y: Math.round(y), width: Math.round(width), height: Math.round(height) };
  }

  /// Where a new window goes: the remembered rect (or 70% of the viewport,
  /// centred), moved down and right by `open` steps so it does not hide
  /// the windows already open, and kept on screen.
  function placeRect(saved, open, viewWidth, viewHeight) {
    const base =
      saved && Number.isFinite(saved.width) && Number.isFinite(saved.height)
        ? saved
        : {
            width: viewWidth * 0.7,
            height: viewHeight * 0.7,
            x: viewWidth * 0.15,
            y: viewHeight * 0.15,
          };
    const step = CASCADE * Math.max(0, open);
    return clampRect({ ...base, x: base.x + step, y: base.y + step }, viewWidth, viewHeight);
  }

  globalThis.ssfPaneGeometry = { MIN_WIDTH, MIN_HEIGHT, clampRect, placeRect };
})();
