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
import { pickMainRearCamera, zoomLockConstraint } from "./camera.js";
import {
  initialGuides,
  moveGuide,
  fitTransform,
  imageToView,
  viewToImage,
  clampTransform,
  zoomAt,
  panBy,
  pinchTransform,
  hitGuide,
  guidesToWallMm,
} from "./wall-bounds.js";
import {
  session,
  recordPrintScale,
  recordRectifiedWallImage,
  recordPanResult,
  recordWallBounds,
  clearWallBounds,
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

// Finger-sized grab distance for a guide, in CSS pixels: measured in view
// space so zooming in refines placement without shrinking the target.
const HANDLE_SLOP_CSS_PX = 26;

// Guide clamping guardrail, NOT a product judgement: guides can't be pushed
// into a rectangle smaller than this, so a confirmed wall always has
// meaningfully positive area even after a stray drag.
const MIN_WALL_DIMENSION_MM = 100;

const bounds = {
  meta: null, // {widthPx, heightPx, mmPerPx, originXMm, originYMm, source}
  image: null, // offscreen canvas holding the rectified pixels
  guides: null, // image-px guide positions (wall-bounds.js shape)
  view: null, // zoom/pan transform image->view (wall-bounds.js shape)
  minGapPx: 0,
  confirmed: false,
  pointers: new Map(), // active pointerId -> [x, y] view px
  mode: null, // {type:"drag", guide} | {type:"pan"} | {type:"pinch"}
};

/** Match the canvas backing store to its CSS box (times devicePixelRatio)
 *  so guide lines stay crisp; the view transform works in backing px. */
function sizeBoundsCanvas() {
  const rect = boundsCanvas.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  const w = Math.max(2, Math.round(rect.width * dpr));
  const h = Math.max(2, Math.round(rect.height * dpr));
  if (boundsCanvas.width !== w || boundsCanvas.height !== h) {
    boundsCanvas.width = w;
    boundsCanvas.height = h;
  }
}

/** Backing pixels per CSS pixel — sizes strokes/handles/slop finger-true. */
function boundsUnit() {
  const rect = boundsCanvas.getBoundingClientRect();
  return rect.width > 0 ? boundsCanvas.width / rect.width : 1;
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
  confirmBoundsBtn.disabled = !bounds.meta || bounds.confirmed;
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
      "Wall bounds and Floor Line locked in — the next step (tracing " +
      "Obstructions) will work inside them. Moving any guide unlocks " +
      "re-confirmation.";
  } else {
    const mm = guidesToWallMm(bounds.guides, bounds.meta);
    boundsSummary.className = "result ok";
    boundsSummary.textContent =
      `Current guides: ${(mm.rightXMm - mm.leftXMm).toFixed(0)} mm wide × ` +
      `${(mm.floorYMm - mm.topYMm).toFixed(0)} mm from the Floor Line to the top edge.`;
    boundsGate.className = "result warn";
    boundsGate.textContent =
      "Not confirmed yet — the following steps stay locked until you place " +
      "the guides and press Confirm.";
  }
}

/** A guide moved: any prior confirmation no longer describes the screen. */
function unconfirmBounds() {
  if (!bounds.confirmed) return;
  bounds.confirmed = false;
  clearWallBounds();
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
  bounds.pointers.clear();
  bounds.mode = null;
  boundsSection.hidden = false;
  sizeBoundsCanvas();
  bounds.view = fitTransform(meta.widthPx, meta.heightPx, boundsCanvas.width, boundsCanvas.height);
  drawBounds();
  updateBoundsGate();
}

function wireBoundsStep() {
  const viewSize = () => [boundsCanvas.width, boundsCanvas.height];

  boundsCanvas.addEventListener("pointerdown", (event) => {
    if (!bounds.image) return;
    event.preventDefault();
    try {
      boundsCanvas.setPointerCapture(event.pointerId);
    } catch {
      // No capturable pointer (synthetic events, some stylus edge cases):
      // the drag still works, it just loses the off-canvas grace capture.
    }
    bounds.pointers.set(event.pointerId, canvasPoint(boundsCanvas, event));
    if (bounds.pointers.size === 2) {
      // Second finger always switches to pinch — even mid guide-drag: the
      // Homeowner is asking for precision, not a bigger drag.
      bounds.mode = { type: "pinch" };
    } else if (bounds.pointers.size === 1) {
      const p = bounds.pointers.get(event.pointerId);
      const guide = hitGuide(bounds.guides, bounds.view, p, HANDLE_SLOP_CSS_PX * boundsUnit());
      bounds.mode = guide ? { type: "drag", guide } : { type: "pan" };
    }
  });

  boundsCanvas.addEventListener("pointermove", (event) => {
    if (!bounds.image || !bounds.pointers.has(event.pointerId)) return;
    event.preventDefault();
    const p = canvasPoint(boundsCanvas, event);
    const prev = bounds.pointers.get(event.pointerId);
    const m = bounds.meta;
    const [vw, vh] = viewSize();

    if (bounds.mode?.type === "pinch" && bounds.pointers.size >= 2) {
      const ids = [...bounds.pointers.keys()].slice(0, 2);
      if (!ids.includes(event.pointerId)) return; // ignore a stray 3rd finger
      const before = ids.map((id) => bounds.pointers.get(id));
      bounds.pointers.set(event.pointerId, p);
      const after = ids.map((id) => bounds.pointers.get(id));
      bounds.view = pinchTransform(bounds.view, before, after, m.widthPx, m.heightPx, vw, vh);
      drawBounds();
      return;
    }

    bounds.pointers.set(event.pointerId, p);
    if (bounds.mode?.type === "drag") {
      const [ix, iy] = viewToImage(bounds.view, p);
      const alongX = bounds.mode.guide === "left" || bounds.mode.guide === "right";
      bounds.guides = moveGuide(
        bounds.guides,
        bounds.mode.guide,
        alongX ? ix : iy,
        m.widthPx,
        m.heightPx,
        bounds.minGapPx,
      );
      unconfirmBounds();
      drawBounds();
      updateBoundsGate();
    } else if (bounds.mode?.type === "pan") {
      bounds.view = panBy(bounds.view, p[0] - prev[0], p[1] - prev[1], m.widthPx, m.heightPx, vw, vh);
      drawBounds();
    }
  });

  const releasePointer = (event) => {
    if (!bounds.pointers.delete(event.pointerId)) return;
    if (bounds.pointers.size === 0) {
      bounds.mode = null;
    } else if (bounds.pointers.size === 1 && bounds.mode?.type === "pinch") {
      bounds.mode = { type: "pan" }; // lifted one pinch finger: keep panning
    }
  };
  boundsCanvas.addEventListener("pointerup", releasePointer);
  boundsCanvas.addEventListener("pointercancel", releasePointer);

  // Desktop precision without touch: wheel zooms about the cursor.
  boundsCanvas.addEventListener(
    "wheel",
    (event) => {
      if (!bounds.image) return;
      event.preventDefault();
      const m = bounds.meta;
      bounds.view = zoomAt(
        bounds.view,
        canvasPoint(boundsCanvas, event),
        event.deltaY < 0 ? 1.2 : 1 / 1.2,
        m.widthPx,
        m.heightPx,
        boundsCanvas.width,
        boundsCanvas.height,
      );
      drawBounds();
    },
    { passive: false },
  );

  confirmBoundsBtn.addEventListener("click", confirmBounds);
  resetGuidesBtn.addEventListener("click", () => {
    if (!bounds.meta) return;
    bounds.guides = initialGuides(bounds.meta.widthPx, bounds.meta.heightPx);
    bounds.view = fitTransform(bounds.meta.widthPx, bounds.meta.heightPx, boundsCanvas.width, boundsCanvas.height);
    unconfirmBounds();
    drawBounds();
    updateBoundsGate();
  });

  // Rotating the phone / resizing the window reshapes the canvas box: keep
  // the backing store matched and re-clamp the view (guides are image-px
  // state, so they survive untouched).
  window.addEventListener("resize", () => {
    if (!bounds.image) return;
    sizeBoundsCanvas();
    bounds.view = clampTransform(
      bounds.view,
      bounds.meta.widthPx,
      bounds.meta.heightPx,
      boundsCanvas.width,
      boundsCanvas.height,
    );
    drawBounds();
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

  startCaptureBtn.addEventListener("click", () => {
    if (!hasVerifiedPrintScale()) return; // button is disabled anyway
    startCaptureBtn.disabled = true;
    startCapture(wasm, version, isolated);
  });
}

main().catch((err) => {
  showError("Failed to start: " + (err && err.message ? err.message : String(err)));
});
