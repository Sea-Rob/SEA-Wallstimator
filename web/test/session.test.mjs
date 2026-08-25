// node --test — session state for the Rectified Wall Image (issue #3 slice).

import test from "node:test";
import assert from "node:assert/strict";

import {
  session,
  recordPrintScale,
  recordRectifiedWallImage,
  lockForCapture,
  resetSession,
} from "../session.js";

const RECT = {
  widthPx: 800,
  heightPx: 600,
  mmPerPx: 0.85,
  markerIds: [0],
  residualRmsPx: 0.21,
  residualMaxPx: 0.4,
  pointsUsed: 4,
  inliers: 4,
};

test("rectified image cannot be recorded before print scale is verified", () => {
  resetSession();
  assert.equal(recordRectifiedWallImage(RECT), null);
  assert.equal(session.rectified, null);
});

test("rectified image cannot be recorded before capture starts", () => {
  resetSession();
  recordPrintScale({ measuredMm: 200, nominalMm: 200, correctionFactor: 1 });
  assert.equal(recordRectifiedWallImage(RECT), null);
});

test("rectified image records after scale verification + capture lock, and re-capture replaces it", () => {
  resetSession();
  recordPrintScale({ measuredMm: 188, nominalMm: 200, correctionFactor: 0.94 });
  lockForCapture();
  const stored = recordRectifiedWallImage(RECT);
  assert.equal(stored, session.rectified);
  assert.equal(stored.mmPerPx, 0.85);
  assert.deepEqual(stored.markerIds, [0]);
  assert.ok(Object.isFrozen(stored), "record must be immutable");

  const again = recordRectifiedWallImage({ ...RECT, mmPerPx: 0.9, markerIds: [0, 1] });
  assert.equal(session.rectified, again);
  assert.equal(again.mmPerPx, 0.9);
});

test("resetSession clears the rectified record", () => {
  resetSession();
  recordPrintScale({ measuredMm: 200, nominalMm: 200, correctionFactor: 1 });
  lockForCapture();
  recordRectifiedWallImage(RECT);
  resetSession();
  assert.equal(session.rectified, null);
});
