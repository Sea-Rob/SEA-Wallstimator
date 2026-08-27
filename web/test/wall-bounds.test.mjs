// node --test — Wall bounds + Floor Line coordinate math (issue #7 slice).

import test from "node:test";
import assert from "node:assert/strict";

import {
  MAX_ZOOM,
  initialGuides,
  moveGuide,
  fitScale,
  fitTransform,
  imageToView,
  viewToImage,
  clampTransform,
  zoomAt,
  panBy,
  pinchTransform,
  hitGuide,
  guidesToWallMm,
} from "../wall-bounds.js";

const close = (a, b, eps = 1e-9) =>
  assert.ok(Math.abs(a - b) <= eps, `expected ${a} ≈ ${b}`);

// --- Guides -----------------------------------------------------------------

test("initial guides sit at the image extent with the floor line near the bottom", () => {
  const g = initialGuides(2000, 800);
  assert.equal(g.left, 0);
  assert.equal(g.right, 2000);
  assert.equal(g.top, 0);
  assert.ok(g.floor > 800 * 0.75 && g.floor < 800, "floor near but not at the bottom");
});

test("moveGuide clamps to the image extent", () => {
  const g = initialGuides(2000, 800);
  assert.equal(moveGuide(g, "left", -50, 2000, 800, 20).left, 0);
  assert.equal(moveGuide(g, "right", 99999, 2000, 800, 20).right, 2000);
  assert.equal(moveGuide(g, "top", -1, 2000, 800, 20).top, 0);
  assert.equal(moveGuide(g, "floor", 5000, 2000, 800, 20).floor, 800);
});

test("opposing guides can never cross: min gap is enforced on both sides", () => {
  let g = initialGuides(2000, 800);
  g = moveGuide(g, "right", 500, 2000, 800, 20);
  // Left dragged past right stops a gap short.
  g = moveGuide(g, "left", 700, 2000, 800, 20);
  assert.equal(g.left, 480);
  // And right dragged back down onto left stops the same gap short.
  g = moveGuide(g, "right", 100, 2000, 800, 20);
  assert.equal(g.right, 500);

  g = moveGuide(g, "floor", 300, 2000, 800, 20);
  g = moveGuide(g, "top", 750, 2000, 800, 20);
  assert.equal(g.top, 280);
  g = moveGuide(g, "floor", 10, 2000, 800, 20);
  assert.equal(g.floor, 300);
});

test("moveGuide never mutates its input", () => {
  const g = initialGuides(2000, 800);
  const moved = moveGuide(g, "left", 100, 2000, 800, 20);
  assert.equal(g.left, 0);
  assert.equal(moved.left, 100);
});

test("moveGuide refuses an unknown guide name", () => {
  assert.throws(() => moveGuide(initialGuides(10, 10), "bottom", 5, 10, 10, 1));
});

// --- View transform ----------------------------------------------------------

test("fitTransform letterboxes a wide image (pan) and a tall view symmetrically", () => {
  // 2000×500 image into a 1000×600 view: width-limited, scale 0.5.
  const t = fitTransform(2000, 500, 1000, 600);
  close(t.scale, 0.5);
  close(t.tx, 0);
  close(t.ty, (600 - 250) / 2);
  // Corners land inside the view.
  assert.deepEqual(imageToView(t, [0, 0]), [0, 175]);
  assert.deepEqual(imageToView(t, [2000, 500]), [1000, 425]);
});

test("imageToView and viewToImage are inverses", () => {
  const t = { scale: 1.7, tx: -300, ty: 42 };
  const [vx, vy] = imageToView(t, [123.4, 567.8]);
  const [ix, iy] = viewToImage(t, [vx, vy]);
  close(ix, 123.4);
  close(iy, 567.8);
});

test("zoomAt keeps the image point under the cursor fixed", () => {
  const t = fitTransform(2000, 500, 1000, 600);
  const cursor = [700, 300];
  const before = viewToImage(t, cursor);
  const z = zoomAt(t, cursor, 2, 2000, 500, 1000, 600);
  const after = viewToImage(z, cursor);
  close(z.scale, t.scale * 2);
  close(after[0], before[0], 1e-6);
  close(after[1], before[1], 1e-6);
});

test("zoom clamps between fit scale and MAX_ZOOM×fit", () => {
  const t = fitTransform(2000, 500, 1000, 600);
  const fit = fitScale(2000, 500, 1000, 600);
  // Can't zoom out below fit…
  const out = zoomAt(t, [500, 300], 0.01, 2000, 500, 1000, 600);
  close(out.scale, fit);
  // …and zooming out to fit re-centres exactly (no drift accumulates).
  close(out.tx, t.tx, 1e-6);
  close(out.ty, t.ty, 1e-6);
  // Can't zoom in past the ceiling.
  let z = t;
  for (let i = 0; i < 20; i++) z = zoomAt(z, [500, 300], 2, 2000, 500, 1000, 600);
  close(z.scale, fit * MAX_ZOOM);
});

test("panning cannot push an image edge past the view edge", () => {
  // Zoom in first so there is something to pan.
  let t = zoomAt(fitTransform(2000, 500, 1000, 600), [500, 300], 4, 2000, 500, 1000, 600);
  t = panBy(t, 1e9, 1e9, 2000, 500, 1000, 600);
  close(t.tx, 0); // left image edge at the left view edge, no further
  t = panBy(t, -1e9, -1e9, 2000, 500, 1000, 600);
  close(t.tx, 1000 - 2000 * t.scale);
  // The axis where the scaled image is smaller than the view stays centred.
  const small = panBy(fitTransform(2000, 500, 1000, 600), 50, 50, 2000, 500, 1000, 600);
  close(small.ty, (600 - 500 * small.scale) / 2);
});

test("pinch scales by the finger-distance ratio about the midpoint", () => {
  const t = zoomAt(fitTransform(2000, 500, 1000, 600), [500, 300], 2, 2000, 500, 1000, 600);
  const before = [
    [400, 300],
    [600, 300],
  ];
  const after = [
    [300, 300],
    [700, 300],
  ]; // spread ×2, same midpoint
  const imgAtMid = viewToImage(t, [500, 300]);
  const z = pinchTransform(t, before, after, 2000, 500, 1000, 600);
  close(z.scale, t.scale * 2);
  const midNow = imageToView(z, imgAtMid);
  close(midNow[0], 500, 1e-6);
  close(midNow[1], 300, 1e-6);
});

test("pinch with a moving midpoint pans; degenerate pinch (zero spread) only pans", () => {
  const t = zoomAt(fitTransform(2000, 500, 1000, 600), [500, 300], 4, 2000, 500, 1000, 600);
  const imgAtMid = viewToImage(t, [500, 300]);
  const moved = pinchTransform(
    t,
    [
      [450, 300],
      [550, 300],
    ],
    [
      [400, 250],
      [500, 250],
    ],
    2000,
    500,
    1000,
    600,
  );
  close(moved.scale, t.scale);
  const midNow = imageToView(moved, imgAtMid);
  close(midNow[0], 450, 1e-6);
  close(midNow[1], 250, 1e-6);

  const degenerate = pinchTransform(
    t,
    [
      [500, 300],
      [500, 300],
    ],
    [
      [480, 300],
      [480, 300],
    ],
    2000,
    500,
    1000,
    600,
  );
  close(degenerate.scale, t.scale);
});

test("clampTransform recentres an undersized axis and clamps an oversized one", () => {
  const t = clampTransform({ scale: 0.5, tx: 500, ty: -500 }, 2000, 500, 1000, 600);
  close(t.tx, 0); // 2000×0.5 = 1000 = view width: only tx=0 shows it all
  close(t.ty, (600 - 250) / 2); // 250 < 600: centred regardless of input
});

// --- Hit-testing --------------------------------------------------------------

test("hitGuide picks the nearest guide within the slop, in view px", () => {
  const g = { left: 100, right: 1900, top: 50, floor: 700 };
  const t = { scale: 0.5, tx: 0, ty: 0 }; // left at view x=50, floor at view y=350
  assert.equal(hitGuide(g, t, [58, 200], 20), "left");
  assert.equal(hitGuide(g, t, [500, 341], 20), "floor");
  assert.equal(hitGuide(g, t, [500, 200], 20), null);
  // Slop is a view-px constant: at 0.5× scale a 20 view-px slop reaches
  // 40 image px, so zooming out widens the reach in image terms.
  assert.equal(hitGuide(g, t, [500, 44], 20), "top"); // top at view y=25
});

test("hitGuide prefers the floor line on an exact tie with the top edge", () => {
  const g = { left: 0, right: 1000, top: 100, floor: 300 };
  const t = { scale: 1, tx: 0, ty: 0 };
  // Equidistant between top (100) and floor (300).
  assert.equal(hitGuide(g, t, [500, 200], 150), "floor");
});

test("hitGuide near a corner picks the closer of the two guides", () => {
  const g = { left: 100, right: 1900, top: 50, floor: 700 };
  const t = { scale: 1, tx: 0, ty: 0 };
  assert.equal(hitGuide(g, t, [104, 692], 20), "left"); // 4 px vs 8 px
  assert.equal(hitGuide(g, t, [110, 703], 20), "floor"); // 10 px vs 3 px
});

// --- Metric conversion ---------------------------------------------------------

test("guidesToWallMm converts through the image's own scale and origin", () => {
  const g = { left: 100, right: 1600, top: 40, floor: 740 };
  const mm = guidesToWallMm(g, { mmPerPx: 2, originXMm: -140, originYMm: -110 });
  close(mm.leftXMm, -140 + 200);
  close(mm.rightXMm, -140 + 3200);
  close(mm.topYMm, -110 + 80);
  close(mm.floorYMm, -110 + 1480);
  // y grows downward: the Floor Line is BELOW the top edge in wall coords.
  assert.ok(mm.floorYMm > mm.topYMm);
  // Width/height a consumer would derive.
  close(mm.rightXMm - mm.leftXMm, 3000);
  close(mm.floorYMm - mm.topYMm, 1400);
});
