# Raw capture video is uploaded as a deliverable

Wallstimator will integrate with SEA's existing photo-request flow, whose backend bundles customer-submitted photos/video as deliverables. The Session Artifact therefore includes the raw capture video alongside the derived data (Rectified Wall Image, traced Obstructions, Exclusion Zones, placement, Fit Verdict, error estimates, keyframes).

We considered and rejected an on-device-only stance (raw footage never leaves the phone): it was the stronger privacy story, but it would have made Wallstimator's deliverables a special case in the existing bundling pipeline, and it permanently barred the back office from re-running the geometry pipeline on dubious captures — recapture requests would have been the only remedy.

## Consequences

- The back office can reprocess raw footage, so borderline or mis-traced sessions are recoverable without bothering the Homeowner.
- Uploads are tens of MB on mobile data; the flow needs upload progress, retry, and ideally Wi-Fi nudging.
- Capture consent copy must state plainly that video of the home is submitted to SEA, with retention governed by the same policy as existing photo-request deliverables.
