// In-memory state for one capture session (one Wall, one Homeowner visit).
//
// Later slices extend this shape: marker detections, the Rectified Wall
// Image, Obstructions, the Fit Verdict. This slice only carries the
// print-scale correction from the Reference Marker flow step; metric
// computation must multiply nominal printed dimensions by
// `printScale.correctionFactor` to get real-world millimetres.

function blankState() {
  return {
    // null until the Homeowner has verified their printout's scale.
    printScale: null,
    // Once capture starts, the print scale is locked: every frame of one
    // session must be interpreted with the same correction factor.
    captureStarted: false,
    // Metadata of the latest Rectified Wall Image (issue #3 still path):
    // metric scale plus the marker reprojection residuals that will seed
    // the Error Bound. Pixels stay on the canvas; re-capturing replaces it.
    rectified: null,
  };
}

export const session = blankState();

/**
 * Store the verified print scale for this session. Call only with a
 * measurement that passed plausibility (see print-scale.js). Rejected once
 * capture has started — re-verifying then requires a session reset.
 */
export function recordPrintScale({ measuredMm, nominalMm, correctionFactor }) {
  if (session.captureStarted) {
    return null;
  }
  session.printScale = Object.freeze({ measuredMm, nominalMm, correctionFactor });
  return session.printScale;
}

/** Mark capture as started, locking the print-scale record for the session. */
export function lockForCapture() {
  session.captureStarted = true;
}

/**
 * Store the metric metadata of a freshly rendered Rectified Wall Image.
 * Requires a locked print scale: without the correction factor the mm/px
 * scale would be meaningless.
 */
export function recordRectifiedWallImage({
  widthPx,
  heightPx,
  mmPerPx,
  markerIds,
  residualRmsPx,
  residualMaxPx,
  pointsUsed,
  inliers,
}) {
  if (!session.printScale || !session.captureStarted) {
    return null;
  }
  session.rectified = Object.freeze({
    widthPx,
    heightPx,
    mmPerPx,
    markerIds,
    residualRmsPx,
    residualMaxPx,
    pointsUsed,
    inliers,
  });
  return session.rectified;
}

export function hasVerifiedPrintScale() {
  return session.printScale !== null;
}

/** Reset all session state (e.g. the Homeowner reprints the markers). */
export function resetSession() {
  Object.assign(session, blankState());
}
