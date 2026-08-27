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
    // Result of the latest recorded pan (issue #4): keyframes kept, loop
    // closure status and the session's Error Bound. Pixels stay on the
    // canvas; re-recording replaces it.
    pan: null,
    // Confirmed Wall bounds + Floor Line (issue #7), in metric wall
    // coordinates (mm; y grows downward, matching the Rectified Wall
    // Image). Null until the Homeowner explicitly confirms — later steps
    // (Obstruction tracing, fit checking) are gated on this being set.
    wallBounds: null,
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
  originXMm,
  originYMm,
  markerIds,
  secondMarkerRejected,
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
    // Wall-plane mm coordinate of the image's top-left pixel: with mmPerPx
    // this maps any image pixel to metric wall coordinates (issue #7's
    // bounds confirmation converts through exactly this pair).
    originXMm,
    originYMm,
    markerIds,
    secondMarkerRejected: Boolean(secondMarkerRejected),
    residualRmsPx,
    residualMaxPx,
    pointsUsed,
    inliers,
  });
  // Bounds confirmed on a PREVIOUS image describe pixels that no longer
  // exist — a re-capture always invalidates the confirmation.
  session.wallBounds = null;
  return session.rectified;
}

/**
 * Store the result of a processed recorded pan (issue #4): the full-wall
 * Rectified Wall Image's metric metadata, the keyframes kept, loop-closure
 * status and the session's Error Bound. Requires a locked print scale for
 * the same reason as the still record.
 */
export function recordPanResult({
  widthPx,
  heightPx,
  mmPerPx,
  originXMm,
  originYMm,
  keyframesUsed,
  truncated,
  closureApplied,
  closureRejected,
  closureDiscrepancyMm,
  closureResidualMm,
  scaleCorrection,
  calibrated,
  calibratedFocalPx,
  calibratedK1,
  distortionCorrected,
  appliedK1,
  errorBoundNearMm,
  errorBoundFarMm,
  errorBoundWorstMm,
  errorBoundNearSpanMm,
  errorBoundFullSpanMm,
  linkInliers,
}) {
  if (!session.printScale || !session.captureStarted) {
    return null;
  }
  session.pan = Object.freeze({
    widthPx,
    heightPx,
    mmPerPx,
    originXMm,
    originYMm,
    keyframesUsed,
    truncated: Boolean(truncated),
    closureApplied: Boolean(closureApplied),
    // Marker B was seen but its closure was refused (implausible drift):
    // the result is open-loop and the Homeowner was told to retake.
    closureRejected: Boolean(closureRejected),
    closureDiscrepancyMm,
    closureResidualMm,
    scaleCorrection,
    // Self-calibration outcome (issue #6). `calibrated: false` is an honest
    // fallback and the focal / k1 fields are null then — never a made-up
    // focal. `distortionCorrected` can be true WITHOUT `calibrated`: k1
    // passed its conditioning gate but the focal didn't, so distortion was
    // corrected (`appliedK1`) and no focal is claimed.
    calibrated: Boolean(calibrated),
    calibratedFocalPx: calibrated ? calibratedFocalPx : null,
    calibratedK1: calibrated ? calibratedK1 : null,
    distortionCorrected: Boolean(distortionCorrected),
    appliedK1: distortionCorrected ? appliedK1 : null,
    // Per-position components (see pan.rs: not standalone 95% bounds).
    errorBoundNearMm,
    errorBoundFarMm,
    errorBoundWorstMm,
    // THE 95% distance-bound contract, at two representative spans.
    errorBoundNearSpanMm,
    errorBoundFullSpanMm,
    linkInliers: Object.freeze(Array.from(linkInliers ?? [])),
  });
  // Same invalidation as the still path: a re-recorded pan replaces the
  // Rectified Wall Image the bounds were confirmed against.
  session.wallBounds = null;
  return session.pan;
}

/**
 * Store the Homeowner's explicitly confirmed Wall bounds + Floor Line
 * (issue #7) in metric wall coordinates. `source` names which Rectified
 * Wall Image the guides were placed on ("pan" or "still") and that record
 * must exist — bounds without an image to have been confirmed against are
 * meaningless. Wall y grows downward, so the Floor Line is the rectangle's
 * LARGEST y; a height above the floor is `floorYMm - yMm`. Degenerate
 * rectangles are refused (the UI's clamping should make them impossible;
 * refusing here keeps the session record trustworthy regardless).
 */
export function recordWallBounds({ leftXMm, rightXMm, topYMm, floorYMm, source }) {
  if (!session.printScale || !session.captureStarted) {
    return null;
  }
  const image = source === "pan" ? session.pan : source === "still" ? session.rectified : null;
  if (!image) {
    return null;
  }
  const values = [leftXMm, rightXMm, topYMm, floorYMm];
  if (!values.every(Number.isFinite) || rightXMm <= leftXMm || floorYMm <= topYMm) {
    return null;
  }
  // Containment cross-check against the source image's own extent (review
  // hardening): the UI clamps guides to the image, so bounds outside it can
  // only come from a buggy caller — refuse rather than store coordinates
  // the image cannot substantiate. Half a pixel of slack absorbs rounding.
  const slackMm = image.mmPerPx / 2;
  const maxXMm = image.originXMm + image.widthPx * image.mmPerPx;
  const maxYMm = image.originYMm + image.heightPx * image.mmPerPx;
  if (
    leftXMm < image.originXMm - slackMm ||
    rightXMm > maxXMm + slackMm ||
    topYMm < image.originYMm - slackMm ||
    floorYMm > maxYMm + slackMm
  ) {
    return null;
  }
  session.wallBounds = Object.freeze({
    leftXMm,
    rightXMm,
    topYMm,
    floorYMm,
    // Derived once here so every consumer agrees on them.
    widthMm: rightXMm - leftXMm,
    heightMm: floorYMm - topYMm,
    source,
  });
  return session.wallBounds;
}

/**
 * Drop a confirmed bounds record: the Homeowner moved a guide after
 * confirming, so the stored rectangle no longer matches what is on screen
 * and later steps must re-lock until they re-confirm.
 */
export function clearWallBounds() {
  session.wallBounds = null;
}

export function hasConfirmedWallBounds() {
  return session.wallBounds !== null;
}

export function hasVerifiedPrintScale() {
  return session.printScale !== null;
}

/** Reset all session state (e.g. the Homeowner reprints the markers). */
export function resetSession() {
  Object.assign(session, blankState());
}
