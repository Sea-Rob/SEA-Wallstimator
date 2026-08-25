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
import { session, recordPrintScale, hasVerifiedPrintScale } from "./session.js";

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

  recordPrintScale({ measuredMm, nominalMm, correctionFactor: verdict.correctionFactor });
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
      setStatus([
        `geometry-core v${version}`,
        `crossOriginIsolated: ${isolated}`,
        correction,
        `${width}×${height} @ ${fps.toFixed(1)} fps`,
      ]);
      frames = 0;
      fpsWindowStart = now;
    }
  }
  requestAnimationFrame(renderLoop);
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

  startCaptureBtn.addEventListener("click", () => {
    if (!hasVerifiedPrintScale()) return; // button is disabled anyway
    startCaptureBtn.disabled = true;
    startCapture(wasm, version, isolated);
  });
}

main().catch((err) => {
  showError("Failed to start: " + (err && err.message ? err.message : String(err)));
});
