// Wall bounds + Floor Line confirmation math (issue #7). Pure module, no DOM.
//
// The Homeowner confirms the Wall's extent on the Rectified Wall Image by
// dragging three edge guides (left / right / top) and the Floor Line — the
// vertical datum for the Mounting-Height Band and every height measurement
// (CONTEXT.md). This module owns ALL the coordinate math the confirmation UI
// needs: guide placement and clamping, the zoom/pan view transform (pinch on
// phones, wheel on desktop), finger-sized hit-testing, and the final
// conversion from rectified-image pixels to metric wall coordinates. The DOM
// wiring in main.js stays thin and untestable-by-design; everything here runs
// under node --test.
//
// Coordinate frames:
//   image px  — rectified-image pixels, origin top-left, y down.
//   view px   — the display canvas' backing pixels; the transform {scale, tx,
//               ty} maps image → view as view = image·scale + t.
//   wall mm   — the core's wall plane: wall = origin_mm + image_px·mm_per_px
//               on both axes (the anchor marker's printed top-left corner is
//               the plane origin, y grows DOWNWARD like the image).

/** Hard ceiling on zoom-in, as a multiple of the fit-to-view scale. Enough
 *  to place a guide within a couple of millimetres on any phone; more just
 *  turns the image to mush. */
export const MAX_ZOOM = 8;

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
// View transform (zoom + pan).

/** Scale that letterboxes the whole image inside the view. */
export function fitScale(imageW, imageH, viewW, viewH) {
  return Math.min(viewW / imageW, viewH / imageH);
}

/** Transform showing the whole image centred in the view (zoomed all the
 *  way out — the state every fresh image starts in). */
export function fitTransform(imageW, imageH, viewW, viewH) {
  const scale = fitScale(imageW, imageH, viewW, viewH);
  return {
    scale,
    tx: (viewW - imageW * scale) / 2,
    ty: (viewH - imageH * scale) / 2,
  };
}

export function imageToView(t, [x, y]) {
  return [x * t.scale + t.tx, y * t.scale + t.ty];
}

export function viewToImage(t, [x, y]) {
  return [(x - t.tx) / t.scale, (y - t.ty) / t.scale];
}

/**
 * Keep the view inside the image: zoom never drops below fit (the whole
 * image is the natural floor) or above MAX_ZOOM×fit, and panning can never
 * push an image edge past the view edge it letterboxes against — a lost
 * image with guides floating over black is unrecoverable for an untrained
 * Homeowner. Axes where the image is smaller than the view stay centred.
 */
export function clampTransform(t, imageW, imageH, viewW, viewH) {
  const fit = fitScale(imageW, imageH, viewW, viewH);
  const scale = clampNum(t.scale, fit, fit * MAX_ZOOM);
  const clampAxis = (offset, scaledSize, viewSize) =>
    scaledSize <= viewSize
      ? (viewSize - scaledSize) / 2 // smaller than the view: centre, no pan
      : clampNum(offset, viewSize - scaledSize, 0);
  return {
    scale,
    tx: clampAxis(t.tx, imageW * scale, viewW),
    ty: clampAxis(t.ty, imageH * scale, viewH),
  };
}

/**
 * Zoom by `factor` about a fixed view point (wheel position / pinch centre):
 * the image point under the cursor stays under the cursor, so zooming aims
 * itself at what the Homeowner is looking at.
 */
export function zoomAt(t, [vx, vy], factor, imageW, imageH, viewW, viewH) {
  const fit = fitScale(imageW, imageH, viewW, viewH);
  const scale = clampNum(t.scale * factor, fit, fit * MAX_ZOOM);
  const k = scale / t.scale;
  return clampTransform(
    { scale, tx: vx - k * (vx - t.tx), ty: vy - k * (vy - t.ty) },
    imageW,
    imageH,
    viewW,
    viewH,
  );
}

/** Translate the view (one-finger drag on the image while zoomed). */
export function panBy(t, dx, dy, imageW, imageH, viewW, viewH) {
  return clampTransform(
    { scale: t.scale, tx: t.tx + dx, ty: t.ty + dy },
    imageW,
    imageH,
    viewW,
    viewH,
  );
}

/**
 * Two-finger pinch update: scale by the ratio of the finger distances and
 * translate so the image point that was under the fingers' midpoint stays
 * under it — the standard pinch-zoom feel (zoom and pan in one gesture,
 * no rotation). `before`/`after` are the two pointers' view positions at
 * the previous and current events.
 */
export function pinchTransform(t, before, after, imageW, imageH, viewW, viewH) {
  const dist = ([a, b]) => Math.hypot(b[0] - a[0], b[1] - a[1]);
  const mid = ([a, b]) => [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2];
  const d0 = dist(before);
  const d1 = dist(after);
  // Degenerate pinch (fingers reported on top of each other): pan only.
  const factor = d0 > 1e-6 && d1 > 1e-6 ? d1 / d0 : 1;

  const fit = fitScale(imageW, imageH, viewW, viewH);
  const scale = clampNum(t.scale * factor, fit, fit * MAX_ZOOM);
  const k = scale / t.scale;
  const [mx0, my0] = mid(before);
  const [mx1, my1] = mid(after);
  return clampTransform(
    { scale, tx: mx1 - k * (mx0 - t.tx), ty: my1 - k * (my0 - t.ty) },
    imageW,
    imageH,
    viewW,
    viewH,
  );
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
