// Obstruction type -> Exclusion Zone buffer mapping (issue #8).
//
// REVIEWABLE CONFIG, deliberately not code: every buffer is a compliance
// judgement, so each entry carries a one-line WHY that a reviewer (or SEA's
// back office) can check against the standard without reading any geometry.
// The Rust core computes the zones (crates/geometry-core/src/exclusion.rs);
// it never hard-codes a buffer — this file is the single source of truth
// for what each Obstruction type means.
//
// Key order is the display order of the type picker on the capture page.

export const OBSTRUCTION_TYPES = Object.freeze({
  window: Object.freeze({
    label: "Window",
    bufferMm: 600,
    why: "AS/NZS 5139 requires 600 mm clearance between battery equipment and an openable window (an opening into the building).",
  }),
  door: Object.freeze({
    label: "Door",
    bufferMm: 600,
    why: "AS/NZS 5139 requires 600 mm clearance to a door — an opening AND an egress path.",
  }),
  vent: Object.freeze({
    label: "Vent",
    bufferMm: 600,
    why: "A vent is an opening into the building — AS/NZS 5139 gives it the same 600 mm clearance as windows and doors.",
  }),
  pipe: Object.freeze({
    label: "Pipe",
    bufferMm: 0,
    why: "Purely physical: the product must not overlap it, but no standard mandates a standoff.",
  }),
  meter_box: Object.freeze({
    label: "Meter box",
    bufferMm: 0,
    why: "Purely physical for fit purposes; utility-access clearance is the back office's call on review, not a fit constraint.",
  }),
  other: Object.freeze({
    label: "Other",
    bufferMm: 0,
    why: "Unknown type gets no buffer — the traced outline itself still blocks placement, and the back office sees the label in the Session Artifact.",
  }),
});

/** Type pre-selected in the picker: windows are the most common Obstruction
 *  and the one whose buffer teaches the Homeowner what zones mean. */
export const DEFAULT_OBSTRUCTION_TYPE = "window";

export function isObstructionType(type) {
  return Object.prototype.hasOwnProperty.call(OBSTRUCTION_TYPES, type);
}

/** Compliance buffer (mm) for a type, or null for an unknown type — a null
 *  must surface as an error upstream, never silently become 0 mm. */
export function bufferMmForType(type) {
  return isObstructionType(type) ? OBSTRUCTION_TYPES[type].bufferMm : null;
}
