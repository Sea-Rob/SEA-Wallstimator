# Wallstimator

Lets a homeowner photograph a wall before a quote so SEA can verify — remotely and without a site visit — that a product (battery/inverter) will fit in a compliant, obstruction-free position. Domain vocabulary lives in [CONTEXT.md](CONTEXT.md); the live-pipeline architecture decision in [docs/adr/0001-custom-wasm-live-pipeline.md](docs/adr/0001-custom-wasm-live-pipeline.md).

Current state: **walking skeleton + Reference Marker supply flow + still-frame rectification + recorded pan (guided-record tier)**. The capture page walks the Homeowner through:

1. **Print the Reference Markers** — download the two-page A4 PDF ([`web/reference-marker.pdf`](web/reference-marker.pdf)): page 1 is Marker A (ArUco 4X4_50, ID 0, LEFT end of the wall area), page 2 is Marker B (ID 1, RIGHT end). Each page carries a 150 mm marker with quiet zone and a 200 mm ruler strip.
2. **Verify print scale** ([ADR-0002](docs/adr/0002-two-marker-metric-reference.md)) — home printers silently rescale ("fit to page"), so the Homeowner measures the printed ruler strip and enters the length; measured / nominal is stored in session state as the print-scale correction factor all later metric computation consumes. Entries beyond ±15% of nominal (wrong units, broken printout) are rejected with a re-measure / reprint prompt — centimetre and inch entries are recognised and called out.
3. **Capture** (gated on step 2) — camera frames stream through the Rust `geometry-core` crate (compiled to WASM) and back onto a canvas as a grayscale + edge overlay, served cross-origin isolated (COOP/COEP) so WASM threads are available to later slices. A **Capture frame** button runs the still-frame path on the current frame; **Record pan** / **Stop & process** runs the recorded-pan path (below).
4. **Rectified Wall Image + measure check** — the captured frame (or stitched pan) is re-projected to fronto-parallel metric coordinates and shown with a two-point measure tool: tap two points, read the distance in millimetres, and check it against a tape measure on the wall. Pan results also display the session's **Error Bound** ("±N mm near Marker A / ±M mm at the far end").

The still path (`FrameProcessor.rectify_captured(correctionFactor)`) runs entirely in the Rust core: classical marker detection (adaptive threshold → quad extraction → 6×6 grid decode against the `marker` dictionary in all 4 rotations → sub-pixel corner refinement via edge-line fits), wall-plane homography estimation (normalized DLT; RANSAC + Levenberg–Marquardt on the general N-point path — 8 corners when both markers are in frame), and inverse-warp rendering with bilinear sampling. It returns a `RectifiedWallImage` exposing the pixels (zero-copy pointer), the mm/px scale (marker side = 150 mm × correction factor), and the corner reprojection residuals (RMS / max, shown in the page status line — the seed of the session Error Bound).

### Recorded pan (issue #4, guided-record tier)

With two Reference Markers taped one per wall end (ADR-0002), the Homeowner records a slow pan from Marker A to Marker B. The core (`pan` module) does everything on-device:

- **Keyframe selection during capture** (`PanRecorder.push_frame`): each frame is scored for sharpness and its motion tracked incrementally by small-window NCC on a 1/8-scale thumbnail; only sharp, well-overlapping keyframes are kept (capped at 30), so a multi-second pan stays within tens of MB instead of buffering gigabytes. Losing track mid-pan is detected and refused loudly.
- **Feature tracking + chaining** (`PanRecorder.finish(correctionFactor)`): Harris-style corners matched by NCC patch search between consecutive keyframes (plus shared marker corners) feed the same RANSAC + LM homography estimator as the still path; per-keyframe homographies are chained into the wall plane anchored at Marker A. Untrackable segments fail with an error naming the segment.
- **Loop closure**: Marker B's known printed size is the far-end metric constraint — the chained back-projection of B is compared with a true-size square, the measured drift is redistributed along the chain as a progressive local-scale field (log-space interpolation with integrated positions), and the post-closure residual is re-measured.
- **Stitching**: one full-wall Rectified Wall Image, each pixel inverse-warped from the covering keyframe whose view centre is nearest (Voronoi pick-best-source; unblended seams).
- **Error Bound** (`error_bound_mm_at(x)`, `error_bound_near_mm/_far_mm/_worst_mm`): per-session 95% bound combining the anchor's corner residuals (with a vertical-extrapolation factor), the per-link tracking random walk, the closure's own precision, and the measured post-closure residual. Without a usable Marker B the result is flagged open-loop and a conservative documented prior widens the bound. CI asserts known distances at both wall ends and the middle are recovered within the reported bound and that the bound stays ≤ 30 mm on a clean synthetic pan (`crates/geometry-core/tests/pan_sequence.rs`).

## Layout

- `crates/geometry-core` — Rust core (wasm-bindgen); all frame processing: the Reference Marker dictionary (`marker` module: ArUco DICT_4X4_50 words for IDs 0/1 and the nominal print geometry — single source of truth for the PDF generator and the detector), marker detection (`detect`), homography estimation (`homography`, hand-rolled linear algebra in `linalg`), still-frame rectification (`rectify`), the recorded-pan pipeline (`pan`: keyframe selection, tracking, chaining, loop closure, stitching, Error Bound), and the synthetic ground-truth scene renderer used by the CI accuracy tests (`synthetic`, incl. a pan-camera model; never shipped in the bundle).
- `crates/marker-pdf` — deterministic zero-dependency PDF writer; regenerates `web/reference-marker.pdf` (committed; CI fails if it drifts from the code).
- `web/` — capture page (plain JS, no framework), `print-scale.js` (pure correction/plausibility math), `session.js` (in-memory session state), and a tiny dev server that sends the COOP/COEP headers.
- `web/pkg/` — generated WASM bundle (built, not committed).

## Prerequisites

- Rust stable with the `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`)
- [wasm-pack](https://rustwasm.github.io/wasm-pack/)
- `wasm-opt` (binaryen)
- Node.js ≥ 18 (dev server only; no npm dependencies)

## Commands

```sh
npm run dev         # build WASM (release) and serve http://localhost:8787/
npm run build:wasm  # wasm-pack build + wasm-opt into web/pkg/
npm run build:pdf   # regenerate web/reference-marker.pdf from geometry-core's marker module
npm run serve       # serve without rebuilding
npm test            # cargo test --workspace + node --test (JS scale math)
npm run test:js     # JS tests only
```

The capture page needs a camera-capable device; over the network use HTTPS (getUserMedia requires a secure context — `localhost` is exempt). The page shows `crossOriginIsolated`, the loaded core version, and the processing frame rate in its status line.

## CI

GitHub Actions runs `cargo test`, the `node --test` JS tests, verifies the committed Reference Marker PDF matches the generator output, builds the release WASM bundle, and fails if the gzipped `.wasm` exceeds 300 KB (ADR-0001 budget: tens-to-low-hundreds of KB).
