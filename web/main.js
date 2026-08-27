// Wallstimator capture page.
//
// Session flow so far (issue #2 slice):
//   1. Print the two-page Reference Marker PDF (served statically, generated
//      by crates/marker-pdf from the geometry-core marker definitions).
//   2. Verify print scale: the Homeowner measures the printed ruler strip;
//      measured / nominal becomes the session's print-scale correction
//      factor (ADR-0002). Implausible entries (wrong units, wrong page
//      scaling) are rejected with a re-measure / reprint prompt.
//   3. Capture: getUserMedia -> canvas pixels -> WASM (geometry-core) ->
//      processed overlay (walking-skeleton pipeline; marker detection lands
//      with issue #3). Gated until print scale is verified.
//
// The page must be served cross-origin isolated (COOP/COEP, see ADR-0001) so
// that SharedArrayBuffer — and with it WASM threads — is available to later
// slices. This slice only asserts the isolation; it does not use threads yet.

import init, {
  FrameProcessor,
  PanRecorder,
  core_version,
  ruler_nominal_mm,
} from "./pkg/geometry_core.js";
import { evaluateRulerMeasurement } from "./print-scale.js";
import {
  session,
  recordPrintScale,
  recordRectifiedWallImage,
  recordPanResult,
  hasVerifiedPrintScale,
  lockForCapture,
} from "./session.js";

// Processing resolution: enough to see edges, cheap enough for a mid-range
// phone on the CPU-only skeleton path (GPU preprocessing comes later).
const PROC_WIDTH = 640;

const statusEl = document.getElementById("status");
const errorEl = document.getElementById("error");
const video = document.getElementById("camera");
const overlay = document.getElementById("overlay");
const overlayCtx = overlay.getContext("2d");

const measuredInput = document.getElementById("measured-mm");
const confirmBtn = document.getElementById("confirm-measure");
const exactBtn = document.getElementById("exact-nominal");
const scaleResult = document.getElementById("scale-result");
const startCaptureBtn = document.getElementById("start-capture");
const captureFrameBtn = document.getElementById("capture-frame");
const recordPanBtn = document.getElementById("record-pan");
const stopPanBtn = document.getElementById("stop-pan");
const panStatus = document.getElementById("pan-status");
const coachEl = document.getElementById("coach");
const errorBoundEl = document.getElementById("error-bound");
const captureResult = document.getElementById("capture-result");
const rectifiedSection = document.getElementById("step-rectified");
const rectifiedCanvas = document.getElementById("rectified");
const rectifiedCtx = rectifiedCanvas.getContext("2d");
const measureResult = document.getElementById("measure-result");
const clearMeasureBtn = document.getElementById("clear-measure");

// Debug / test handle: lets DevTools (and smoke tests) inspect session state.
window.wallstimatorSession = session;

function showError(message) {
  errorEl.textContent = message;
  errorEl.classList.add("visible");
}

function setStatus(parts) {
  statusEl.textContent = parts.join(" · ");
}

// ---------------------------------------------------------------------------
// Step 2 — print-scale self-verification.

function showScaleResult(kind, message) {
  scaleResult.className = `result ${kind}`;
  scaleResult.textContent = message;
}

function handleMeasurement(measuredMm, nominalMm) {
  const verdict = evaluateRulerMeasurement(measuredMm, nominalMm);

  if (verdict.status === "invalid") {
    showScaleResult(
      "warn",
      "Enter the measured length in millimetres as a positive number " +
        `(the strip is nominally ${nominalMm} mm).`,
    );
    return;
  }

  if (verdict.status === "implausible") {
    const hint = verdict.hint
      ? ` ${verdict.hint}`
      : " Re-measure the strip in millimetres, or reprint the PDF at 100% / actual size and measure again.";
    showScaleResult(
      "warn",
      `${measuredMm} mm is ${verdict.deviationPercent.toFixed(0)}% off the printed ` +
        `${nominalMm} mm strip — that is beyond any plausible printer scaling, so it was not stored.` +
        hint,
    );
    return;
  }

  const stored = recordPrintScale({
    measuredMm,
    nominalMm,
    correctionFactor: verdict.correctionFactor,
  });
  if (!stored) {
    showScaleResult(
      "warn",
      "Capture has already started, so this session's print scale is locked. " +
        "Reload the page to start over with a new measurement.",
    );
    return;
  }
  const deviation =
    Math.abs(verdict.deviationPercent) < 0.05
      ? "no print scaling detected"
      : `print scale ${verdict.deviationPercent > 0 ? "+" : ""}${verdict.deviationPercent.toFixed(1)}% corrected`;
  showScaleResult(
    "ok",
    `Print-scale correction factor ${verdict.correctionFactor.toFixed(4)} stored (${deviation}). ` +
      "All measurements this session will use it. You can start the camera.",
  );
  startCaptureBtn.disabled = false;
}

function wireScaleStep(nominalMm) {
  const nominalLabel = Number.isInteger(nominalMm) ? String(nominalMm) : nominalMm.toFixed(1);
  for (const el of document.querySelectorAll(".nominal-mm")) {
    el.textContent = nominalLabel;
  }
  measuredInput.placeholder = `e.g. ${nominalLabel}`;
  confirmBtn.disabled = false;
  exactBtn.disabled = false;

  confirmBtn.addEventListener("click", () => {
    handleMeasurement(Number.parseFloat(measuredInput.value), nominalMm);
  });
  measuredInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") confirmBtn.click();
  });
  exactBtn.addEventListener("click", () => {
    measuredInput.value = nominalLabel;
    handleMeasurement(nominalMm, nominalMm);
  });
}

// ---------------------------------------------------------------------------
// Step 3 — camera capture through the WASM core (walking-skeleton pipeline).

async function openCamera() {
  if (!navigator.mediaDevices?.getUserMedia) {
    throw new Error("This browser does not support camera capture (getUserMedia).");
  }
  // Rear camera preferred: the Homeowner points the phone at the Wall.
  return navigator.mediaDevices.getUserMedia({
    audio: false,
    video: { facingMode: "environment", width: { ideal: 1280 } },
  });
}

async function startCapture(wasm, version, isolated) {
  let stream;
  try {
    stream = await openCamera();
  } catch (err) {
    showError(
      "Camera unavailable: " +
        (err && err.message ? err.message : String(err)) +
        " — allow camera access and try again. The WASM core itself loaded fine.",
    );
    startCaptureBtn.disabled = !hasVerifiedPrintScale();
    return;
  }

  video.srcObject = stream;
  await video.play();
  overlay.classList.add("live");

  // One session, one correction factor: freeze the print-scale record and
  // grey out the step-2 controls once frames are flowing.
  lockForCapture();
  confirmBtn.disabled = true;
  exactBtn.disabled = true;
  measuredInput.disabled = true;

  // Size everything from the actual camera resolution, downscaled for the CPU.
  const scale = Math.min(1, PROC_WIDTH / video.videoWidth);
  const width = Math.max(2, Math.round(video.videoWidth * scale));
  const height = Math.max(2, Math.round(video.videoHeight * scale));
  overlay.width = width;
  overlay.height = height;

  // Off-screen canvas to pull RGBA pixels out of the video element.
  const grab = document.createElement("canvas");
  grab.width = width;
  grab.height = height;
  const grabCtx = grab.getContext("2d", { willReadFrequently: true });

  const processor = new FrameProcessor(width, height);
  const frameLen = processor.frame_len();

  // The render loop below keeps the latest frame in the WASM input buffer,
  // so "Capture frame" simply runs the still path on whatever is there.
  captureFrameBtn.hidden = false;
  captureFrameBtn.addEventListener("click", () => {
    try {
      captureFrame(wasm, processor);
    } catch (err) {
      showCaptureResult(
        "warn",
        "Rectification failed: " + (err && err.message ? err.message : String(err)),
      );
    }
  });

  // Recorded pan (issue #4): frames are fed to the PanRecorder during
  // capture; the core keeps only sharp, well-spaced keyframes, so a long
  // pan never buffers more than the capped keyframe set.
  recordPanBtn.hidden = false;
  recordPanBtn.addEventListener("click", () => {
    try {
      pan.recorder = new PanRecorder(width, height);
    } catch (err) {
      showPanStatus("warn", "Could not start the pan recorder: " + errMsg(err));
      return;
    }
    pan.recording = true;
    pan.lastKept = 0;
    pan.lastCue = -1;
    recordPanBtn.hidden = true;
    captureFrameBtn.disabled = true;
    stopPanBtn.hidden = false;
    showCoach(1); // "Start with Marker A in view" until the core sees it
    showPanStatus("ok", "Recording — pan slowly from Marker A to Marker B. Keyframes kept: 1");
  });
  stopPanBtn.addEventListener("click", () => {
    if (!pan.recorder) return;
    pan.recording = false;
    stopPanBtn.hidden = true;
    hideCoach();
    // Claim the recorder synchronously: a second activation in the paint
    // window below must find null here, not schedule a double-finish.
    const recorder = pan.recorder;
    pan.recorder = null;
    // Retake gate (issue #5): failures surface NOW, with the reason, rather
    // than after a long processing wait that cannot succeed.
    const reason = retakeReason(recorder);
    if (reason) {
      recorder.free();
      recordPanBtn.hidden = false;
      captureFrameBtn.disabled = false;
      showPanStatus("warn", `Please record the pan again — ${reason}`);
      return;
    }
    const kept = recorder.keyframe_count();
    showPanStatus("ok", `Processing ${kept} keyframes (tracking, chaining, loop closure)…`);
    // Let the status paint before the synchronous WASM processing blocks
    // the main thread (single-threaded v1; a worker is a later slice).
    setTimeout(() => {
      try {
        const image = recorder.finish(session.printScale.correctionFactor);
        try {
          showPanResult(wasm, image);
        } finally {
          image.free();
        }
      } catch (err) {
        showPanStatus(
          "warn",
          "Pan processing failed: " + errMsg(err) +
            " You can record another pan or use single-frame capture.",
        );
      } finally {
        recorder.free();
        recordPanBtn.hidden = false;
        captureFrameBtn.disabled = false;
      }
    }, 50);
  });

  let frames = 0;
  let fpsWindowStart = performance.now();

  function renderLoop() {
    // A throw here (e.g. a WASM trap in process()) is outside startCapture's
    // catch; without this guard the overlay would freeze with no visible error.
    try {
      renderFrame();
    } catch (err) {
      showError(
        "Frame processing stopped: " +
          (err && err.message ? err.message : String(err)) +
          " — reload the page.",
      );
      return;
    }
    requestAnimationFrame(renderLoop);
  }

  function renderFrame() {
    grabCtx.drawImage(video, 0, 0, width, height);
    const frame = grabCtx.getImageData(0, 0, width, height);

    // Write the frame straight into WASM memory, process, read the overlay
    // back out. Views are recreated per frame: memory growth detaches them.
    new Uint8Array(wasm.memory.buffer, processor.input_ptr(), frameLen).set(
      frame.data,
    );
    processor.process();

    if (pan.recording && pan.recorder) {
      new Uint8Array(wasm.memory.buffer, pan.recorder.input_ptr(), frameLen).set(
        frame.data,
      );
      pan.recorder.push_frame();
      // Live coaching (issue #5): one prominent line, the core's single
      // highest-priority cue, updated the frame a check trips or clears.
      const cue = pan.recorder.coach_cue();
      if (cue !== pan.lastCue) {
        pan.lastCue = cue;
        showCoach(cue);
      }
      const kept = pan.recorder.keyframe_count();
      if (kept !== pan.lastKept) {
        pan.lastKept = kept;
        showPanStatus(
          "ok",
          `Recording — pan slowly from Marker A to Marker B. Keyframes kept: ${kept}`,
        );
      }
    }
    const out = new Uint8ClampedArray(
      wasm.memory.buffer,
      processor.output_ptr(),
      frameLen,
    );
    overlayCtx.putImageData(new ImageData(out.slice(), width, height), 0, 0);

    frames += 1;
    const now = performance.now();
    if (now - fpsWindowStart >= 1000) {
      const fps = (frames * 1000) / (now - fpsWindowStart);
      const correction = session.printScale
        ? `print scale ×${session.printScale.correctionFactor.toFixed(4)}`
        : "print scale unverified";
      const parts = [
        `geometry-core v${version}`,
        `crossOriginIsolated: ${isolated}`,
        correction,
        `${width}×${height} @ ${fps.toFixed(1)} fps`,
      ];
      if (session.rectified) {
        const quality =
          session.rectified.pointsUsed > 4
            ? `residual RMS ${session.rectified.residualRmsPx.toFixed(2)} px`
            : "exact fit";
        parts.push(
          `rectified ${session.rectified.mmPerPx.toFixed(2)} mm/px, ${quality}`,
        );
      }
      setStatus(parts);
      frames = 0;
      fpsWindowStart = now;
    }
  }
  requestAnimationFrame(renderLoop);
}

// ---------------------------------------------------------------------------
// Step 4 — Rectified Wall Image + two-point measure tool (issue #3).

// State of the currently displayed Rectified Wall Image.
const rectified = {
  imageData: null, // base pixels, redrawn under the measure overlay
  mmPerPx: 0,
  points: [], // up to two [x, y] in canvas px
  // Pan results only: sampled per-position Error Bound component, so each
  // measurement can show its own 95% distance bound. Null for still
  // captures (no Error Bound yet — see issue #17).
  bound: null,
};

/** Conservative bound component (mm) at a canvas x, from the samples. */
function boundComponentAt(canvasX) {
  const b = rectified.bound;
  const wallX = b.originXMm + canvasX * rectified.mmPerPx;
  const t = (wallX - b.originXMm) / b.stepMm;
  const i0 = Math.max(0, Math.min(b.samples.length - 1, Math.floor(t)));
  const i1 = Math.max(0, Math.min(b.samples.length - 1, Math.ceil(t)));
  return Math.max(b.samples[i0], b.samples[i1]);
}

// Recorded-pan state (issue #4).
const pan = {
  recorder: null,
  recording: false,
  lastKept: 0,
  lastCue: -1,
};

// Live-coaching cues (issue #5): the core resolves the single
// highest-priority tripping check (lost marker > too fast > exposure) to
// one code; this page only renders the words. Codes match geometry-core's
// pan::CoachCue.
const COACH_MESSAGES = {
  0: ["ok", "Looking good — keep panning slowly toward Marker B."],
  1: ["warn", "Start with Marker A fully in view."],
  2: ["warn", "No marker seen for a while — keep the markers' line in frame and keep going toward Marker B."],
  3: ["warn", "Slow down — pan more slowly so the frames stay sharp."],
  4: ["warn", "Too dark — turn on a light."],
  5: ["warn", "Too bright — the image is washing out. Avoid aiming at direct light."],
};

function showCoach(cue) {
  const [kind, message] = COACH_MESSAGES[cue] ?? COACH_MESSAGES[0];
  coachEl.className = kind;
  coachEl.textContent = message;
}

function hideCoach() {
  coachEl.className = "";
  coachEl.textContent = "";
}

/**
 * End-of-recording gate (issue #5): a recording that never saw both
 * Reference Markers, whose tracking the core already knows broke mid-pan,
 * or that was mostly blurred/untrackable, gets an immediate retake prompt
 * with the reason INSTEAD of processing. Returns the reason, or null when
 * the recording is worth processing. (A weak tracking segment can still
 * surface only after processing — the core cannot know link quality until
 * full-resolution matching runs.)
 */
function retakeReason(recorder) {
  const aSeen = recorder.marker_a_seen();
  const bSeen = recorder.marker_b_seen();
  if (!aSeen && !bSeen) {
    return "neither Reference Marker was ever seen. Start with Marker A in view and finish with Marker B in view.";
  }
  if (!aSeen) {
    return "Marker A was never seen. Start the pan with Marker A (left end) fully in view.";
  }
  if (!bSeen) {
    return "Marker B was never seen. Keep panning until Marker B (right end) is fully in view before stopping.";
  }
  // The core knows for certain that processing would fail with the same
  // fact: refuse now, not after the wait.
  if (recorder.tracking_lost()) {
    return "tracking was lost mid-pan (the camera moved too fast, or crossed a stretch with nothing to track), so the recording cannot be stitched. Pan again, slowly and steadily.";
  }
  const blurFraction = recorder.blur_fraction();
  if (blurFraction > 0.5) {
    return `most of the recording (${Math.round(blurFraction * 100)}%) was too blurred or fast to track. Pan again, slowly and steadily.`;
  }
  return null;
}

function errMsg(err) {
  return err && err.message ? err.message : String(err);
}

function showPanStatus(kind, message) {
  panStatus.className = `result ${kind}`;
  panStatus.textContent = message;
}

function showCaptureResult(kind, message) {
  captureResult.className = `result ${kind}`;
  captureResult.textContent = message;
}

/** Canvas-pixel coordinates of a pointer event (canvas is CSS-scaled). */
function canvasPoint(canvas, event) {
  const r = canvas.getBoundingClientRect();
  return [
    ((event.clientX - r.left) / r.width) * canvas.width,
    ((event.clientY - r.top) / r.height) * canvas.height,
  ];
}

function drawMeasureOverlay() {
  rectifiedCtx.putImageData(rectified.imageData, 0, 0);
  const px = Math.max(2, rectifiedCanvas.width / 240);
  rectifiedCtx.lineWidth = px / 2;
  rectifiedCtx.strokeStyle = "#ff5252";
  rectifiedCtx.fillStyle = "#ff5252";
  for (const [x, y] of rectified.points) {
    rectifiedCtx.beginPath();
    rectifiedCtx.moveTo(x - 3 * px, y);
    rectifiedCtx.lineTo(x + 3 * px, y);
    rectifiedCtx.moveTo(x, y - 3 * px);
    rectifiedCtx.lineTo(x, y + 3 * px);
    rectifiedCtx.stroke();
  }
  if (rectified.points.length === 2) {
    const [[x1, y1], [x2, y2]] = rectified.points;
    rectifiedCtx.beginPath();
    rectifiedCtx.moveTo(x1, y1);
    rectifiedCtx.lineTo(x2, y2);
    rectifiedCtx.stroke();
  }
}

function updateMeasureReadout() {
  if (rectified.points.length < 2) {
    measureResult.className = "result ok";
    measureResult.textContent =
      rectified.points.length === 0
        ? "Tap the first point on the image."
        : "Tap the second point to measure.";
    return;
  }
  const [[x1, y1], [x2, y2]] = rectified.points;
  const distMm = Math.hypot(x2 - x1, y2 - y1) * rectified.mmPerPx;
  // Pan results carry the sampled bound: this measurement's own 95%
  // distance bound is the sum of its two endpoint components.
  const boundText = rectified.bound
    ? ` ± ${(boundComponentAt(x1) + boundComponentAt(x2)).toFixed(0)} mm (95%)`
    : "";
  measureResult.className = "result ok";
  measureResult.textContent =
    `Distance: ${distMm.toFixed(1)} mm${boundText} (${(distMm / 10).toFixed(1)} cm). ` +
    "Check it against your tape measure. Tap again to restart.";
}

function clearMeasurePoints() {
  rectified.points = [];
  if (rectified.imageData) drawMeasureOverlay();
  updateMeasureReadout();
}

function wireMeasureTool() {
  rectifiedCanvas.addEventListener("pointerdown", (event) => {
    if (!rectified.imageData) return;
    event.preventDefault();
    if (rectified.points.length >= 2) rectified.points = [];
    rectified.points.push(canvasPoint(rectifiedCanvas, event));
    drawMeasureOverlay();
    updateMeasureReadout();
    clearMeasureBtn.hidden = false;
  });
  clearMeasureBtn.addEventListener("click", clearMeasurePoints);
}

/**
 * Display the stitched full-wall Rectified Wall Image from a processed pan
 * and arm the (unchanged) two-point measure tool on it. The Error Bound is
 * shown per wall end — the far end of a chained pan is honestly less
 * certain than the metre around Marker A.
 */
function showPanResult(wasm, image) {
  const width = image.width();
  const height = image.height();
  const pixels = new Uint8ClampedArray(
    wasm.memory.buffer,
    image.pixels_ptr(),
    image.pixels_len(),
  );
  rectifiedCanvas.width = width;
  rectifiedCanvas.height = height;
  rectified.imageData = new ImageData(pixels.slice(), width, height);
  rectified.mmPerPx = image.mm_per_px();
  rectified.points = [];

  const closure = image.closure_applied();
  const closureRejected = image.closure_rejected();
  const keyframes = image.keyframes_used();
  const truncated = image.truncated();
  const linkInliers = Array.from(image.link_inliers());
  const originXMm = image.origin_x_mm();
  const farXMm = image.far_x_mm();

  // Sample the per-position bound component across the rendered extent
  // BEFORE the WASM object is freed, so the measure tool can show every
  // measurement's own distance bound (sum of its two endpoint components;
  // that sum is the validated 95% contract).
  const BOUND_SAMPLES = 256;
  const boundStepMm = (width * rectified.mmPerPx) / (BOUND_SAMPLES - 1);
  const boundSamples = new Float64Array(BOUND_SAMPLES);
  for (let i = 0; i < BOUND_SAMPLES; i++) {
    boundSamples[i] = image.error_bound_mm_at(originXMm + i * boundStepMm);
  }
  rectified.bound = { originXMm, stepMm: boundStepMm, samples: boundSamples };

  const nearSpanMm = image.error_bound_between_mm(0, 300);
  const fullSpanMm = image.error_bound_between_mm(0, farXMm);

  const meta = recordPanResult({
    widthPx: width,
    heightPx: height,
    mmPerPx: rectified.mmPerPx,
    originXMm,
    originYMm: image.origin_y_mm(),
    keyframesUsed: keyframes,
    truncated,
    closureApplied: closure,
    closureRejected,
    closureDiscrepancyMm: image.closure_discrepancy_mm(),
    closureResidualMm: image.closure_residual_mm(),
    scaleCorrection: image.closure_scale_correction(),
    errorBoundNearMm: image.error_bound_near_mm(),
    errorBoundFarMm: image.error_bound_far_mm(),
    errorBoundWorstMm: image.error_bound_worst_mm(),
    errorBoundNearSpanMm: nearSpanMm,
    errorBoundFullSpanMm: fullSpanMm,
    linkInliers,
  });

  drawMeasureOverlay();
  rectifiedSection.hidden = false;
  updateMeasureReadout();

  errorBoundEl.className = "result ok";
  errorBoundEl.textContent =
    `Error Bound (95%, on distances): ±${nearSpanMm.toFixed(0)} mm for a short ` +
    `span near Marker A, ±${fullSpanMm.toFixed(0)} mm across the full wall. ` +
    "Each measurement below shows its own bound. " +
    (closure
      ? `Loop closed against Marker B (drift measured: ` +
        `${meta ? meta.closureDiscrepancyMm.toFixed(1) : "?"} mm, redistributed).`
      : "Marker B could not be used to close the loop, so scale drift is " +
        "bounded by a prior, not a measurement — treat far-end values with care.");
  if (closureRejected) {
    errorBoundEl.className = "result warn";
    errorBoundEl.textContent +=
      " A Marker B sighting had to be REJECTED (implausible drift — usually a " +
      "blurred marker, a rotate-in-place sweep, or a marker off the wall " +
      "plane). Re-record walking parallel to the wall with both markers sharp.";
  }

  const weakest = linkInliers.length ? Math.min(...linkInliers) : 0;
  let summary =
    `Full-wall Rectified Wall Image stitched from ${keyframes} keyframes ` +
    `(${linkInliers.length} tracked links, weakest ${weakest} agreeing points): ` +
    `${width}×${height} px at ${rectified.mmPerPx.toFixed(2)} mm/px. Scroll down to measure.`;
  showPanStatus("ok", summary);
  if (truncated) {
    showPanStatus(
      "warn",
      summary +
        " The keyframe limit was reached before you stopped — the far end of " +
        "the pan may be missing. Consider re-recording with a steadier, shorter sweep.",
    );
  }
  rectifiedSection.scrollIntoView({ behavior: "smooth", block: "nearest" });
}

/**
 * Run the still-frame path on the frame currently in the WASM input buffer:
 * detect Reference Marker(s), estimate the wall homography, render the
 * Rectified Wall Image, and show it with the measure tool armed.
 */
function captureFrame(wasm, processor) {
  const correctionFactor = session.printScale.correctionFactor;
  const image = processor.rectify_captured(correctionFactor);
  if (!image) {
    showCaptureResult(
      "warn",
      "No Reference Marker found in that frame. Get the whole marker (black " +
        "square and white border) sharply in view and capture again.",
    );
    return;
  }
  try {
    const width = image.width();
    const height = image.height();
    const pixels = new Uint8ClampedArray(
      wasm.memory.buffer,
      image.pixels_ptr(),
      image.pixels_len(),
    );
    rectifiedCanvas.width = width;
    rectifiedCanvas.height = height;
    rectified.imageData = new ImageData(pixels.slice(), width, height);
    rectified.mmPerPx = image.mm_per_px();
    rectified.points = [];
    rectified.bound = null; // stills have no Error Bound yet (issue #17)

    const markerIds = Array.from(image.marker_ids());
    const secondMarkerRejected = image.second_marker_rejected();
    const meta = recordRectifiedWallImage({
      widthPx: width,
      heightPx: height,
      mmPerPx: rectified.mmPerPx,
      markerIds,
      secondMarkerRejected,
      residualRmsPx: image.residual_rms_px(),
      residualMaxPx: image.residual_max_px(),
      pointsUsed: image.points_used(),
      inliers: image.inliers(),
    });

    drawMeasureOverlay();
    rectifiedSection.hidden = false;
    updateMeasureReadout();
    // A single still has no chained Error Bound; don't leave a stale one up.
    errorBoundEl.className = "result";
    errorBoundEl.textContent = "";
    const markerNames = markerIds.map((id) => (id === 0 ? "A" : "B")).join(" + ");
    // With one marker the 4-point fit is exact by construction: a residual
    // of 0.00 px says nothing about capture quality, so don't present it as
    // if it did.
    const quality =
      meta.pointsUsed > 4
        ? `corner reprojection RMS ${meta.residualRmsPx.toFixed(2)} px ` +
          `(max ${meta.residualMaxPx.toFixed(2)} px, ${meta.inliers}/${meta.pointsUsed} corners)`
        : "single-marker exact fit (4 corners; no redundancy to score quality)";
    showCaptureResult(
      "ok",
      `Rectified Wall Image rendered from marker ${markerNames}: ` +
        `${width}×${height} px at ${rectified.mmPerPx.toFixed(2)} mm/px, ${quality}. ` +
        "Scroll down to measure.",
    );
    if (secondMarkerRejected) {
      showCaptureResult(
        "warn",
        "A second Reference Marker was visible but could not be used " +
          "(poor detection or inconsistent fit) — measurements far from " +
          `marker ${markerNames} are less accurate. Re-capture with both ` +
          "markers sharp and fully in view if possible.",
      );
    }
    rectifiedSection.scrollIntoView({ behavior: "smooth", block: "nearest" });
  } finally {
    image.free(); // pixels were copied out; release the WASM-side buffer
  }
}

// ---------------------------------------------------------------------------

async function main() {
  const isolated = self.crossOriginIsolated === true;
  const wasm = await init();
  const version = core_version();
  setStatus([
    `geometry-core v${version} (WASM loaded)`,
    `crossOriginIsolated: ${isolated}`,
  ]);
  if (!isolated) {
    showError(
      "This page is not cross-origin isolated (COOP/COEP headers missing). " +
        "WASM threads will be unavailable — check the server configuration.",
    );
  }

  // Nominal ruler length comes from geometry-core: the same source of truth
  // the PDF was generated from, so page copy can never drift from the print.
  wireScaleStep(ruler_nominal_mm());
  wireMeasureTool();

  startCaptureBtn.addEventListener("click", () => {
    if (!hasVerifiedPrintScale()) return; // button is disabled anyway
    startCaptureBtn.disabled = true;
    startCapture(wasm, version, isolated);
  });
}

main().catch((err) => {
  showError("Failed to start: " + (err && err.message ? err.message : String(err)));
});
