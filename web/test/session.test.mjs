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
  originXMm: -60,
  originYMm: -45,
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

// --- Recorded pan (issue #4 slice) -----------------------------------------

const { recordPanResult } = await import("../session.js");

const PAN = {
  widthPx: 2100,
  heightPx: 400,
  mmPerPx: 2.0,
  originXMm: -140,
  originYMm: -110,
  keyframesUsed: 10,
  truncated: false,
  closureApplied: true,
  closureDiscrepancyMm: 5.5,
  closureResidualMm: 5.5,
  scaleCorrection: 1.0082,
  errorBoundNearMm: 10.1,
  errorBoundFarMm: 29.1,
  errorBoundWorstMm: 29.1,
  linkInliers: [29, 16, 14, 25, 21, 26, 17, 14, 28],
};

test("pan result cannot be recorded before print scale is verified", () => {
  resetSession();
  assert.equal(recordPanResult(PAN), null);
  assert.equal(session.pan, null);
});

test("pan result cannot be recorded before capture starts", () => {
  resetSession();
  recordPrintScale({ measuredMm: 200, nominalMm: 200, correctionFactor: 1 });
  assert.equal(recordPanResult(PAN), null);
});

test("pan result records after scale verification + capture lock, frozen, and re-record replaces it", () => {
  resetSession();
  recordPrintScale({ measuredMm: 188, nominalMm: 200, correctionFactor: 0.94 });
  lockForCapture();
  const stored = recordPanResult(PAN);
  assert.equal(stored, session.pan);
  assert.equal(stored.keyframesUsed, 10);
  assert.equal(stored.closureApplied, true);
  assert.equal(stored.errorBoundFarMm, 29.1);
  assert.ok(Object.isFrozen(stored), "pan record must be immutable");
  assert.ok(Object.isFrozen(stored.linkInliers), "link list must be immutable");
  assert.deepEqual([...stored.linkInliers], PAN.linkInliers);

  const again = recordPanResult({ ...PAN, keyframesUsed: 12, closureApplied: false });
  assert.equal(session.pan, again);
  assert.equal(again.keyframesUsed, 12);
  assert.equal(again.closureApplied, false);
});

test("calibration outcome is recorded frozen; uncalibrated pans get nulls, never a fake focal", () => {
  resetSession();
  recordPrintScale({ measuredMm: 200, nominalMm: 200, correctionFactor: 1 });
  lockForCapture();

  // Calibrated pan: focal + k1 stored as reported by the core.
  const calibrated = recordPanResult({
    ...PAN,
    calibrated: true,
    calibratedFocalPx: 693.7,
    calibratedK1: -0.08,
  });
  assert.equal(calibrated.calibrated, true);
  assert.equal(calibrated.calibratedFocalPx, 693.7);
  assert.equal(calibrated.calibratedK1, -0.08);
  assert.ok(Object.isFrozen(calibrated));

  // Uncalibrated pan: the WASM getters return 0.0 sentinels when the
  // conditioning gates refused — the record must store null, not 0 px.
  const uncal = recordPanResult({
    ...PAN,
    calibrated: false,
    calibratedFocalPx: 0,
    calibratedK1: 0,
  });
  assert.equal(uncal.calibrated, false);
  assert.equal(uncal.calibratedFocalPx, null);
  assert.equal(uncal.calibratedK1, null);
});

test("distortion-only outcome: appliedK1 stored without a focal claim; pinhole pans get null", () => {
  resetSession();
  recordPrintScale({ measuredMm: 200, nominalMm: 200, correctionFactor: 1 });
  lockForCapture();

  // k1 passed its conditioning gate, the focal didn't: distortion corrected,
  // calibrated stays false and the focal fields stay null.
  const partial = recordPanResult({
    ...PAN,
    calibrated: false,
    calibratedFocalPx: 0,
    calibratedK1: 0,
    distortionCorrected: true,
    appliedK1: -0.051,
  });
  assert.equal(partial.calibrated, false);
  assert.equal(partial.calibratedFocalPx, null);
  assert.equal(partial.calibratedK1, null);
  assert.equal(partial.distortionCorrected, true);
  assert.equal(partial.appliedK1, -0.051);

  // Fully pinhole pan: the applied-k1 sentinel 0.0 must become null.
  const pinhole = recordPanResult({
    ...PAN,
    calibrated: false,
    distortionCorrected: false,
    appliedK1: 0,
  });
  assert.equal(pinhole.distortionCorrected, false);
  assert.equal(pinhole.appliedK1, null);
});

test("still record stores the wall-plane origin the bounds confirmation converts through", () => {
  resetSession();
  recordPrintScale({ measuredMm: 200, nominalMm: 200, correctionFactor: 1 });
  lockForCapture();
  const stored = recordRectifiedWallImage(RECT);
  assert.equal(stored.originXMm, -60);
  assert.equal(stored.originYMm, -45);
});

test("pan record coexists with the still rectified record and resets with the session", () => {
  resetSession();
  recordPrintScale({ measuredMm: 200, nominalMm: 200, correctionFactor: 1 });
  lockForCapture();
  recordRectifiedWallImage(RECT);
  recordPanResult(PAN);
  assert.ok(session.rectified, "still record intact");
  assert.ok(session.pan, "pan record present");
  resetSession();
  assert.equal(session.pan, null);
  assert.equal(session.rectified, null);
});

// --- Wall bounds + Floor Line (issue #7 slice) ------------------------------

const { recordWallBounds, clearWallBounds, hasConfirmedWallBounds } = await import(
  "../session.js"
);

// Inside the PAN fixture's extent (x -140..4060 mm, y -110..690 mm): the
// containment cross-check refuses bounds the source image cannot
// substantiate.
const BOUNDS = {
  leftXMm: 60,
  rightXMm: 3180,
  topYMm: -30,
  floorYMm: 620,
  source: "pan",
};

// Inside the RECT (still) fixture's extent (x -60..620 mm, y -45..465 mm).
const STILL_BOUNDS = {
  leftXMm: -20,
  rightXMm: 600,
  topYMm: -30,
  floorYMm: 440,
  source: "still",
};

function startedSession() {
  resetSession();
  recordPrintScale({ measuredMm: 200, nominalMm: 200, correctionFactor: 1 });
  lockForCapture();
}

test("wall bounds cannot be recorded before print scale / capture / an image", () => {
  resetSession();
  assert.equal(recordWallBounds(BOUNDS), null);

  startedSession();
  // No Rectified Wall Image yet: nothing the guides could have been placed on.
  assert.equal(recordWallBounds(BOUNDS), null);
  assert.equal(session.wallBounds, null);
  assert.equal(hasConfirmedWallBounds(), false);
});

test("wall bounds require the record their source names", () => {
  startedSession();
  recordRectifiedWallImage(RECT); // still only
  assert.equal(recordWallBounds(BOUNDS), null, "pan-sourced bounds need a pan record");
  const stored = recordWallBounds(STILL_BOUNDS);
  assert.ok(stored);
  assert.equal(stored.source, "still");
  assert.equal(recordWallBounds({ ...STILL_BOUNDS, source: "elsewhere" }), null);
});

test("wall bounds store frozen metric coordinates with derived width/height", () => {
  startedSession();
  recordPanResult(PAN);
  const stored = recordWallBounds(BOUNDS);
  assert.equal(stored, session.wallBounds);
  assert.ok(Object.isFrozen(stored), "bounds record must be immutable");
  assert.equal(stored.leftXMm, 60);
  assert.equal(stored.floorYMm, 620);
  assert.equal(stored.widthMm, 3120);
  assert.equal(stored.heightMm, 650);
  assert.equal(hasConfirmedWallBounds(), true);
});

test("degenerate or non-finite rectangles are refused", () => {
  startedSession();
  recordPanResult(PAN);
  assert.equal(recordWallBounds({ ...BOUNDS, rightXMm: BOUNDS.leftXMm }), null);
  assert.equal(recordWallBounds({ ...BOUNDS, floorYMm: BOUNDS.topYMm - 1 }), null);
  assert.equal(recordWallBounds({ ...BOUNDS, leftXMm: NaN }), null);
  assert.equal(session.wallBounds, null);
});

test("bounds outside the source image's extent are refused (containment cross-check)", () => {
  startedSession();
  recordPanResult(PAN); // extent: x -140..4060 mm, y -110..690 mm
  assert.equal(recordWallBounds({ ...BOUNDS, floorYMm: 800 }), null, "below the image");
  assert.equal(recordWallBounds({ ...BOUNDS, leftXMm: -200 }), null, "left of the image");
  assert.equal(recordWallBounds({ ...BOUNDS, rightXMm: 4200 }), null, "right of the image");
  assert.equal(recordWallBounds({ ...BOUNDS, topYMm: -150 }), null, "above the image");
  assert.equal(session.wallBounds, null);
  // Exactly on the extent (within the half-pixel rounding slack) is fine.
  assert.ok(recordWallBounds({ ...BOUNDS, leftXMm: -140, rightXMm: 4060, floorYMm: 690 }));
});

test("re-capturing invalidates confirmed bounds: the image they described is gone", () => {
  startedSession();
  recordPanResult(PAN);
  recordWallBounds(BOUNDS);
  assert.ok(session.wallBounds);
  recordPanResult(PAN); // re-recorded pan
  assert.equal(session.wallBounds, null);

  recordRectifiedWallImage(RECT);
  recordWallBounds(STILL_BOUNDS);
  assert.ok(session.wallBounds);
  recordRectifiedWallImage(RECT); // re-captured still
  assert.equal(session.wallBounds, null);
});

test("clearWallBounds drops the record (guide moved after confirm) and reset clears it", () => {
  startedSession();
  recordPanResult(PAN);
  recordWallBounds(BOUNDS);
  clearWallBounds();
  assert.equal(session.wallBounds, null);

  recordWallBounds(BOUNDS);
  resetSession();
  assert.equal(session.wallBounds, null);
});

// --- Obstructions (issue #8 slice) ------------------------------------------

const { recordObstructions, clearObstructions } = await import("../session.js");

// Inside the BOUNDS fixture's rectangle (x 60..3180 mm, y -30..620 mm).
const OBSTRUCTIONS = [
  { leftXMm: 500, topYMm: 100, rightXMm: 1400, bottomYMm: 550, type: "window" },
  { leftXMm: 2000, topYMm: -20, rightXMm: 2060, bottomYMm: 600, type: "pipe" },
];

function confirmedSession() {
  startedSession();
  recordPanResult(PAN);
  recordWallBounds(BOUNDS);
}

test("obstructions cannot be recorded before the wall bounds are confirmed", () => {
  resetSession();
  assert.equal(recordObstructions(OBSTRUCTIONS), null);

  startedSession();
  recordPanResult(PAN);
  // Image present, bounds not confirmed: the outlines would have no datum.
  assert.equal(recordObstructions(OBSTRUCTIONS), null);
  assert.equal(session.obstructions, null);
});

test("obstructions store frozen metric outlines with their types, in order", () => {
  confirmedSession();
  const stored = recordObstructions(OBSTRUCTIONS);
  assert.equal(stored, session.obstructions);
  assert.ok(Object.isFrozen(stored), "list must be immutable");
  assert.equal(stored.length, 2);
  assert.ok(Object.isFrozen(stored[0]), "entries must be immutable");
  assert.equal(stored[0].type, "window");
  assert.equal(stored[0].leftXMm, 500);
  assert.equal(stored[1].type, "pipe");
  assert.equal(stored[1].bottomYMm, 600);
});

test("an empty list is a real record (blank wall), distinct from null", () => {
  confirmedSession();
  const stored = recordObstructions([]);
  assert.ok(stored, "empty list must store");
  assert.equal(stored.length, 0);
  assert.notEqual(session.obstructions, null);
});

test("re-recording replaces the whole list (the UI mirrors every edit)", () => {
  confirmedSession();
  recordObstructions(OBSTRUCTIONS);
  const again = recordObstructions([OBSTRUCTIONS[1]]);
  assert.equal(session.obstructions, again);
  assert.equal(again.length, 1);
  assert.equal(again[0].type, "pipe");
});

test("unknown types, degenerate and non-finite outlines are refused whole-batch", () => {
  confirmedSession();
  recordObstructions(OBSTRUCTIONS);
  const bad = (patch) => [OBSTRUCTIONS[0], { ...OBSTRUCTIONS[1], ...patch }];
  assert.equal(recordObstructions(bad({ type: "skylight" })), null);
  assert.equal(recordObstructions(bad({ rightXMm: 2000 })), null, "zero width");
  assert.equal(recordObstructions(bad({ bottomYMm: -20 })), null, "zero height");
  assert.equal(recordObstructions(bad({ leftXMm: NaN })), null);
  assert.equal(recordObstructions("not a list"), null);
  // The prior good record survives a refused batch untouched.
  assert.equal(session.obstructions.length, 2);
});

test("outlines outside the confirmed bounds are refused (containment cross-check)", () => {
  confirmedSession(); // bounds: x 60..3180, y -30..620 (floor)
  const bad = (patch) => [{ ...OBSTRUCTIONS[0], ...patch }];
  assert.equal(recordObstructions(bad({ leftXMm: 40 })), null, "left of the wall");
  assert.equal(recordObstructions(bad({ rightXMm: 3200 })), null, "right of the wall");
  assert.equal(recordObstructions(bad({ topYMm: -50 })), null, "above the wall");
  assert.equal(recordObstructions(bad({ bottomYMm: 650 })), null, "below the Floor Line");
  // Flush against the bounds (within the half-millimetre slack) is fine.
  assert.ok(
    recordObstructions(bad({ leftXMm: 60, rightXMm: 3180, topYMm: -30, bottomYMm: 620 })),
  );
});

test("re-confirming or un-confirming the bounds clears obstructions: their datum moved", () => {
  confirmedSession();
  recordObstructions(OBSTRUCTIONS);
  assert.ok(session.obstructions);
  recordWallBounds(BOUNDS); // re-confirmed, even with identical numbers
  assert.equal(session.obstructions, null);

  recordObstructions(OBSTRUCTIONS);
  clearWallBounds(); // a guide moved after confirming
  assert.equal(session.obstructions, null);
});

test("re-capturing clears obstructions along with the bounds", () => {
  confirmedSession();
  recordObstructions(OBSTRUCTIONS);
  recordPanResult(PAN); // re-recorded pan
  assert.equal(session.obstructions, null);

  confirmedSession();
  recordObstructions(OBSTRUCTIONS);
  recordRectifiedWallImage(RECT); // re-captured still
  assert.equal(session.obstructions, null);
});

test("clearObstructions and resetSession drop the record", () => {
  confirmedSession();
  recordObstructions(OBSTRUCTIONS);
  clearObstructions();
  assert.equal(session.obstructions, null);
  assert.ok(session.wallBounds, "clearing obstructions must not touch the bounds");

  recordObstructions(OBSTRUCTIONS);
  resetSession();
  assert.equal(session.obstructions, null);
});
