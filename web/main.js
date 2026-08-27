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
  exclusion_zones_mm,
  ruler_nominal_mm,
} from "./pkg/geometry_core.js";
import { evaluateRulerMeasurement } from "./print-scale.js";
import { pickMainRearCamera, zoomLockConstraint } from "./camera.js";
import {
  fitTransform,
  imageToView,
  viewToImage,
  clampTransform,
  zoomAt,
  panBy,
  pinchTransform,
} from "./view-transform.js";
import { initialGuides, moveGuide, hitGuide, guidesToWallMm } from "./wall-bounds.js";
import { OBSTRUCTION_TYPES, DEFAULT_OBSTRUCTION_TYPE } from "./obstruction-types.js";
import {
  traceRect,
  meetsMinSize,
  moveRectBy,
  resizeRect,
  hitObstruction,
  rectToWallMm,
  rectFromWallMm,
  wallBoundsToImagePx,
  packObstructionsMm,
  unpackZonesMm,
} from "./obstructions.js";
import {
  session,
  recordPrintScale,
  recordRectifiedWallImage,
  recordPanResult,
  recordWallBounds,
  clearWallBounds,
  recordObstructions,
  hasConfirmedWallBounds,
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
const boundsSection = document.getElementById("step-bounds");
const boundsCanvas = document.getElementById("bounds-canvas");
const boundsCtx = boundsCanvas.getContext("2d");
const confirmBoundsBtn = document.getElementById("confirm-bounds");
const resetGuidesBtn = document.getElementById("reset-guides");
const boundsSummary = document.getElementById("bounds-summary");
const boundsGate = document.getElementById("bounds-gate");
const obstSection = document.getElementById("step-obstructions");
const obstCanvas = document.getElementById("obstructions-canvas");
const obstCtx = obstCanvas.getContext("2d");
const obstTypeSelect = document.getElementById("obstruction-type");
const traceObstructionBtn = document.getElementById("trace-obstruction");
const deleteObstructionBtn = document.getElementById("delete-obstruction");
const obstSummary = document.getElementById("obstructions-summary");

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

// Lens state for the status line (issue #6): which camera the capture is
// actually using and whether zoom could be locked. Every path is honest —
// "default rear lens" and "zoom not lockable" are reported, not hidden.
// Reset at every openCamera so a retried capture never inherits a previous
// camera's claims.
const lens = { label: "default rear lens", zoom: "zoom not lockable in this browser" };

async function openCamera() {
  lens.label = "default rear lens";
  lens.zoom = "zoom not lockable in this browser";
  if (!navigator.mediaDevices?.getUserMedia) {
    throw new Error("This browser does not support camera capture (getUserMedia).");
  }
  // Rear camera preferred: the Homeowner points the phone at the Wall.
  // Device labels are only exposed after a permission grant, so this first
  // request doubles as the grant; the main-lens pick below can then read
  // the labels and switch if the browser handed over an auxiliary lens.
  let stream = await navigator.mediaDevices.getUserMedia({
    audio: false,
    video: { facingMode: "environment", width: { ideal: 1280 } },
  });

  // Main (1x) lens preference (issue #6): multi-lens phones sometimes
  // satisfy facingMode with the ultra-wide, whose distortion the geometry
  // core should not have to fight. Where labels identify the main rear
  // lens, switch to it by deviceId; where they don't (iOS's single virtual
  // "Back Camera", unlabelled/privacy browsers), keep what we got.
  try {
    const devices = (await navigator.mediaDevices.enumerateDevices?.()) ?? [];
    const pick = pickMainRearCamera(devices);
    const currentId = stream.getVideoTracks()[0]?.getSettings?.().deviceId;
    if (pick && pick.deviceId !== currentId) {
      // Stop the facingMode stream BEFORE opening the pick: many Android
      // camera HALs cannot hold two physical cameras open at once, and a
      // concurrent open would fail with NotReadableError — leaving us on
      // exactly the auxiliary lens this switch exists to escape.
      for (const t of stream.getTracks()) t.stop();
      try {
        stream = await navigator.mediaDevices.getUserMedia({
          audio: false,
          video: { deviceId: { exact: pick.deviceId }, width: { ideal: 1280 } },
        });
        lens.label = `main lens (${pick.label})`;
      } catch {
        // The pick was refused (device busy/detached): reopen the original
        // facingMode stream (already stopped above) and say so.
        stream = await navigator.mediaDevices.getUserMedia({
          audio: false,
          video: { facingMode: "environment", width: { ideal: 1280 } },
        });
        lens.label = "default rear lens (main-lens switch failed)";
      }
    } else if (pick) {
      lens.label = `main lens (${pick.label})`;
    }
  } catch {
    // enumerateDevices unavailable: facingMode was the best possible ask.
  }

  // Zoom lock at 1x where the browser exposes zoom control (Chrome on
  // Android, mostly). Elsewhere the status line reports it honestly; the
  // self-calibration downstream estimates the true focal either way.
  const track = stream.getVideoTracks()[0];
  const lock = zoomLockConstraint(track?.getCapabilities?.());
  if (lock) {
    try {
      // `advanced` constraint sets are best-effort per the spec — an
      // unsatisfiable set is dropped WITHOUT rejection — so claiming the
      // lock requires reading the setting back, not just not-throwing.
      await track.applyConstraints({ advanced: [lock] });
      const applied = track.getSettings?.().zoom;
      lens.zoom =
        typeof applied === "number" && Math.abs(applied - lock.zoom) < 0.01
          ? `zoom locked ${lock.zoom}×`
          : "zoom lock not honoured by the camera";
    } catch {
      lens.zoom = "zoom lock refused by the camera";
    }
  }
  return stream;
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
      // Calibration state (issue #6): focal + k1 once a processed pan's
      // self-calibration passed its conditioning gates; "distortion
      // corrected" when only k1 proved itself; an explicit "uncalibrated"
      // otherwise — never an invented focal.
      const calibration = !session.pan
        ? "no pan yet"
        : session.pan.calibrated
          ? `calibrated f ${session.pan.calibratedFocalPx.toFixed(0)} px, ` +
            `k1 ${session.pan.calibratedK1.toFixed(3)}`
          : session.pan.distortionCorrected
            ? `distortion corrected (k1 ${session.pan.appliedK1.toFixed(3)}), ` +
              "focal unclaimed"
            : "uncalibrated";
      const parts = [
        `geometry-core v${version}`,
        `crossOriginIsolated: ${isolated}`,
        correction,
        `${lens.label}, ${lens.zoom}`,
        calibration,
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

// ---------------------------------------------------------------------------
// Step 5 — Wall bounds + Floor Line confirmation (issue #7).
//
// The Homeowner drags three edge guides and the Floor Line on the Rectified
// Wall Image, then explicitly confirms; the confirmed rectangle is stored in
// METRIC wall coordinates (session.wallBounds) and gates every later step
// (Obstruction tracing, fit checking). All coordinate math lives in the pure
// wall-bounds.js module — this block only routes pointer events and draws.

// Finger-sized grab distance for a guide or an Obstruction handle, in CSS
// pixels: measured in view space so zooming in refines placement without
// shrinking the target.
const HANDLE_SLOP_CSS_PX = 26;

// Guide clamping guardrail, NOT a product judgement: guides can't be pushed
// into a rectangle smaller than this, so a confirmed wall always has
// meaningfully positive area even after a stray drag.
const MIN_WALL_DIMENSION_MM = 100;

const bounds = {
  meta: null, // {widthPx, heightPx, mmPerPx, originXMm, originYMm, source}
  image: null, // offscreen canvas holding the rectified pixels
  guides: null, // image-px guide positions (wall-bounds.js shape)
  view: null, // zoom/pan transform image->view (view-transform.js shape)
  minGapPx: 0,
  confirmed: false,
  // The Floor Line pre-placement is a guess; Confirm stays disabled until
  // the Homeowner has grabbed the floor guide at least once.
  floorEngaged: false,
};

/** Match a canvas' backing store to its CSS box (times devicePixelRatio)
 *  so lines stay crisp; the view transform works in backing px. */
function sizeImageCanvas(canvas) {
  const rect = canvas.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  const w = Math.max(2, Math.round(rect.width * dpr));
  const h = Math.max(2, Math.round(rect.height * dpr));
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
  }
}

/** Backing pixels per CSS pixel — sizes strokes/handles/slop finger-true. */
function canvasUnit(canvas) {
  const rect = canvas.getBoundingClientRect();
  return rect.width > 0 ? canvas.width / rect.width : 1;
}

const sizeBoundsCanvas = () => sizeImageCanvas(boundsCanvas);
const boundsUnit = () => canvasUnit(boundsCanvas);

// ---------------------------------------------------------------------------
// Shared gesture wiring for the zoomable Rectified-Wall-Image canvases
// (step 5 bounds, step 6 obstructions): pointer bookkeeping, two-finger
// pinch-zoom, one-finger pan fallback, wheel zoom, and canvas/view upkeep on
// window resize — extracted from the issue #7 bounds step so both steps
// share one gesture feel instead of two drifting copies. The step-specific
// single-pointer behaviour (dragging a guide, tracing an outline) plugs in
// via `h.beginSingle`, which either claims the pointer with a drag handler
// or declines (null) to get the default image pan.
//
//   h.enabled()             — step armed with an image
//   h.imageSize()           — [w, h] image px
//   h.getView() / h.setView(t)
//   h.sizeCanvas()          — match the backing store to the CSS box
//   h.draw()
//   h.beginSingle(p)        — {move(p), end()?, cancel()?} | null (view px);
//                             `end` fires on release, `cancel` when a second
//                             finger converts the gesture into a pinch.
//   h.tap(p)                — optional: a single pointer went down and up
//                             without claiming a drag or really panning.

/** A pan that moved less than this (CSS px) was a tap, not a pan. */
const TAP_SLOP_CSS_PX = 8;

function wireImageGestures(canvas, h) {
  const pointers = new Map(); // active pointerId -> [x, y] view px
  let mode = null; // {type:"drag", drag} | {type:"pan", moved} | {type:"pinch"}

  canvas.addEventListener("pointerdown", (event) => {
    if (!h.enabled()) return;
    event.preventDefault();
    try {
      canvas.setPointerCapture(event.pointerId);
    } catch {
      // No capturable pointer (synthetic events, some stylus edge cases):
      // the drag still works, it just loses the off-canvas grace capture.
    }
    pointers.set(event.pointerId, canvasPoint(canvas, event));
    if (pointers.size === 2) {
      // Second finger always switches to pinch — even mid guide-drag or
      // mid-trace: the Homeowner is asking for precision, not a bigger
      // drag. A claimed drag gets told (cancel), never silently dropped.
      if (mode?.type === "drag") mode.drag.cancel?.();
      mode = { type: "pinch" };
    } else if (pointers.size === 1) {
      const p = pointers.get(event.pointerId);
      const drag = h.beginSingle(p);
      mode = drag ? { type: "drag", drag } : { type: "pan", moved: 0 };
    }
  });

  canvas.addEventListener("pointermove", (event) => {
    if (!h.enabled() || !pointers.has(event.pointerId)) return;
    event.preventDefault();
    const p = canvasPoint(canvas, event);
    const prev = pointers.get(event.pointerId);
    const [iw, ih] = h.imageSize();
    const [vw, vh] = [canvas.width, canvas.height];

    if (mode?.type === "pinch" && pointers.size >= 2) {
      const ids = [...pointers.keys()].slice(0, 2);
      if (!ids.includes(event.pointerId)) {
        // Stray 3rd finger: not part of the pinch, but keep its stored
        // position fresh — if a tracked finger lifts, this one may be
        // promoted into the pinch pair and a stale position would inject
        // one frame of garbage delta.
        pointers.set(event.pointerId, p);
        return;
      }
      const before = ids.map((id) => pointers.get(id));
      pointers.set(event.pointerId, p);
      const after = ids.map((id) => pointers.get(id));
      h.setView(pinchTransform(h.getView(), before, after, iw, ih, vw, vh));
      h.draw();
      return;
    }

    pointers.set(event.pointerId, p);
    if (mode?.type === "drag") {
      mode.drag.move(p);
    } else if (mode?.type === "pan") {
      mode.moved += Math.hypot(p[0] - prev[0], p[1] - prev[1]);
      h.setView(panBy(h.getView(), p[0] - prev[0], p[1] - prev[1], iw, ih, vw, vh));
      h.draw();
    }
  });

  const releasePointer = (event, upNotCancel) => {
    if (!pointers.delete(event.pointerId)) return;
    if (pointers.size === 0) {
      const ending = mode;
      mode = null;
      if (ending?.type === "drag") {
        if (upNotCancel) {
          ending.drag.end?.();
        } else {
          ending.drag.cancel?.();
        }
      } else if (
        upNotCancel &&
        ending?.type === "pan" &&
        ending.moved <= TAP_SLOP_CSS_PX * canvasUnit(canvas)
      ) {
        h.tap?.(canvasPoint(canvas, event));
      }
    } else if (pointers.size === 1 && mode?.type === "pinch") {
      // Lifted one pinch finger: keep panning (a pinch is never a tap).
      mode = { type: "pan", moved: Infinity };
    }
  };
  canvas.addEventListener("pointerup", (event) => releasePointer(event, true));
  canvas.addEventListener("pointercancel", (event) => releasePointer(event, false));

  // Desktop precision without touch: wheel zooms about the cursor.
  canvas.addEventListener(
    "wheel",
    (event) => {
      if (!h.enabled()) return;
      event.preventDefault();
      // Horizontal trackpad scrolls deliver deltaY 0 — that is not a zoom
      // request in either direction.
      if (event.deltaY === 0) return;
      const [iw, ih] = h.imageSize();
      h.setView(
        zoomAt(
          h.getView(),
          canvasPoint(canvas, event),
          event.deltaY < 0 ? 1.2 : 1 / 1.2,
          iw,
          ih,
          canvas.width,
          canvas.height,
        ),
      );
      h.draw();
    },
    { passive: false },
  );

  // Rotating the phone / resizing the window reshapes the canvas box: keep
  // the backing store matched and re-clamp the view (overlays are image-px
  // state, so they survive untouched).
  window.addEventListener("resize", () => {
    if (!h.enabled()) return;
    h.sizeCanvas();
    const [iw, ih] = h.imageSize();
    h.setView(clampTransform(h.getView(), iw, ih, canvas.width, canvas.height));
    h.draw();
  });
}

function drawBounds() {
  const W = boundsCanvas.width;
  const H = boundsCanvas.height;
  const t = bounds.view;
  const g = bounds.guides;
  const u = boundsUnit();

  boundsCtx.setTransform(1, 0, 0, 1, 0, 0);
  boundsCtx.fillStyle = "#000";
  boundsCtx.fillRect(0, 0, W, H);
  boundsCtx.setTransform(t.scale, 0, 0, t.scale, t.tx, t.ty);
  boundsCtx.drawImage(bounds.image, 0, 0);
  boundsCtx.setTransform(1, 0, 0, 1, 0, 0);

  // Guide positions in view px. Lines are drawn across the full view (not
  // just the image) so a guide near the letterbox edge stays visible.
  const [l] = imageToView(t, [g.left, 0]);
  const [r] = imageToView(t, [g.right, 0]);
  const [, tp] = imageToView(t, [0, g.top]);
  const [, fl] = imageToView(t, [0, g.floor]);

  // Dim everything outside the bounded rectangle: the kept region reads as
  // "the wall" at a glance, mistakes are obvious.
  boundsCtx.fillStyle = "rgba(0, 0, 0, 0.5)";
  boundsCtx.fillRect(0, 0, l, H);
  boundsCtx.fillRect(r, 0, W - r, H);
  boundsCtx.fillRect(l, 0, r - l, tp);
  boundsCtx.fillRect(l, fl, r - l, H - fl);

  const line = (x1, y1, x2, y2, style, widthPx) => {
    boundsCtx.strokeStyle = style;
    boundsCtx.lineWidth = widthPx;
    boundsCtx.beginPath();
    boundsCtx.moveTo(x1, y1);
    boundsCtx.lineTo(x2, y2);
    boundsCtx.stroke();
  };
  const handle = (x, y, style) => {
    if (x < 0 || x > W || y < 0 || y > H) return; // guide off-view: no grip
    boundsCtx.fillStyle = style;
    boundsCtx.strokeStyle = "#111";
    boundsCtx.lineWidth = 2 * u;
    boundsCtx.beginPath();
    boundsCtx.arc(x, y, 12 * u, 0, 2 * Math.PI);
    boundsCtx.fill();
    boundsCtx.stroke();
  };

  // Wall edges in blue; the Floor Line thicker and amber — it is a different
  // KIND of thing (the vertical datum), not a fourth edge.
  const EDGE = "#5ad1ff";
  const FLOOR = "#fc6";
  line(l, 0, l, H, EDGE, 2 * u);
  line(r, 0, r, H, EDGE, 2 * u);
  line(0, tp, W, tp, EDGE, 2 * u);
  line(0, fl, W, fl, FLOOR, 4 * u);
  handle(l, H / 2, EDGE);
  handle(r, H / 2, EDGE);
  handle(W / 2, tp, EDGE);
  handle(W / 2, fl, FLOOR);
  if (fl >= 0 && fl <= H) {
    boundsCtx.fillStyle = FLOOR;
    boundsCtx.font = `${13 * u}px system-ui, sans-serif`;
    boundsCtx.fillText("Floor Line", W / 2 + 18 * u, fl - 8 * u);
  }
}

/** Live width × height readout while placing, mm readback once confirmed. */
function updateBoundsGate() {
  // The Floor Line pre-placement is a GUESS (90% of image height): the
  // vertical datum for every later height must never be confirmable
  // untouched, so Confirm stays disabled until it has been grabbed at
  // least once (review finding on issue #7).
  confirmBoundsBtn.disabled = !bounds.meta || bounds.confirmed || !bounds.floorEngaged;
  resetGuidesBtn.disabled = !bounds.meta;
  if (!bounds.meta) return;
  if (bounds.confirmed && session.wallBounds) {
    const b = session.wallBounds;
    boundsSummary.className = "result ok";
    boundsSummary.textContent =
      `Confirmed: wall ${b.widthMm.toFixed(0)} mm wide × ` +
      `${b.heightMm.toFixed(0)} mm from the Floor Line to the top edge ` +
      `(stored in wall coordinates from the ${b.source === "pan" ? "recorded pan" : "captured frame"}).`;
    boundsGate.className = "result ok";
    boundsGate.textContent =
      "Wall bounds and Floor Line locked in — trace the Obstructions in the " +
      "step below. Moving any guide re-locks that step (and clears its " +
      "tracings: they were measured against these bounds).";
  } else {
    const mm = guidesToWallMm(bounds.guides, bounds.meta);
    boundsSummary.className = "result ok";
    boundsSummary.textContent =
      `Current guides: ${(mm.rightXMm - mm.leftXMm).toFixed(0)} mm wide × ` +
      `${(mm.floorYMm - mm.topYMm).toFixed(0)} mm from the Floor Line to the top edge.`;
    boundsGate.className = "result warn";
    boundsGate.textContent = bounds.floorEngaged
      ? "Not confirmed yet — the following steps stay locked until you place " +
        "the guides and press Confirm."
      : "Drag the amber Floor Line to where the wall meets the floor — it is " +
        "the datum every height is measured from, so Confirm stays disabled " +
        "until you have placed it.";
  }
}

/** A guide moved: any prior confirmation no longer describes the screen —
 *  and any Obstructions traced against it lose their datum with it. */
function unconfirmBounds() {
  if (!bounds.confirmed) return;
  bounds.confirmed = false;
  clearWallBounds();
  hideObstructionsStep();
}

function confirmBounds() {
  if (!bounds.meta || bounds.confirmed) return;
  const mm = guidesToWallMm(bounds.guides, bounds.meta);
  const stored = recordWallBounds({ ...mm, source: bounds.meta.source });
  if (!stored) {
    // Should be unreachable (guides are clamped, the image record exists),
    // but a silent no-op here would fake a confirmation.
    boundsSummary.className = "result warn";
    boundsSummary.textContent =
      "Could not store the wall bounds — re-capture the wall and try again.";
    return;
  }
  bounds.confirmed = true;
  updateBoundsGate();
  showObstructionsStep();
}

/**
 * Arm step 5 on a freshly rendered Rectified Wall Image (still or pan).
 * Guides reset to the image extent and the confirmation resets with them:
 * bounds confirmed on a previous image describe pixels that no longer
 * exist (session.js already dropped the wallBounds record).
 */
function showBoundsStep(meta) {
  bounds.meta = meta;
  // Offscreen copy of the rectified pixels: cheap redraws under zoom/pan
  // without touching the measure canvas above.
  const img = document.createElement("canvas");
  img.width = meta.widthPx;
  img.height = meta.heightPx;
  img.getContext("2d").putImageData(rectified.imageData, 0, 0);
  bounds.image = img;
  bounds.guides = initialGuides(meta.widthPx, meta.heightPx);
  // The mm guardrail, capped so a small test image can still move guides.
  bounds.minGapPx = Math.min(
    Math.max(4, MIN_WALL_DIMENSION_MM / meta.mmPerPx),
    meta.widthPx / 4,
    meta.heightPx / 4,
  );
  bounds.confirmed = false;
  bounds.floorEngaged = false;
  boundsSection.hidden = false;
  // A fresh image also tears down the obstruction step: session.js already
  // dropped both records, and the pixels its outlines sat on are gone.
  hideObstructionsStep();
  sizeBoundsCanvas();
  bounds.view = fitTransform(meta.widthPx, meta.heightPx, boundsCanvas.width, boundsCanvas.height);
  drawBounds();
  updateBoundsGate();
}

function wireBoundsStep() {
  wireImageGestures(boundsCanvas, {
    enabled: () => bounds.image !== null,
    imageSize: () => [bounds.meta.widthPx, bounds.meta.heightPx],
    getView: () => bounds.view,
    setView: (t) => {
      bounds.view = t;
    },
    sizeCanvas: sizeBoundsCanvas,
    draw: drawBounds,
    beginSingle: (p) => {
      const guide = hitGuide(bounds.guides, bounds.view, p, HANDLE_SLOP_CSS_PX * boundsUnit());
      if (!guide) return null; // not on a guide: the default pan
      if (guide === "floor" && !bounds.floorEngaged) {
        // Grabbing the Floor Line counts as engaging with it (even a grab
        // that ends where it started is a deliberate aim at the datum).
        bounds.floorEngaged = true;
        updateBoundsGate();
      }
      return {
        move: (q) => {
          const [ix, iy] = viewToImage(bounds.view, q);
          const alongX = guide === "left" || guide === "right";
          const moved = moveGuide(
            bounds.guides,
            guide,
            alongX ? ix : iy,
            bounds.meta.widthPx,
            bounds.meta.heightPx,
            bounds.minGapPx,
          );
          // Only a drag that actually changed a value invalidates a prior
          // confirmation: a tap's pixel of jitter, or a drag fully absorbed
          // by clamping, leaves the confirmed record still describing the
          // screen.
          if (moved[guide] !== bounds.guides[guide]) {
            bounds.guides = moved;
            unconfirmBounds();
            drawBounds();
            updateBoundsGate();
          }
        },
      };
    },
  });

  confirmBoundsBtn.addEventListener("click", confirmBounds);
  resetGuidesBtn.addEventListener("click", () => {
    if (!bounds.meta) return;
    bounds.guides = initialGuides(bounds.meta.widthPx, bounds.meta.heightPx);
    bounds.view = fitTransform(bounds.meta.widthPx, bounds.meta.heightPx, boundsCanvas.width, boundsCanvas.height);
    // Back to the guessed defaults — the Floor Line must be re-engaged
    // before Confirm re-enables.
    bounds.floorEngaged = false;
    unconfirmBounds();
    drawBounds();
    updateBoundsGate();
  });
}

// ---------------------------------------------------------------------------
// Step 6 — Obstruction tracing with typed Exclusion Zones (issue #8).
//
// Gated on the explicit wall-bounds confirmation (hasConfirmedWallBounds):
// the Homeowner traces rectangles over Obstructions on the same Rectified
// Wall Image, types each one (window, door, pipe, …), and sees the typed
// compliance buffer as a red hatched Exclusion Zone around the outline —
// 600 mm past a window's edge, nothing past a pipe's. Outlines live in
// image px here; the session record and the zone computation are in metric
// wall coordinates (the Rust core inflates and clips — see
// crates/geometry-core/src/exclusion.rs). All coordinate math lives in the
// pure obstructions.js module; this block only routes pointer events, calls
// the core, and draws.

// Smallest traceable Obstruction side (mm): below this a "rectangle" is
// finger noise, not a wall feature — even a conduit is ~20 mm wide.
const MIN_OBSTRUCTION_MM = 20;

const obst = {
  meta: null, // same shape as bounds.meta
  image: null, // shares bounds.image (same rectified pixels)
  wallPx: null, // confirmed wall bounds as an image-px rect (the clamp region)
  list: [], // [{left, top, right, bottom, type}] image px
  zonesPx: [], // Exclusion Zones from the core, image px, parallel to list
  selected: -1,
  view: null,
  minSizePx: 0,
  traceArmed: false, // "Trace obstruction" pressed; next drag traces
  tracePreview: null, // live rect while the tracing finger is down
};

const sizeObstCanvas = () => sizeImageCanvas(obstCanvas);
const obstUnit = () => canvasUnit(obstCanvas);

/** Diagonal hatching clipped to a rectangle — the Exclusion Zone texture.
 *  Distinct from any solid outline at a glance, and it reads as "keep out"
 *  even where zones overlap each other or their own outline. */
function hatchRect(ctx, x, y, w, h, spacingPx, style, widthPx) {
  ctx.save();
  ctx.beginPath();
  ctx.rect(x, y, w, h);
  ctx.clip();
  ctx.strokeStyle = style;
  ctx.lineWidth = widthPx;
  ctx.beginPath();
  for (let d = -h; d < w; d += spacingPx) {
    ctx.moveTo(x + d, y + h);
    ctx.lineTo(x + d + h, y);
  }
  ctx.stroke();
  ctx.restore();
}

function drawObstructions() {
  const W = obstCanvas.width;
  const H = obstCanvas.height;
  const t = obst.view;
  const u = obstUnit();

  obstCtx.setTransform(1, 0, 0, 1, 0, 0);
  obstCtx.fillStyle = "#000";
  obstCtx.fillRect(0, 0, W, H);
  obstCtx.setTransform(t.scale, 0, 0, t.scale, t.tx, t.ty);
  obstCtx.drawImage(obst.image, 0, 0);
  obstCtx.setTransform(1, 0, 0, 1, 0, 0);

  const viewRect = (r) => {
    const [l, tp] = imageToView(t, [r.left, r.top]);
    const [rr, b] = imageToView(t, [r.right, r.bottom]);
    return [l, tp, rr - l, b - tp];
  };

  // Dim everything outside the confirmed wall bounds: Obstructions can only
  // be traced on the Wall, and the dimming says so without a word.
  const [wl, wt, ww, wh] = viewRect(obst.wallPx);
  obstCtx.fillStyle = "rgba(0, 0, 0, 0.5)";
  obstCtx.fillRect(0, 0, wl, H);
  obstCtx.fillRect(wl + ww, 0, W - wl - ww, H);
  obstCtx.fillRect(wl, 0, ww, wt);
  obstCtx.fillRect(wl, wt + wh, ww, H - wt - wh);

  // Exclusion Zones first (under the outlines): translucent red wash +
  // diagonal hatching + dashed edge — unmistakably a different KIND of
  // overlay from the solid outline it surrounds. A zero-buffer zone sits
  // exactly under its outline and disappears behind it, which is the truth.
  const ZONE = "rgba(255, 82, 82, 0.9)";
  for (const zone of obst.zonesPx) {
    const [x, y, w, h] = viewRect(zone);
    obstCtx.fillStyle = "rgba(255, 82, 82, 0.15)";
    obstCtx.fillRect(x, y, w, h);
    hatchRect(obstCtx, x, y, w, h, 14 * u, "rgba(255, 82, 82, 0.45)", 1.5 * u);
    obstCtx.strokeStyle = ZONE;
    obstCtx.lineWidth = 1.5 * u;
    obstCtx.setLineDash([6 * u, 5 * u]);
    obstCtx.strokeRect(x, y, w, h);
    obstCtx.setLineDash([]);
  }

  // Obstruction outlines: one solid style + a type label; the selected one
  // gets a white stroke and corner handles (the resize affordance).
  const OUTLINE = "#5ad1ff";
  obst.list.forEach((o, i) => {
    const [x, y, w, h] = viewRect(o);
    const isSelected = i === obst.selected;
    obstCtx.fillStyle = "rgba(90, 209, 255, 0.12)";
    obstCtx.fillRect(x, y, w, h);
    obstCtx.strokeStyle = isSelected ? "#fff" : OUTLINE;
    obstCtx.lineWidth = (isSelected ? 3 : 2) * u;
    obstCtx.strokeRect(x, y, w, h);

    const label = OBSTRUCTION_TYPES[o.type].label;
    obstCtx.font = `${12 * u}px system-ui, sans-serif`;
    const pad = 4 * u;
    const tw = obstCtx.measureText(label).width;
    obstCtx.fillStyle = "rgba(0, 0, 0, 0.65)";
    obstCtx.fillRect(x, y, tw + 2 * pad, 18 * u);
    obstCtx.fillStyle = isSelected ? "#fff" : OUTLINE;
    obstCtx.fillText(label, x + pad, y + 13 * u);

    if (isSelected) {
      for (const [cx, cy] of [
        [x, y],
        [x + w, y],
        [x, y + h],
        [x + w, y + h],
      ]) {
        obstCtx.fillStyle = "#fff";
        obstCtx.strokeStyle = "#111";
        obstCtx.lineWidth = 2 * u;
        obstCtx.beginPath();
        obstCtx.arc(cx, cy, 9 * u, 0, 2 * Math.PI);
        obstCtx.fill();
        obstCtx.stroke();
      }
    }
  });

  // Live trace preview: dashed, no label yet — it becomes an Obstruction
  // (and gets its zone) on release, if it is big enough to be real.
  if (obst.tracePreview) {
    const [x, y, w, h] = viewRect(obst.tracePreview);
    obstCtx.strokeStyle = "#fff";
    obstCtx.lineWidth = 2 * u;
    obstCtx.setLineDash([8 * u, 6 * u]);
    obstCtx.strokeRect(x, y, w, h);
    obstCtx.setLineDash([]);
  }
}

function showObstructionsSummary(kind, message) {
  obstSummary.className = `result ${kind}`;
  obstSummary.textContent = message;
}

function updateObstructionControls() {
  deleteObstructionBtn.disabled = obst.selected < 0;
  traceObstructionBtn.textContent = obst.traceArmed
    ? "Now drag on the image…"
    : "Trace obstruction";
  if (obst.selected >= 0) {
    obstTypeSelect.value = obst.list[obst.selected].type;
  }
}

/** One line describing what is stored, e.g. "2 windows, 1 pipe". */
function obstructionCensus() {
  const counts = new Map();
  for (const o of obst.list) {
    counts.set(o.type, (counts.get(o.type) ?? 0) + 1);
  }
  return [...counts]
    .map(([type, n]) => `${n} ${OBSTRUCTION_TYPES[type].label.toLowerCase()}${n > 1 ? "s" : ""}`)
    .join(", ");
}

/**
 * The one mutation funnel: after EVERY edit (trace, move, resize, retype,
 * delete) recompute the Exclusion Zones in the core, mirror the outlines
 * into the session record in metric wall coordinates, and redraw. Zones are
 * never cached across edits, so screen, session and core can't disagree.
 */
function syncObstructions() {
  if (!obst.meta) return;
  const b = session.wallBounds;
  try {
    const { outlines, buffers } = packObstructionsMm(obst.list, obst.meta);
    const wallMm = new Float64Array([b.leftXMm, b.topYMm, b.rightXMm, b.floorYMm]);
    const zones = unpackZonesMm(exclusion_zones_mm(outlines, buffers, wallMm));
    obst.zonesPx = zones.map((z) => rectFromWallMm(z, obst.meta));
  } catch (err) {
    // Should be unreachable (outlines are clamped inside the wall), but a
    // stale zone overlay lying about clearances would be worse than a bang.
    showObstructionsSummary("warn", "Could not compute Exclusion Zones: " + errMsg(err));
    obst.zonesPx = [];
    drawObstructions();
    return;
  }
  const stored = recordObstructions(
    obst.list.map((o) => ({ ...rectToWallMm(o, obst.meta), type: o.type })),
  );
  if (!stored) {
    showObstructionsSummary(
      "warn",
      "Could not store the Obstructions — re-confirm the wall bounds and try again.",
    );
    drawObstructions();
    return;
  }
  drawObstructions();
  updateObstructionControls();
  if (stored.length === 0) {
    showObstructionsSummary(
      "ok",
      "No Obstructions traced yet. Pick a type and press “Trace obstruction”, " +
        "then drag a rectangle over each window, door, pipe, meter box or " +
        "vent on the wall. A blank wall needs nothing here.",
    );
  } else {
    const buffered = stored.filter((o) => OBSTRUCTION_TYPES[o.type].bufferMm > 0).length;
    showObstructionsSummary(
      "ok",
      `${stored.length === 1 ? "1 Obstruction" : `${stored.length} Obstructions`} stored ` +
        `in wall coordinates (${obstructionCensus()}). ` +
        (buffered > 0
          ? "The red hatched Exclusion Zones show the 600 mm clearance " +
            "AS/NZS 5139 requires around openings — the product must stay " +
            "out of those too. "
          : "") +
        "Tap an outline to select it; drag to move, corners to resize.",
    );
  }
}

/**
 * Arm step 6 on the freshly confirmed wall bounds. Always starts empty:
 * every path here means the outlines' datum is new (first confirmation,
 * re-confirmation after a guide moved, or a new capture), and session.js
 * has already dropped any previous record for exactly that reason.
 */
function showObstructionsStep() {
  if (!hasConfirmedWallBounds() || !bounds.meta) return;
  obst.meta = bounds.meta;
  obst.image = bounds.image;
  obst.wallPx = wallBoundsToImagePx(session.wallBounds, obst.meta);
  obst.list = [];
  obst.zonesPx = [];
  obst.selected = -1;
  obst.traceArmed = false;
  obst.tracePreview = null;
  // The mm guardrail, capped so tiny test images can still trace at all.
  obst.minSizePx = Math.min(
    Math.max(4, MIN_OBSTRUCTION_MM / obst.meta.mmPerPx),
    (obst.wallPx.right - obst.wallPx.left) / 4,
    (obst.wallPx.bottom - obst.wallPx.top) / 4,
  );
  obstSection.hidden = false;
  sizeObstCanvas();
  obst.view = fitTransform(
    obst.meta.widthPx,
    obst.meta.heightPx,
    obstCanvas.width,
    obstCanvas.height,
  );
  syncObstructions();
  obstSection.scrollIntoView({ behavior: "smooth", block: "nearest" });
}

/** Tear step 6 down when its gate re-locks (bounds un-confirmed or a new
 *  capture). The session record is already gone — session.js clears it on
 *  every path that gets here — this only retires the on-screen state. */
function hideObstructionsStep() {
  obstSection.hidden = true;
  obst.meta = null;
  obst.image = null;
  obst.wallPx = null;
  obst.list = [];
  obst.zonesPx = [];
  obst.selected = -1;
  obst.traceArmed = false;
  obst.tracePreview = null;
}

function wireObstructionsStep() {
  // The type picker is built from the same reviewable config the buffers
  // come from — the UI can never offer a type the core has no buffer for.
  for (const [type, def] of Object.entries(OBSTRUCTION_TYPES)) {
    const option = document.createElement("option");
    option.value = type;
    option.textContent =
      def.bufferMm > 0 ? `${def.label} (${def.bufferMm} mm zone)` : def.label;
    obstTypeSelect.append(option);
  }
  obstTypeSelect.value = DEFAULT_OBSTRUCTION_TYPE;

  wireImageGestures(obstCanvas, {
    enabled: () => obst.image !== null,
    imageSize: () => [obst.meta.widthPx, obst.meta.heightPx],
    getView: () => obst.view,
    setView: (t) => {
      obst.view = t;
    },
    sizeCanvas: sizeObstCanvas,
    draw: drawObstructions,
    beginSingle: (p) => {
      const start = viewToImage(obst.view, p);

      if (obst.traceArmed) {
        return {
          move: (q) => {
            obst.tracePreview = traceRect(start, viewToImage(obst.view, q), obst.wallPx);
            drawObstructions();
          },
          end: () => {
            const rect = obst.tracePreview ?? traceRect(start, start, obst.wallPx);
            obst.tracePreview = null;
            obst.traceArmed = false;
            if (meetsMinSize(rect, obst.minSizePx)) {
              obst.list.push({ ...rect, type: obstTypeSelect.value });
              // Deliberately NOT auto-selected: the picker retypes the
              // current selection, so auto-selecting would make "trace a
              // window, then pick Pipe for the next trace" silently retype
              // the window — a 600 mm compliance hole from one tap. Retyping
              // is always an explicit tap-then-pick.
              obst.selected = -1;
              syncObstructions();
            } else {
              // A tap or a sliver: honest no-op, and say what to do instead.
              updateObstructionControls();
              drawObstructions();
              showObstructionsSummary(
                "warn",
                "That rectangle was too small to be an obstruction — press " +
                  "“Trace obstruction” and drag across the whole feature " +
                  "(pinch to zoom in first for small ones).",
              );
            }
          },
          cancel: () => {
            // Second finger arrived: the trace becomes a pinch, nothing is
            // committed, and the button must be pressed again — a half-drawn
            // rectangle is not an Obstruction.
            obst.tracePreview = null;
            obst.traceArmed = false;
            updateObstructionControls();
            drawObstructions();
          },
        };
      }

      const hit = hitObstruction(
        obst.list,
        obst.selected,
        obst.view,
        p,
        HANDLE_SLOP_CSS_PX * obstUnit(),
      );
      if (!hit) return null; // empty wall: the default pan (tap deselects)

      if (hit.part === "inside") {
        if (obst.selected !== hit.index) {
          obst.selected = hit.index;
          updateObstructionControls();
          drawObstructions();
        }
        // Drag to move: anchored to where inside the outline the finger
        // landed, so the rectangle doesn't jump to centre under the finger.
        const grabbed = obst.list[hit.index];
        const offset = [start[0] - grabbed.left, start[1] - grabbed.top];
        return {
          move: (q) => {
            const [ix, iy] = viewToImage(obst.view, q);
            const current = obst.list[hit.index];
            const moved = moveRectBy(
              current,
              ix - offset[0] - current.left,
              iy - offset[1] - current.top,
              obst.wallPx,
            );
            if (moved.left !== current.left || moved.top !== current.top) {
              obst.list[hit.index] = { ...moved, type: current.type };
              syncObstructions();
            }
          },
        };
      }

      // Corner handle of the selected outline: resize.
      return {
        move: (q) => {
          const current = obst.list[hit.index];
          const resized = resizeRect(
            current,
            hit.part,
            viewToImage(obst.view, q),
            obst.wallPx,
            obst.minSizePx,
          );
          if (
            resized.left !== current.left ||
            resized.top !== current.top ||
            resized.right !== current.right ||
            resized.bottom !== current.bottom
          ) {
            obst.list[hit.index] = { ...resized, type: current.type };
            syncObstructions();
          }
        },
      };
    },
    // A tap on empty wall clears the selection (and its handles).
    tap: () => {
      if (obst.selected < 0) return;
      obst.selected = -1;
      updateObstructionControls();
      drawObstructions();
    },
  });

  traceObstructionBtn.addEventListener("click", () => {
    if (!obst.meta) return;
    obst.traceArmed = !obst.traceArmed;
    if (obst.traceArmed && obst.selected >= 0) {
      // Arming a trace is about the NEXT obstruction: drop the selection so
      // the type picker reads as "type of what I'm about to trace", not as
      // a retype of what happens to be selected.
      obst.selected = -1;
      drawObstructions();
    }
    updateObstructionControls();
    if (obst.traceArmed) {
      showObstructionsSummary(
        "ok",
        `Drag a rectangle over the ${OBSTRUCTION_TYPES[obstTypeSelect.value].label.toLowerCase()} ` +
          "— corner to corner. Pinch to zoom first if it is small.",
      );
    }
  });

  deleteObstructionBtn.addEventListener("click", () => {
    if (!obst.meta || obst.selected < 0) return;
    obst.list.splice(obst.selected, 1);
    obst.selected = -1;
    syncObstructions();
  });

  // Retype the selected Obstruction in place: its Exclusion Zone follows
  // immediately (a window mistyped as a pipe is a 600 mm compliance hole).
  obstTypeSelect.addEventListener("change", () => {
    if (!obst.meta || obst.selected < 0) return;
    const current = obst.list[obst.selected];
    obst.list[obst.selected] = { ...current, type: obstTypeSelect.value };
    syncObstructions();
  });
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
  const originYMm = image.origin_y_mm();
  const farXMm = image.far_x_mm();
  const calibrated = image.calibrated();
  const calibratedFocalPx = image.calibrated_focal_px();
  const calibratedK1 = image.calibrated_k1();
  const distortionCorrected = image.distortion_corrected();
  const appliedK1 = image.applied_k1();

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
    originYMm,
    keyframesUsed: keyframes,
    truncated,
    closureApplied: closure,
    closureRejected,
    closureDiscrepancyMm: image.closure_discrepancy_mm(),
    closureResidualMm: image.closure_residual_mm(),
    scaleCorrection: image.closure_scale_correction(),
    calibrated,
    calibratedFocalPx,
    calibratedK1,
    distortionCorrected,
    appliedK1,
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
  showBoundsStep({
    widthPx: width,
    heightPx: height,
    mmPerPx: rectified.mmPerPx,
    originXMm,
    originYMm,
    source: "pan",
  });

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
  // Self-calibration outcome (issue #6), stated in all three cases: a
  // refused calibration is a wider bound, not a hidden one, and a
  // distortion-only result never invents a focal.
  const lensText = calibrated
    ? `Lens self-calibrated (focal ${calibratedFocalPx.toFixed(0)} px, ` +
      `k1 ${calibratedK1.toFixed(3)}).`
    : distortionCorrected
      ? `Lens distortion corrected (k1 ${appliedK1.toFixed(3)}); focal ` +
        "unclaimed (this pan had too little rotation to pin it)."
      : "Lens uncalibrated (this pan couldn't support self-calibration; " +
        "pinhole fallback with the wider Error Bound).";
  let summary =
    `Full-wall Rectified Wall Image stitched from ${keyframes} keyframes ` +
    `(${linkInliers.length} tracked links, weakest ${weakest} agreeing points): ` +
    `${width}×${height} px at ${rectified.mmPerPx.toFixed(2)} mm/px. ${lensText} ` +
    "Scroll down to measure.";
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
    const originXMm = image.origin_x_mm();
    const originYMm = image.origin_y_mm();
    const meta = recordRectifiedWallImage({
      widthPx: width,
      heightPx: height,
      mmPerPx: rectified.mmPerPx,
      originXMm,
      originYMm,
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
    showBoundsStep({
      widthPx: width,
      heightPx: height,
      mmPerPx: rectified.mmPerPx,
      originXMm,
      originYMm,
      source: "still",
    });
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
  wireBoundsStep();
  wireObstructionsStep();

  startCaptureBtn.addEventListener("click", () => {
    if (!hasVerifiedPrintScale()) return; // button is disabled anyway
    startCaptureBtn.disabled = true;
    startCapture(wasm, version, isolated);
  });
}

main().catch((err) => {
  showError("Failed to start: " + (err && err.message ? err.message : String(err)));
});
