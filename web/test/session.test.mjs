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
