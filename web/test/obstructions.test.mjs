// node --test — Obstruction tracing coordinate math (issue #8 slice).

import test from "node:test";
import assert from "node:assert/strict";

import {
  traceRect,
  meetsMinSize,
  moveRectBy,
  resizeRect,
  hitObstruction,
  rectToWallMm,
  rectFromWallMm,
  wallBoundsToImagePx,
  packObstructionsMm,
  unpackZonesMm,
} from "../obstructions.js";

const close = (a, b, eps = 1e-9) =>
  assert.ok(Math.abs(a - b) <= eps, `expected ${a} ≈ ${b}`);

// Wall rectangle in image px used throughout.
const WALL = { left: 100, top: 50, right: 1900, bottom: 950 };

// --- Tracing -----------------------------------------------------------------

test("traceRect normalizes any drag direction into the same rectangle", () => {
  const expected = { left: 300, top: 200, right: 700, bottom: 600 };
  assert.deepEqual(traceRect([300, 200], [700, 600], WALL), expected);
  assert.deepEqual(traceRect([700, 600], [300, 200], WALL), expected);
  assert.deepEqual(traceRect([700, 200], [300, 600], WALL), expected);
});

test("traceRect clamps both corners into the wall rectangle", () => {
  // Finger wandered past the top-left wall corner: outline pins to it.
  assert.deepEqual(traceRect([20, -40], [500, 400], WALL), {
    left: 100,
    top: 50,
    right: 500,
    bottom: 400,
  });
  // And past the floor/right: pins to the Floor Line and right edge.
  assert.deepEqual(traceRect([1800, 900], [5000, 5000], WALL), {
    left: 1800,
    top: 900,
    right: 1900,
    bottom: 950,
  });
});

test("meetsMinSize gates the commit: a tap-sized trace is not an obstruction", () => {
  assert.equal(meetsMinSize({ left: 0, top: 0, right: 30, bottom: 30 }, 30), true);
  assert.equal(meetsMinSize({ left: 0, top: 0, right: 29, bottom: 300 }, 30), false);
  assert.equal(meetsMinSize({ left: 0, top: 0, right: 300, bottom: 29 }, 30), false);
});

// --- Moving ------------------------------------------------------------------

test("moveRectBy translates, preserves size, and stops at the wall edges", () => {
  const r = { left: 300, top: 200, right: 700, bottom: 600 };
  assert.deepEqual(moveRectBy(r, 50, -25, WALL), {
    left: 350,
    top: 175,
    right: 750,
    bottom: 575,
  });
  // Slammed into the top-left corner: clamped flush, size intact.
  const pinned = moveRectBy(r, -1e6, -1e6, WALL);
  assert.deepEqual(pinned, { left: 100, top: 50, right: 500, bottom: 450 });
  // And into the bottom-right (Floor Line included).
  const floor = moveRectBy(r, 1e6, 1e6, WALL);
  assert.deepEqual(floor, { left: 1500, top: 550, right: 1900, bottom: 950 });
});

test("moveRectBy never mutates its input", () => {
  const r = { left: 300, top: 200, right: 700, bottom: 600 };
  moveRectBy(r, 10, 10, WALL);
  assert.deepEqual(r, { left: 300, top: 200, right: 700, bottom: 600 });
});

// --- Resizing ----------------------------------------------------------------

test("resizeRect drags one corner with the opposite corner fixed", () => {
  const r = { left: 300, top: 200, right: 700, bottom: 600 };
  assert.deepEqual(resizeRect(r, "nw", [250, 150], WALL, 40), {
    left: 250,
    top: 150,
    right: 700,
    bottom: 600,
  });
  assert.deepEqual(resizeRect(r, "se", [800, 700], WALL, 40), {
    left: 300,
    top: 200,
    right: 800,
    bottom: 700,
  });
  assert.deepEqual(resizeRect(r, "ne", [750, 100], WALL, 40), {
    left: 300,
    top: 100,
    right: 750,
    bottom: 600,
  });
  assert.deepEqual(resizeRect(r, "sw", [250, 700], WALL, 40), {
    left: 250,
    top: 200,
    right: 700,
    bottom: 700,
  });
});

test("resizeRect clamps to the wall and refuses to cross the opposite corner", () => {
  const r = { left: 300, top: 200, right: 700, bottom: 600 };
  // Dragged far past the opposite corner: stops min-size short of it.
  assert.deepEqual(resizeRect(r, "nw", [5000, 5000], WALL, 40), {
    left: 660,
    top: 560,
    right: 700,
    bottom: 600,
  });
  // Dragged out of the wall: pinned to the wall edges.
  assert.deepEqual(resizeRect(r, "se", [1e6, 1e6], WALL, 40), {
    left: 300,
    top: 200,
    right: 1900,
    bottom: 950,
  });
  assert.deepEqual(resizeRect(r, "nw", [-1e6, -1e6], WALL, 40), {
    left: 100,
    top: 50,
    right: 700,
    bottom: 600,
  });
});

// --- Hit-testing ---------------------------------------------------------------

const RECTS = [
  { left: 200, top: 100, right: 600, bottom: 500 },
  { left: 500, top: 400, right: 900, bottom: 800 }, // overlaps the first
];
const T = { scale: 0.5, tx: 10, ty: 20 };

test("hitObstruction grabs the selected rect's nearest corner handle within the slop", () => {
  // Rect 0's nw corner in view px: (200*0.5+10, 100*0.5+20) = (110, 70).
  assert.deepEqual(hitObstruction(RECTS, 0, T, [115, 74], 20), { index: 0, part: "nw" });
  // se corner at (310, 270).
  assert.deepEqual(hitObstruction(RECTS, 0, T, [305, 275], 20), { index: 0, part: "se" });
  // Handles belong to the SELECTION only: with rect 1 selected, the same
  // point over rect 0's corner is just "inside rect 0".
  assert.deepEqual(hitObstruction(RECTS, 1, T, [115, 74], 20), { index: 0, part: "inside" });
});

test("hitObstruction slop is a view-px constant (finger-sized at any zoom)", () => {
  // 25 view px from rect 0's nw corner: caught at slop 30, missed at 20 —
  // and outside every rect, so the miss is a clean null.
  assert.deepEqual(hitObstruction(RECTS, 0, T, [110, 45], 30), { index: 0, part: "nw" });
  assert.equal(hitObstruction(RECTS, 0, T, [110, 45], 20), null);
});

test("hitObstruction picks the topmost (last-traced) rect where outlines overlap", () => {
  // (550, 450) image px is inside both rects; view = (285, 245).
  assert.deepEqual(hitObstruction(RECTS, -1, T, [285, 245], 20), { index: 1, part: "inside" });
  // A point only inside rect 0.
  assert.deepEqual(hitObstruction(RECTS, -1, T, [125, 90], 20), { index: 0, part: "inside" });
  // A point inside nothing.
  assert.equal(hitObstruction(RECTS, -1, T, [800, 600], 20), null);
});

// --- Metric conversion ----------------------------------------------------------

const META = { mmPerPx: 2, originXMm: -140, originYMm: -110 };

test("rectToWallMm and rectFromWallMm are inverses through the image's scale and origin", () => {
  const rect = { left: 120, top: 80, right: 620, bottom: 480 };
  const mm = rectToWallMm(rect, META);
  assert.deepEqual(mm, { leftXMm: 100, topYMm: 50, rightXMm: 1100, bottomYMm: 850 });
  const back = rectFromWallMm(mm, META);
  close(back.left, rect.left);
  close(back.top, rect.top);
  close(back.right, rect.right);
  close(back.bottom, rect.bottom);
});

test("wallBoundsToImagePx maps the confirmed bounds record into the clamp rectangle", () => {
  const px = wallBoundsToImagePx(
    { leftXMm: 60, rightXMm: 3180, topYMm: -30, floorYMm: 620 },
    META,
  );
  close(px.left, 100);
  close(px.right, 1660);
  close(px.top, 40);
  close(px.bottom, 365); // the Floor Line is the rectangle's bottom
});

// --- Core call packing ------------------------------------------------------------

test("packObstructionsMm lays outlines out flat with the config's buffer per type", () => {
  const { outlines, buffers } = packObstructionsMm(
    [
      { left: 120, top: 80, right: 620, bottom: 480, type: "window" },
      { left: 700, top: 100, right: 730, bottom: 480, type: "pipe" },
    ],
    META,
  );
  assert.deepEqual([...outlines], [100, 50, 1100, 850, 1260, 90, 1320, 850]);
  assert.deepEqual([...buffers], [600, 0], "window buffers 600 mm, pipe none");
});

test("packObstructionsMm throws on a type the config does not know", () => {
  assert.throws(
    () =>
      packObstructionsMm([{ left: 0, top: 0, right: 10, bottom: 10, type: "skylight" }], META),
    /unknown obstruction type/,
  );
});

test("unpackZonesMm splits the core's flat reply back into mm rectangles in order", () => {
  assert.deepEqual(unpackZonesMm(new Float64Array([1, 2, 3, 4, 5, 6, 7, 8])), [
    { leftXMm: 1, topYMm: 2, rightXMm: 3, bottomYMm: 4 },
    { leftXMm: 5, topYMm: 6, rightXMm: 7, bottomYMm: 8 },
  ]);
  assert.deepEqual(unpackZonesMm(new Float64Array([])), []);
});
