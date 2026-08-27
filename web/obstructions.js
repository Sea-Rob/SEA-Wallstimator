// Obstruction tracing math (issue #8). Pure module, no DOM.
//
// On the confirmed Rectified Wall Image (gated on the issue #7 wall-bounds
// confirmation) the Homeowner traces rectangles over Obstructions — windows,
// doors, pipes, meter boxes, vents — and each typed outline gets an
// Exclusion Zone computed by the Rust core (outline inflated by the type's
// compliance buffer, clipped to the Wall bounds). This module owns all the
// coordinate math the tracing UI needs: rectangle construction/clamping for
// trace, move and resize, finger-sized hit-testing of outlines and their
// corner handles, and the conversions between rectified-image pixels and
// metric wall coordinates (both directions — outlines go out to the core in
// mm, zones come back in mm and are drawn in px). The zoom/pan view
// transform is shared with the bounds step via view-transform.js. The DOM
// wiring in main.js stays thin; everything here runs under node --test.
//
// Rectangles are {left, top, right, bottom} with right > left and
// bottom > top; image px unless the name says Mm. Wall y grows DOWNWARD.

import { bufferMmForType } from "./obstruction-types.js";

const clampNum = (v, lo, hi) => Math.min(Math.max(v, lo), hi);

// ---------------------------------------------------------------------------
// Rectangle construction and editing (image px, clamped to the Wall).

/**
 * The rectangle spanned by two traced corner points, both clamped into the
 * confirmed Wall rectangle first — an Obstruction is a region OF the Wall,
 * so a finger that wanders past an edge just pins the outline to it. The
 * result is normalized (left ≤ right, top ≤ bottom) but may be degenerate
 * mid-trace; commit only when `meetsMinSize` says so.
 */
export function traceRect([ax, ay], [bx, by], wallPx) {
  const cx = (x) => clampNum(x, wallPx.left, wallPx.right);
  const cy = (y) => clampNum(y, wallPx.top, wallPx.bottom);
  return {
    left: Math.min(cx(ax), cx(bx)),
    top: Math.min(cy(ay), cy(by)),
    right: Math.max(cx(ax), cx(bx)),
    bottom: Math.max(cy(ay), cy(by)),
  };
}

/** Both sides at least `minSizePx`: the commit gate that turns an
 *  accidental tap during tracing into a no-op instead of a sliver. */
export function meetsMinSize(rect, minSizePx) {
  return rect.right - rect.left >= minSizePx && rect.bottom - rect.top >= minSizePx;
}

/**
 * Translate a rectangle by (dx, dy) image px, clamped so it stays entirely
 * inside the Wall rectangle — size is preserved, the drag just stops at the
 * edges. Returns a new rectangle; never mutates.
 */
export function moveRectBy(rect, dx, dy, wallPx) {
  const w = rect.right - rect.left;
  const h = rect.bottom - rect.top;
  const left = clampNum(rect.left + dx, wallPx.left, wallPx.right - w);
  const top = clampNum(rect.top + dy, wallPx.top, wallPx.bottom - h);
  return { left, top, right: left + w, bottom: top + h };
}

/**
 * Drag one corner ("nw" | "ne" | "sw" | "se") to `pointPx`, keeping the
 * opposite corner fixed. Clamped to the Wall rectangle and to `minSizePx`
 * from the fixed corner — corners never cross, so the rectangle keeps its
 * orientation and its area under the sloppiest drag.
 */
export function resizeRect(rect, corner, [px, py], wallPx, minSizePx) {
  const r = { ...rect };
  const west = corner === "nw" || corner === "sw";
  const north = corner === "nw" || corner === "ne";
  if (west) {
    r.left = clampNum(px, wallPx.left, rect.right - minSizePx);
  } else {
    r.right = clampNum(px, rect.left + minSizePx, wallPx.right);
  }
  if (north) {
    r.top = clampNum(py, wallPx.top, rect.bottom - minSizePx);
  } else {
    r.bottom = clampNum(py, rect.top + minSizePx, wallPx.bottom);
  }
  return r;
}

// ---------------------------------------------------------------------------
// Hit-testing.

const CORNERS = [
  ["nw", (r) => [r.left, r.top]],
  ["ne", (r) => [r.right, r.top]],
  ["sw", (r) => [r.left, r.bottom]],
  ["se", (r) => [r.right, r.bottom]],
];

/**
 * What a pointer at `viewPoint` grabs, in priority order:
 *
 * 1. A corner handle of the SELECTED rectangle (nearest within the slop) —
 *    handles are only drawn on the selection, so only the selection resizes.
 * 2. The interior of a rectangle, topmost (last-traced) first — matching
 *    what the eye sees when outlines overlap.
 *
 * Distances are in VIEW px like the bounds step's hitGuide: `slopViewPx` is
 * a constant finger-sized target regardless of zoom. Returns
 * {index, part: "nw"|"ne"|"sw"|"se"|"inside"} or null.
 */
export function hitObstruction(rects, selectedIndex, t, [vx, vy], slopViewPx) {
  const toView = ([x, y]) => [x * t.scale + t.tx, y * t.scale + t.ty];
  if (selectedIndex >= 0 && selectedIndex < rects.length) {
    let best = null;
    let bestDist = Infinity;
    for (const [part, cornerOf] of CORNERS) {
      const [cx, cy] = toView(cornerOf(rects[selectedIndex]));
      const d = Math.hypot(vx - cx, vy - cy);
      if (d <= slopViewPx && d < bestDist) {
        best = { index: selectedIndex, part };
        bestDist = d;
      }
    }
    if (best) return best;
  }
  for (let i = rects.length - 1; i >= 0; i--) {
    const [l, tp] = toView([rects[i].left, rects[i].top]);
    const [r, b] = toView([rects[i].right, rects[i].bottom]);
    if (vx >= l && vx <= r && vy >= tp && vy <= b) {
      return { index: i, part: "inside" };
    }
  }
  return null;
}

// ---------------------------------------------------------------------------
// Metric conversion (image px <-> wall mm) and the core call's flat layout.

/** Outline in image px -> metric wall coordinates, through the Rectified
 *  Wall Image's own scale and origin (print-scale correction included). */
export function rectToWallMm(rect, { mmPerPx, originXMm, originYMm }) {
  return {
    leftXMm: originXMm + rect.left * mmPerPx,
    topYMm: originYMm + rect.top * mmPerPx,
    rightXMm: originXMm + rect.right * mmPerPx,
    bottomYMm: originYMm + rect.bottom * mmPerPx,
  };
}

/** Inverse of rectToWallMm: a zone from the core (mm) back to image px for
 *  drawing. */
export function rectFromWallMm(mmRect, { mmPerPx, originXMm, originYMm }) {
  return {
    left: (mmRect.leftXMm - originXMm) / mmPerPx,
    top: (mmRect.topYMm - originYMm) / mmPerPx,
    right: (mmRect.rightXMm - originXMm) / mmPerPx,
    bottom: (mmRect.bottomYMm - originYMm) / mmPerPx,
  };
}

/** The confirmed Wall bounds record (session.wallBounds, mm) as an image-px
 *  rectangle — the clamp region every outline lives in. */
export function wallBoundsToImagePx(wallBounds, { mmPerPx, originXMm, originYMm }) {
  return {
    left: (wallBounds.leftXMm - originXMm) / mmPerPx,
    top: (wallBounds.topYMm - originYMm) / mmPerPx,
    right: (wallBounds.rightXMm - originXMm) / mmPerPx,
    bottom: (wallBounds.floorYMm - originYMm) / mmPerPx,
  };
}

/**
 * Pack typed outlines (image px) into the flat arrays the core's
 * `exclusion_zones_mm` consumes: 4 mm values per outline plus the per-type
 * compliance buffer from the reviewable config. Throws on an unknown type —
 * a missing buffer must never silently become 0 mm.
 */
export function packObstructionsMm(list, meta) {
  const outlines = new Float64Array(list.length * 4);
  const buffers = new Float64Array(list.length);
  list.forEach((o, i) => {
    const mm = rectToWallMm(o, meta);
    outlines[i * 4] = mm.leftXMm;
    outlines[i * 4 + 1] = mm.topYMm;
    outlines[i * 4 + 2] = mm.rightXMm;
    outlines[i * 4 + 3] = mm.bottomYMm;
    const buffer = bufferMmForType(o.type);
    if (buffer === null) {
      throw new Error(`unknown obstruction type: ${o.type}`);
    }
    buffers[i] = buffer;
  });
  return { outlines, buffers };
}

/** Zones as returned by the core (flat, 4 values each, same order as the
 *  outlines that went in) -> mm rectangles for drawing and inspection. */
export function unpackZonesMm(flat) {
  const zones = [];
  for (let i = 0; i + 3 < flat.length; i += 4) {
    zones.push({
      leftXMm: flat[i],
      topYMm: flat[i + 1],
      rightXMm: flat[i + 2],
      bottomYMm: flat[i + 3],
    });
  }
  return zones;
}
