# Wallstimator

Lets a homeowner photograph a wall before a quote so SEA can verify — remotely and without a site visit — that a product (battery/inverter) will fit in a compliant, obstruction-free position. Domain vocabulary lives in [CONTEXT.md](CONTEXT.md); the live-pipeline architecture decision in [docs/adr/0001-custom-wasm-live-pipeline.md](docs/adr/0001-custom-wasm-live-pipeline.md).

Current state: **walking skeleton** — camera frames stream through the Rust `geometry-core` crate (compiled to WASM) and back onto a canvas as a grayscale + edge overlay, served cross-origin isolated (COOP/COEP) so WASM threads are available to later slices.

## Layout

- `crates/geometry-core` — Rust core (wasm-bindgen); all frame processing.
- `web/` — capture page (plain JS, no framework) and a tiny dev server that sends the COOP/COEP headers.
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
npm run serve       # serve without rebuilding
npm test            # cargo test --workspace
```

The capture page needs a camera-capable device; over the network use HTTPS (getUserMedia requires a secure context — `localhost` is exempt). The page shows `crossOriginIsolated`, the loaded core version, and the processing frame rate in its status line.

## CI

GitHub Actions runs `cargo test`, builds the release WASM bundle, and fails if the gzipped `.wasm` exceeds 300 KB (ADR-0001 budget: tens-to-low-hundreds of KB).
