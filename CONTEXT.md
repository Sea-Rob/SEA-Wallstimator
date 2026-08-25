# Wallstimator

Lets a homeowner photograph a wall before a quote so SEA can verify — remotely and without a site visit — that a product (battery/inverter) will fit in a compliant, obstruction-free position.

## Language

**Homeowner**:
The person operating the app: the customer capturing their own wall before receiving a quote. Assumed untrained; every flow must be self-explanatory.
_Avoid_: user, customer, operator

**Wall**:
The single planar surface being assessed in one capture session. One session measures one wall.

**Reference Marker**:
The printed target the Homeowner fixes flat against the Wall to give the geometry core its metric scale and plane reference.
_Avoid_: fiducial, tag, target (ambiguous with "target wall")

**Rectified Wall Image**:
The captured photo re-projected to fronto-parallel metric coordinates via planar homography. All tracing, measurement, and fit checking happens in this image's coordinate frame.
_Avoid_: flattened image, warped image

**Obstruction**:
A typed region the Homeowner traces on the Rectified Wall Image marking something the product cannot overlap — window, door, pipe, meter box, vent, other. An Obstruction has an outline and a type; the type determines its Exclusion Zone.
_Avoid_: obstacle, blocker

**Exclusion Zone**:
The Obstruction's outline inflated by a type-specific compliance buffer (e.g. 600 mm around openable windows/doors per AS/NZS 5139). Pipes and purely physical Obstructions have a zero buffer. Fit checking operates on Exclusion Zones, not raw outlines.
_Avoid_: clearance (reserved for the Envelope's own required margins), buffer zone

**Envelope**:
The rectangle of wall space a product requires: its physical width × height plus its own mandated installation clearances, together with its Mounting-Height Band. Configured per product; the Homeowner never enters dimensions.
_Avoid_: footprint, required space

**Mounting-Height Band**:
The permitted vertical range, measured from the Floor Line, within which the Envelope's bottom edge may sit (e.g. floor-standing = fixed at 0; wall-mounted = min/max height per product manual and AS/NZS 5139).

**Floor Line**:
The wall/floor junction on the Rectified Wall Image, confirmed by the Homeowner. The vertical datum for the Mounting-Height Band and all height measurements.
_Avoid_: ground line, baseline

**Clear Zone**:
A region of the Rectified Wall Image inside the Wall's bounds and outside every Exclusion Zone. The fit check searches Clear Zones for a placement of the Envelope.

**Fit Verdict**:
The Homeowner-facing outcome of a session: *fits* (with placement), *doesn't fit*, or *can't confirm*. *Fits* means the Envelope fits even after shrinking Clear Zones by the session's Error Bound; *doesn't fit* means it fails even after expanding them by it; everything between is *can't confirm*.
_Avoid_: result, pass/fail

**Error Bound**:
The per-session 95% confidence bound on measurement error, derived from the capture's own quality (marker residuals, tracking drift at loop closure). A sloppy capture widens it — and with it, the *can't confirm* band.
_Avoid_: accuracy, tolerance

**Session Artifact**:
The package submitted to SEA's back office for verification: the Rectified Wall Image annotated with Obstructions, Exclusion Zones, the proposed placement, the Fit Verdict, selected keyframes, and the raw capture video. Bundled as deliverables by SEA's existing photo-request backend.
_Avoid_: report, submission, deliverable (that's the photo-request pipeline's term for the bundle)

## Example dialogue

**Dev**: The homeowner traced a window but the verdict still says *fits* — bug?
**Expert**: Check the Exclusion Zone, not the outline. A window inflates by 600 mm; if the Envelope sits outside that inflated region, *fits* is correct.
**Dev**: And if the marker detection was shaky that day?
**Expert**: Then we don't guess — the Fit Verdict must be *can't confirm* and the Session Artifact still goes to the back office so a human can look.
