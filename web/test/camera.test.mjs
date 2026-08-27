// node --test — main-lens selection and zoom-lock heuristics (issue #6).

import test from "node:test";
import assert from "node:assert/strict";

import { pickMainRearCamera, zoomLockConstraint } from "../camera.js";

const dev = (deviceId, label, kind = "videoinput") => ({ deviceId, kind, label });

test("iPhone-style labels: plain Back Camera beats ultra wide and telephoto", () => {
  const pick = pickMainRearCamera([
    dev("f1", "Front Camera"),
    dev("b-uw", "Back Ultra Wide Camera"),
    dev("b-main", "Back Camera"),
    dev("b-tele", "Back Telephoto Camera"),
  ]);
  assert.equal(pick.deviceId, "b-main");
});

test('"Back Dual Wide Camera" is acceptable as a main lens (plain "wide" is not auxiliary)', () => {
  const pick = pickMainRearCamera([
    dev("f1", "Front Camera"),
    dev("b-uw", "Back Ultra Wide Camera"),
    dev("b-dw", "Back Dual Wide Camera"),
  ]);
  assert.equal(pick.deviceId, "b-dw");
});

test("Android camera2 labels: lowest rear index wins", () => {
  const pick = pickMainRearCamera([
    dev("c1", "camera2 1, facing front"),
    dev("c3", "camera2 3, facing back"),
    dev("c0", "camera2 0, facing back"),
    dev("c2", "camera2 2, facing back"),
  ]);
  assert.equal(pick.deviceId, "c0");
});

test("unlabelled devices (no permission yet / privacy mode) give no pick", () => {
  assert.equal(
    pickMainRearCamera([dev("a", ""), dev("b", "")]),
    null,
  );
});

test("front-only device lists give no pick, never the front camera", () => {
  assert.equal(
    pickMainRearCamera([dev("f1", "Front Camera"), dev("f2", "camera2 1, facing front")]),
    null,
  );
});

test("all-auxiliary rear labels fall back to the least-bad rear lens", () => {
  const pick = pickMainRearCamera([
    dev("f1", "Front Camera"),
    dev("b-tele", "Back Telephoto Camera"),
    dev("b-uw", "Back Ultra Wide Camera"),
  ]);
  // Still a rear lens (front is never acceptable); shortest label wins.
  assert.equal(pick.deviceId, "b-tele");
});

test("non-videoinput devices are ignored", () => {
  assert.equal(
    pickMainRearCamera([dev("m1", "Back Microphone", "audioinput")]),
    null,
  );
});

test("empty and missing lists give no pick", () => {
  assert.equal(pickMainRearCamera([]), null);
  assert.equal(pickMainRearCamera(undefined), null);
});

test("zoom lock: range containing 1x locks exactly 1.0", () => {
  assert.deepEqual(
    zoomLockConstraint({ zoom: { min: 0.5, max: 8 } }),
    { zoom: 1.0 },
  );
});

test("zoom lock: range not containing 1x clamps to the nearest supported value", () => {
  assert.deepEqual(zoomLockConstraint({ zoom: { min: 2, max: 10 } }), { zoom: 2 });
  assert.deepEqual(zoomLockConstraint({ zoom: { min: 0.25, max: 0.5 } }), { zoom: 0.5 });
});

test("zoom lock: no capabilities, or no zoom capability, means no constraint", () => {
  assert.equal(zoomLockConstraint(undefined), null);
  assert.equal(zoomLockConstraint(null), null);
  assert.equal(zoomLockConstraint({ width: { max: 1920 } }), null);
});

test("zoom lock: malformed zoom ranges are refused rather than guessed at", () => {
  assert.equal(zoomLockConstraint({ zoom: { min: 5, max: 2 } }), null);
  assert.equal(zoomLockConstraint({ zoom: { min: NaN, max: 4 } }), null);
});
