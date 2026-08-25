// node --test — print-scale correction math and plausibility bounds.

import test from "node:test";
import assert from "node:assert/strict";

import { evaluateRulerMeasurement, PLAUSIBILITY_TOLERANCE } from "../print-scale.js";
import { session, recordPrintScale, hasVerifiedPrintScale, resetSession } from "../session.js";

const NOMINAL = 200;

test("exact nominal measurement gives correction factor 1", () => {
  const r = evaluateRulerMeasurement(NOMINAL, NOMINAL);
  assert.equal(r.status, "ok");
  assert.equal(r.correctionFactor, 1);
  assert.equal(r.deviationPercent, 0);
});

test('typical "fit to page" shrink (188 mm) is accepted with corrected scale', () => {
  const r = evaluateRulerMeasurement(188, NOMINAL);
  assert.equal(r.status, "ok");
  assert.equal(r.correctionFactor, 0.94);
  assert.ok(Math.abs(r.deviationPercent - -6) < 1e-9);
});

test("plausibility band is inclusive at both ±15% edges", () => {
  assert.equal(evaluateRulerMeasurement(NOMINAL * 0.85, NOMINAL).status, "ok");
  assert.equal(evaluateRulerMeasurement(NOMINAL * 1.15, NOMINAL).status, "ok");
  assert.equal(evaluateRulerMeasurement(NOMINAL * 0.84, NOMINAL).status, "implausible");
  assert.equal(evaluateRulerMeasurement(NOMINAL * 1.16, NOMINAL).status, "implausible");
});

test("centimetre entry (20 for a 200 mm strip) is caught with a cm hint", () => {
  const r = evaluateRulerMeasurement(20, NOMINAL);
  assert.equal(r.status, "implausible");
  assert.match(r.hint, /centimetres/);
  assert.match(r.hint, /200 mm/);
});

test("inch entry (7.9 for a 200 mm strip) is caught with an inch hint", () => {
  const r = evaluateRulerMeasurement(7.9, NOMINAL);
  assert.equal(r.status, "implausible");
  assert.match(r.hint, /inches/);
});

test("wildly wrong entries get no misleading unit hint", () => {
  const r = evaluateRulerMeasurement(1234, NOMINAL);
  assert.equal(r.status, "implausible");
  assert.equal(r.hint, null);
});

test("zero, negative, and non-numeric entries are invalid", () => {
  for (const bad of [0, -5, NaN, Infinity, "200"]) {
    assert.equal(evaluateRulerMeasurement(bad, NOMINAL).status, "invalid", String(bad));
  }
});

test("a broken nominal length is a programming error, not a Homeowner error", () => {
  assert.throws(() => evaluateRulerMeasurement(200, 0));
  assert.throws(() => evaluateRulerMeasurement(200, NaN));
});

test("tolerance constant matches the documented ±15% bound", () => {
  assert.equal(PLAUSIBILITY_TOLERANCE, 0.15);
});

test("session records and resets the print-scale correction", () => {
  resetSession();
  assert.equal(hasVerifiedPrintScale(), false);

  const stored = recordPrintScale({ measuredMm: 188, nominalMm: NOMINAL, correctionFactor: 0.94 });
  assert.equal(hasVerifiedPrintScale(), true);
  assert.equal(session.printScale, stored);
  assert.equal(session.printScale.correctionFactor, 0.94);
  assert.ok(Object.isFrozen(session.printScale), "stored scale must be immutable");

  resetSession();
  assert.equal(session.printScale, null);
});
