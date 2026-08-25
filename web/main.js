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
  core_version,
  ruler_nominal_mm,
} from "./pkg/geometry_core.js";
import { evaluateRulerMeasurement } from "./print-scale.js";
import {
  session,
  recordPrintScale,
  recordRectifiedWallImage,
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
        parts.push(
          `rectified ${session.rectified.mmPerPx.toFixed(2)} mm/px, ` +
            `residual RMS ${session.rectified.residualRmsPx.toFixed(2)} px`,
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
};

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
  measureResult.className = "result ok";
  measureResult.textContent =
    `Distance: ${distMm.toFixed(1)} mm (${(distMm / 10).toFixed(1)} cm). ` +
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

    const markerIds = Array.from(image.marker_ids());
    const meta = recordRectifiedWallImage({
      widthPx: width,
      heightPx: height,
      mmPerPx: rectified.mmPerPx,
      markerIds,
      residualRmsPx: image.residual_rms_px(),
      residualMaxPx: image.residual_max_px(),
      pointsUsed: image.points_used(),
      inliers: image.inliers(),
    });

    drawMeasureOverlay();
    rectifiedSection.hidden = false;
    updateMeasureReadout();
    const markerNames = markerIds.map((id) => (id === 0 ? "A" : "B")).join(" + ");
    showCaptureResult(
      "ok",
      `Rectified Wall Image rendered from marker ${markerNames}: ` +
        `${width}×${height} px at ${rectified.mmPerPx.toFixed(2)} mm/px, ` +
        `corner reprojection RMS ${meta.residualRmsPx.toFixed(2)} px ` +
        `(max ${meta.residualMaxPx.toFixed(2)} px, ${meta.inliers}/${meta.pointsUsed} corners). ` +
        "Scroll down to measure.",
    );
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
