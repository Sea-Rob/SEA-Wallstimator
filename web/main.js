// Wallstimator capture page — walking skeleton.
//
// Proves the full path every later slice rides on:
//   getUserMedia -> canvas pixels -> WASM (geometry-core) -> processed overlay.
//
// The page must be served cross-origin isolated (COOP/COEP, see ADR-0001) so
// that SharedArrayBuffer — and with it WASM threads — is available to later
// slices. This slice only asserts the isolation; it does not use threads yet.

import init, { FrameProcessor, core_version } from "./pkg/geometry_core.js";

// Processing resolution: enough to see edges, cheap enough for a mid-range
// phone on the CPU-only skeleton path (GPU preprocessing comes later).
const PROC_WIDTH = 640;

const statusEl = document.getElementById("status");
const errorEl = document.getElementById("error");
const video = document.getElementById("camera");
const overlay = document.getElementById("overlay");
const overlayCtx = overlay.getContext("2d");

function showError(message) {
  errorEl.textContent = message;
  errorEl.classList.add("visible");
}

function setStatus(parts) {
  statusEl.textContent = parts.join(" · ");
}

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

  let stream;
  try {
    stream = await openCamera();
  } catch (err) {
    showError(
      "Camera unavailable: " +
        (err && err.message ? err.message : String(err)) +
        " — allow camera access and reload. The WASM core itself loaded fine.",
    );
    return;
  }

  video.srcObject = stream;
  await video.play();

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
      setStatus([
        `geometry-core v${version}`,
        `crossOriginIsolated: ${isolated}`,
        `${width}×${height} @ ${fps.toFixed(1)} fps`,
      ]);
      frames = 0;
      fpsWindowStart = now;
    }
    requestAnimationFrame(renderLoop);
  }
  requestAnimationFrame(renderLoop);
}

main().catch((err) => {
  showError("Failed to start: " + (err && err.message ? err.message : String(err)));
});
