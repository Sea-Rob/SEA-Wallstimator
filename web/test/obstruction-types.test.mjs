// node --test — the Obstruction type -> buffer config (issue #8 slice).
//
// The config is reviewable data, so these tests pin the CONTRACT (shape,
// documented WHYs, the compliance-critical 600 mm entries) rather than
// freezing every value: adding a type or tuning a physical-obstruction
// buffer is a config review, not a test rewrite.

import test from "node:test";
import assert from "node:assert/strict";

import {
  OBSTRUCTION_TYPES,
  DEFAULT_OBSTRUCTION_TYPE,
  isObstructionType,
  bufferMmForType,
} from "../obstruction-types.js";

test("every type carries a label, a sane buffer, and a one-line WHY", () => {
  const entries = Object.entries(OBSTRUCTION_TYPES);
  assert.ok(entries.length >= 6, "window, door, vent, pipe, meter box, other at minimum");
  for (const [type, def] of entries) {
    assert.ok(typeof def.label === "string" && def.label.length > 0, `${type} label`);
    assert.ok(Number.isFinite(def.bufferMm) && def.bufferMm >= 0, `${type} buffer`);
    assert.ok(
      typeof def.why === "string" && def.why.length > 10,
      `${type} must document WHY its buffer is what it is`,
    );
    assert.ok(Object.isFrozen(def), `${type} entry must be immutable`);
  }
  assert.ok(Object.isFrozen(OBSTRUCTION_TYPES), "config must be immutable");
});

test("openings carry the AS/NZS 5139 600 mm buffer; purely physical types carry none", () => {
  // These are the compliance-critical values the issue demands — a config
  // edit that changes them must consciously change this test too.
  assert.equal(OBSTRUCTION_TYPES.window.bufferMm, 600);
  assert.equal(OBSTRUCTION_TYPES.door.bufferMm, 600);
  assert.equal(OBSTRUCTION_TYPES.vent.bufferMm, 600);
  assert.equal(OBSTRUCTION_TYPES.pipe.bufferMm, 0);
  assert.equal(OBSTRUCTION_TYPES.meter_box.bufferMm, 0);
  assert.equal(OBSTRUCTION_TYPES.other.bufferMm, 0);
});

test("type lookup is honest about unknowns", () => {
  assert.equal(isObstructionType("window"), true);
  assert.equal(isObstructionType("skylight"), false);
  // Inherited Object properties must not read as types.
  assert.equal(isObstructionType("toString"), false);
  assert.equal(bufferMmForType("door"), 600);
  assert.equal(bufferMmForType("skylight"), null, "unknown type is null, never 0 mm");
});

test("the default type exists in the config", () => {
  assert.ok(isObstructionType(DEFAULT_OBSTRUCTION_TYPE));
});
