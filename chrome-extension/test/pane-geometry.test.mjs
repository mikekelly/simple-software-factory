// The floating terminal window's geometry: `node --test chrome-extension/test/`.
import { test } from "node:test";
import assert from "node:assert/strict";
import "../pane-geometry.js";

const { clampRect, placeRect, MIN_WIDTH, MIN_HEIGHT } = globalThis.ssfPaneGeometry;

test("a rect inside the viewport is kept as it is", () => {
  assert.deepEqual(clampRect({ x: 10, y: 20, width: 400, height: 300 }, 1000, 800), {
    x: 10, y: 20, width: 400, height: 300,
  });
});

test("a rect off the edges is moved back on screen", () => {
  assert.deepEqual(clampRect({ x: -50, y: -5, width: 400, height: 300 }, 1000, 800), {
    x: 0, y: 0, width: 400, height: 300,
  });
  assert.deepEqual(clampRect({ x: 900, y: 700, width: 400, height: 300 }, 1000, 800), {
    x: 600, y: 500, width: 400, height: 300,
  });
});

test("a rect is no smaller than the minimum and no larger than the viewport", () => {
  assert.deepEqual(clampRect({ x: 0, y: 0, width: 10, height: 10 }, 1000, 800), {
    x: 0, y: 0, width: MIN_WIDTH, height: MIN_HEIGHT,
  });
  assert.deepEqual(clampRect({ x: 0, y: 0, width: 5000, height: 5000 }, 1000, 800), {
    x: 0, y: 0, width: 1000, height: 800,
  });
  // A viewport smaller than the minimum: the window fills it.
  assert.deepEqual(clampRect({ x: 30, y: 30, width: 400, height: 300 }, 200, 100), {
    x: 0, y: 0, width: 200, height: 100,
  });
});

test("a stored rect that is not a rect falls back to sane numbers", () => {
  assert.deepEqual(clampRect({ x: "a", y: null, width: NaN }, 1000, 800), {
    x: 0, y: 0, width: MIN_WIDTH, height: MIN_HEIGHT,
  });
});

test("a new window takes the remembered rect, or 70% of the viewport centred", () => {
  assert.deepEqual(placeRect(null, 0, 1000, 800), { x: 150, y: 120, width: 700, height: 560 });
  assert.deepEqual(placeRect({ x: 5, y: 6, width: 400, height: 300 }, 0, 1000, 800), {
    x: 5, y: 6, width: 400, height: 300,
  });
});

test("each further window is set down and right of the last, on screen", () => {
  assert.deepEqual(placeRect({ x: 5, y: 6, width: 400, height: 300 }, 2, 1000, 800), {
    x: 53, y: 54, width: 400, height: 300,
  });
  assert.deepEqual(placeRect({ x: 590, y: 490, width: 400, height: 300 }, 3, 1000, 800), {
    x: 600, y: 500, width: 400, height: 300,
  });
});
