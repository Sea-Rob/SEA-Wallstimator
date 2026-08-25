# Wallstimator

Lets a homeowner photograph a wall before a quote so SEA can verify — remotely and without a site visit — that a product (battery/inverter) will fit in a compliant, obstruction-free position. Domain vocabulary lives in [CONTEXT.md](CONTEXT.md); the live-pipeline architecture decision in [docs/adr/0001-custom-wasm-live-pipeline.md](docs/adr/0001-custom-wasm-live-pipeline.md).

Current state: **walking skeleton + Reference Marker supply flow**. The capture page walks the Homeowner through:

1. **Print the Reference Markers** — download the two-page A4 PDF ([`web/reference-marker.pdf`](web/reference-marker.pdf)): page 1 is Marker A (ArUco 4X4_50, ID 0, LEFT end of the wall area), page 2 is Marker B (ID 1, RIGHT end). Each page carries a 150 mm marker with quiet zone and a 200 mm ruler strip.
2. **Verify print scale** ([ADR-0002](docs/adr/0002-two-marker-metric-reference.md)) — home printers silently rescale ("fit to page"), so the Homeowner measures the printed ruler strip and enters the length; measured / nominal is stored in session state as the print-scale correction factor all later metric computation consumes. Entries beyond ±15% of nominal (wrong units, broken printout) are rejected with a re-measure / reprint prompt — centimetre and inch entries are recognised and called out.
3. **Capture** (gated on step 2) — camera frames stream through the Rust `geometry-core` crate (compiled to WASM) and back onto a canvas as a grayscale + edge overlay, served cross-origin isolated (COOP/COEP) so WASM threads are available to later slices.

## Layout

- `crates/geometry-core` — Rust core (wasm-bindgen); all frame processing, plus the Reference Marker dictionary (`marker` module: ArUco DICT_4X4_50 words for IDs 0/1 and the nominal print geometry — single source of truth for the PDF generator and the upcoming detector).
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
