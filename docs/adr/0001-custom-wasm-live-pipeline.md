# Custom Rust/WASM live capture pipeline instead of platform AR or OpenCV.js

The Homeowner capture flow must run install-free from a link in SEA's pre-quote email, which rules out ARKit/ARCore (native app required) and WebXR (unavailable on iOS Safari). We build a live AR-style capture pipeline — marker detection, inter-frame tracking, homography chaining, self-calibrated intrinsics (focal + k1 in the LM refinement), metric rectification — as a single Rust core compiled to WASM via wasm-bindgen, rather than shipping OpenCV.js (8+ MB WASM vs a tens-to-low-hundreds-of-KB core).

## Consequences

- The pipeline is two-tier: fully live tracking with rectified preview on capable devices, auto-degrading to guided-record-then-process (cheap live checks only, full pipeline on keyframes after capture) when a startup benchmark or runtime fps watchdog says the device can't keep up. Every device completes capture.
- WASM threads require SharedArrayBuffer, so the capture page must be served cross-origin isolated (COOP/COEP). Integration with SEA's existing photo-request form is via a same-origin mount: Wallstimator is served from the same origin with isolation headers scoped to its routes. It cannot be iframed from a non-isolated page (the top-level document must also be cross-origin isolated), so the handoff is a same-origin navigation, not an inline embed.
- Frame preprocessing (grayscale, pyramid downscale) runs on the GPU via WebGL/WebGPU before pixels reach WASM.
- Native apps ("web first, native later") reuse the geometry/fit math via UniFFI but are expected to swap the tracking layer for ARKit/ARCore; only the accuracy-critical math is the shared surface.
- v1 is classical CV only; ONNX Runtime Web + WebGPU is an explicit v2 slot (obstruction auto-suggest, floor-line detection), not a v1 dependency.
