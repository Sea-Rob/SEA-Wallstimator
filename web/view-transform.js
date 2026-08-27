// Zoom/pan view-transform math shared by every step that works ON the
// Rectified Wall Image (issue #7 bounds confirmation, issue #8 Obstruction
// tracing). Pure module, no DOM; extracted verbatim from wall-bounds.js when
// issue #8 needed the identical pinch-zoom/pan interaction — one
// implementation, one set of tests, one feel under the Homeowner's fingers.
//
// Coordinate frames:
//   image px  — rectified-image pixels, origin top-left, y down.
//   view px   — the display canvas' backing pixels; the transform {scale, tx,
//               ty} maps image → view as view = image·scale + t.

/** Hard ceiling on zoom-in, as a multiple of the fit-to-view scale. Enough
 *  to place a guide or an outline within a couple of millimetres on any
 *  phone; more just turns the image to mush. */
export const MAX_ZOOM = 8;

const clampNum = (v, lo, hi) => Math.min(Math.max(v, lo), hi);

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
 * image with overlays floating over black is unrecoverable for an untrained
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
