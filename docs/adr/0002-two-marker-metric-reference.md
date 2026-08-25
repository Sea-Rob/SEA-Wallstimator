# Two printed, self-scale-verified markers anchor the tracked pan

A single A4 marker concentrates all metric control points in a ~20×30 cm patch; homography error grows roughly linearly with extrapolation distance, so sub-pixel corner noise and paper bulge become centimetres of systematic error at the far end of a 3–6 m wall. Instead, the Homeowner prints a two-page PDF and tapes one marker near each end of the candidate area: each anchors scale and plane locally, the live-tracked pan connects them, and drift in the chained frame-to-frame homographies is corrected by loop closure against the second marker.

Because home printers silently rescale ("fit to page", 3–6% error = 15 cm over 5 m), the flow requires scale self-verification: the printout includes a ruler strip the Homeowner measures against a bank card or tape measure, and the app corrects for the reported print scale in software.

## Considered options

- **One marker only** — minimum friction, but the far wall end rests on unanchored tracking drift with no cross-check to detect it.
- **Mailed rigid marker** — best accuracy, but adds days of latency that kills the instant pre-quote flow.
- **Single-view metrology (Criminisi et al.) from wall features** — fragile: rendered walls often lack usable parallel texture, and homeowner-tapped features are imprecise.
