// Print-scale self-verification math (ADR-0002). Pure module, no DOM.
//
// Home printers silently rescale ("fit to page", typically 3-6%), so the
// Homeowner measures the printed ruler strip and the session stores a
// correction factor = measured / nominal that all later metric computation
// consumes. Entries implausibly far from nominal are almost certainly a
// wrong unit (cm, inches) or a broken printout, not a real print scale —
// those are rejected with a re-measure / reprint prompt instead of being
// silently absorbed into the session's geometry.

// Beyond ±15% no home printer scaling is plausible; "fit to page" on A4
// stays well inside this. Within the band, small deviations are ordinary
// measurement noise and print scale — accepted silently.
export const PLAUSIBILITY_TOLERANCE = 0.15;

function withinTolerance(factor) {
  // Tiny epsilon keeps the documented ±15% boundary inclusive despite
  // floating-point noise (|0.85 - 1| evaluates just above 0.15).
  return Math.abs(factor - 1) <= PLAUSIBILITY_TOLERANCE + 1e-9;
}

// When a rejected entry matches the nominal length read in another unit,
// say so: telling the Homeowner *what went wrong* beats a generic error.
function unitHint(measuredMm, nominalMm) {
  if (withinTolerance((measuredMm * 10) / nominalMm)) {
    return `That looks like centimetres — enter the length in millimetres (${measuredMm} cm = ${measuredMm * 10} mm).`;
  }
  if (withinTolerance((measuredMm * 25.4) / nominalMm)) {
    return "That looks like inches — enter the length in millimetres (1 in = 25.4 mm).";
  }
  return null;
}

/**
 * Judge the Homeowner's measurement of the printed ruler strip.
 *
 * @param {number} measuredMm what the Homeowner read off their tape measure
 * @param {number} nominalMm  the strip's nominal printed length (from geometry-core)
 * @returns {{status: "ok", correctionFactor: number, deviationPercent: number}
 *         | {status: "implausible", deviationPercent: number, hint: string | null}
 *         | {status: "invalid"}}
 */
export function evaluateRulerMeasurement(measuredMm, nominalMm) {
  if (!Number.isFinite(nominalMm) || nominalMm <= 0) {
    throw new Error(`nominal ruler length must be a positive number, got ${nominalMm}`);
  }
  if (typeof measuredMm !== "number" || !Number.isFinite(measuredMm) || measuredMm <= 0) {
    return { status: "invalid" };
  }
  const correctionFactor = measuredMm / nominalMm;
  const deviationPercent = (correctionFactor - 1) * 100;
  if (withinTolerance(correctionFactor)) {
    return { status: "ok", correctionFactor, deviationPercent };
  }
  return { status: "implausible", deviationPercent, hint: unitHint(measuredMm, nominalMm) };
}
