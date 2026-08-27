// Wall bounds + Floor Line confirmation math (issue #7). Pure module, no DOM.
//
// The Homeowner confirms the Wall's extent on the Rectified Wall Image by
// dragging three edge guides (left / right / top) and the Floor Line — the
// vertical datum for the Mounting-Height Band and every height measurement
// (CONTEXT.md). This module owns the guide-specific coordinate math the
// confirmation UI needs: guide placement and clamping, finger-sized
// hit-testing, and the final conversion from rectified-image pixels to
// metric wall coordinates. The zoom/pan view transform (pinch on phones,
// wheel on desktop) started life here and moved to view-transform.js when
// issue #8's Obstruction tracing needed the identical interaction; the
// re-exports below keep this module's public surface (and its tests)
// unchanged. The DOM wiring in main.js stays thin and untestable-by-design;
// everything here runs under node --test.
//
// Coordinate frames:
//   image px  — rectified-image pixels, origin top-left, y down.
//   view px   — the display canvas' backing pixels; the transform {scale, tx,
//               ty} maps image → view as view = image·scale + t.
//   wall mm   — the core's wall plane: wall = origin_mm + image_px·mm_per_px
//               on both axes (the anchor marker's printed top-left corner is
//               the plane origin, y grows DOWNWARD like the image).

export {
  MAX_ZOOM,
  fitScale,
  fitTransform,
  imageToView,
  viewToImage,
  clampTransform,
  zoomAt,
  panBy,
  pinchTransform,
} from "./view-transform.js";

const clampNum = (v, lo, hi) => Math.min(Math.max(v, lo), hi);

// ---------------------------------------------------------------------------
// Guides.

/**
 * Sensible starting placement: the wall usually fills most of the rectified
 * image, so the edge guides start at the image extent and the Floor Line
 * starts near the bottom — every guide begins on-screen and visibly wrong
 * enough that the Homeowner understands they are meant to be moved.
 */
export function initialGuides(widthPx, heightPx) {
  return {
    left: 0,
    right: widthPx,
    top: 0,
    floor: heightPx * 0.9,
  };
}

/**
 * Move one guide to `positionPx` (image px: x for left/right, y for
 * top/floor), clamped to the image extent and to `minGapPx` away from its
 * opposing guide — left can never cross right, and the top edge can never
 * cross the Floor Line, so the confirmed rectangle always has positive area.
 * Returns a new guides object; never mutates.
 */
export function moveGuide(guides, which, positionPx, widthPx, heightPx, minGapPx) {
  const g = { ...guides };
  switch (which) {
    case "left":
      g.left = clampNum(positionPx, 0, guides.right - minGapPx);
      break;
    case "right":
      g.right = clampNum(positionPx, guides.left + minGapPx, widthPx);
      break;
    case "top":
      g.top = clampNum(positionPx, 0, guides.floor - minGapPx);
      break;
    case "floor":
      g.floor = clampNum(positionPx, guides.top + minGapPx, heightPx);
      break;
    default:
      throw new Error(`unknown guide: ${which}`);
  }
  return g;
}

// ---------------------------------------------------------------------------
// Hit-testing.

/**
 * Which guide (if any) a pointer at `viewPoint` grabs. Distances are
 * measured perpendicular to each guide line in VIEW pixels, so `slopViewPx`
 * is a constant finger-sized target regardless of zoom — zooming in makes
 * placement finer without shrinking the grab area under the finger. The
 * nearest guide within the slop wins; the Floor Line wins exact ties (it is
 * the datum this step exists to confirm). Returns "left" | "right" | "top" |
 * "floor" | null.
 */
export function hitGuide(guides, t, [vx, vy], slopViewPx) {
  const candidates = [
    ["floor", Math.abs(vy - (guides.floor * t.scale + t.ty))],
    ["top", Math.abs(vy - (guides.top * t.scale + t.ty))],
    ["left", Math.abs(vx - (guides.left * t.scale + t.tx))],
    ["right", Math.abs(vx - (guides.right * t.scale + t.tx))],
  ];
  let best = null;
  let bestDist = Infinity;
  for (const [name, d] of candidates) {
    if (d <= slopViewPx && d < bestDist) {
      best = name;
      bestDist = d;
    }
  }
  return best;
}

// ---------------------------------------------------------------------------
// Metric conversion.

/**
 * Convert confirmed guide positions (image px) to metric wall coordinates
 * using the Rectified Wall Image's own scale and origin (both already carry
 * the session's print-scale correction). Wall y grows DOWNWARD, so the
 * Floor Line has the LARGEST y of the confirmed rectangle; a height above
 * the floor is floorYMm − yMm.
 */
export function guidesToWallMm(guides, { mmPerPx, originXMm, originYMm }) {
  return {
    leftXMm: originXMm + guides.left * mmPerPx,
    rightXMm: originXMm + guides.right * mmPerPx,
    topYMm: originYMm + guides.top * mmPerPx,
    floorYMm: originYMm + guides.floor * mmPerPx,
  };
}
