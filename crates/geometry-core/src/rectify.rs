//! Still-frame rectification: detected Reference Marker(s) -> wall-plane
//! homography -> Rectified Wall Image in fronto-parallel metric coordinates.
//!
//! The wall-plane frame is anchored at the first detected marker: its printed
//! top-left corner is the origin, x runs along its top edge (mm), y down its
//! left edge (mm). The marker's true physical side is
//! `marker::MARKER_SIDE_MM * correction_factor` (MULTIPLY — see lib.rs and
//! ADR-0002), which is what puts real millimetres behind the output's
//! mm-per-px scale.

use crate::detect::{detect_markers, DetectedMarker};
use crate::homography::{estimate, Estimate, Homography};
use crate::marker::MARKER_SIDE_MM;

/// RANSAC / degradation threshold for marker-corner reprojection (px).
const INLIER_THRESHOLD_PX: f64 = 3.0;

/// Longest allowed output side (px): bounds memory for close-up shots of
/// large walls.
const MAX_OUTPUT_PX: usize = 1600;

/// Furthest the output extends from the anchor marker's origin (mm) — points
/// near the horizon back-project to absurd plane coordinates otherwise.
const MAX_EXTENT_MM: f64 = 6000.0;

/// A rendered Rectified Wall Image plus the metric mapping and the quality
/// numbers (reprojection residuals) the API must expose.
pub struct Rectified {
    /// Tightly packed RGBA, `width * height * 4`.
    pub rgba: Vec<u8>,
    pub width: usize,
    pub height: usize,
    /// Millimetres of wall per output pixel (isotropic).
    pub mm_per_px: f64,
    /// Wall-plane mm coordinate of the output's top-left pixel corner.
    pub origin_mm: [f64; 2],
    /// IDs of the markers used, anchor first.
    pub marker_ids: Vec<u16>,
    /// True when a second marker was detected in the frame but its
    /// constraint had to be discarded (implausible back-projection or an
    /// inconsistent joint fit) — the result silently degraded to
    /// single-marker extrapolation, which the UI must surface.
    pub second_marker_rejected: bool,
    /// Homography estimate (wall mm -> source px) with residuals.
    pub estimate: Estimate,
}

impl Rectified {
    /// Convert an output pixel position to wall-plane millimetres.
    pub fn px_to_mm(&self, px: f64, py: f64) -> [f64; 2] {
        [
            self.origin_mm[0] + px * self.mm_per_px,
            self.origin_mm[1] + py * self.mm_per_px,
        ]
    }
}

/// World-plane (mm) corner coordinates of the anchor marker: printed
/// top-left at the origin, clockwise.
fn anchor_corners_mm(side_mm: f64) -> [[f64; 2]; 4] {
    [[0.0, 0.0], [side_mm, 0.0], [side_mm, side_mm], [0.0, side_mm]]
}

/// Estimate the wall-plane homography (mm -> px) from the detected markers.
///
/// One marker gives the exact 4-point DLT + LM path. When both Reference
/// Markers are visible, the second marker's wall position is recovered by
/// back-projecting its corners through the anchor's homography and fitting a
/// rigid square of the known physical side (its size is the extra metric
/// constraint), then all 8 correspondences go through the general RANSAC +
/// LM path — the same estimation core issue #4 chains across keyframes.
fn estimate_wall_homography(
    markers: &[DetectedMarker],
    side_mm: f64,
) -> Option<(Estimate, Vec<u16>, bool)> {
    let anchor = markers.first()?;
    let world = anchor_corners_mm(side_mm);
    let anchor_est = estimate(&world, &anchor.corners, INLIER_THRESHOLD_PX)?;
    if markers.len() == 1 {
        return Some((anchor_est, vec![anchor.id], false));
    }

    // The second marker's wall pose (translation + rotation on the plane) is
    // a nuisance parameter: alternate between (a) rigid-fitting its known-
    // size square to the corners back-projected through the current
    // homography and (b) re-estimating the homography from all 8
    // correspondences, until the joint fit settles. Fixing the pose once
    // from the anchor-only estimate would lock in that estimate's
    // extrapolation error.
    let second = &markers[1];
    let local = anchor_corners_mm(side_mm);
    let mut dst: Vec<[f64; 2]> = anchor.corners.to_vec();
    dst.extend_from_slice(&second.corners);
    let mut current = anchor_est.clone();
    let mut best: Option<Estimate> = None;
    for _ in 0..10 {
        let h_inv = current.h.inverse()?;
        let mut plane_pts = [[0.0f64; 2]; 4];
        for (i, c) in second.corners.iter().enumerate() {
            let (x, y) = h_inv.apply(c[0], c[1])?;
            if x.abs() > MAX_EXTENT_MM * 2.0 || y.abs() > MAX_EXTENT_MM * 2.0 {
                // Second marker back-projects implausibly far — likely a
                // bad detection; fall back to the anchor alone, flagged so
                // the UI can say so (CONTEXT.md: we don't guess silently).
                return Some((anchor_est, vec![anchor.id], true));
            }
            plane_pts[i] = [x, y];
        }
        let fitted = fit_rigid_square(&local, &plane_pts);
        let mut src: Vec<[f64; 2]> = world.to_vec();
        src.extend_from_slice(&fitted);
        let Some(est) = estimate(&src, &dst, INLIER_THRESHOLD_PX) else { break };
        if est.inliers < 6 {
            break; // joint model inconsistent — keep whatever we have
        }
        let done = best.as_ref().is_some_and(|b| b.rms - est.rms < 1e-4);
        if best.as_ref().is_none_or(|b| est.rms <= b.rms) {
            current = est.clone();
            best = Some(est);
        }
        if done {
            break;
        }
    }
    match best {
        Some(est) => Some((est, vec![anchor.id, second.id], false)),
        None => Some((anchor_est, vec![anchor.id], true)),
    }
}

/// 2D Kabsch with fixed scale: rotation + translation mapping `local` onto
/// `target` in the least-squares sense.
fn fit_rigid_square(local: &[[f64; 2]; 4], target: &[[f64; 2]; 4]) -> [[f64; 2]; 4] {
    let cen = |pts: &[[f64; 2]; 4]| -> [f64; 2] {
        let mut c = [0.0, 0.0];
        for p in pts {
            c[0] += p[0] / 4.0;
            c[1] += p[1] / 4.0;
        }
        c
    };
    let cl = cen(local);
    let ct = cen(target);
    let (mut dot, mut cross) = (0.0, 0.0);
    for i in 0..4 {
        let lx = local[i][0] - cl[0];
        let ly = local[i][1] - cl[1];
        let tx = target[i][0] - ct[0];
        let ty = target[i][1] - ct[1];
        dot += lx * tx + ly * ty;
        cross += lx * ty - ly * tx;
    }
    let theta = cross.atan2(dot);
    let (s, c) = theta.sin_cos();
    let mut out = [[0.0f64; 2]; 4];
    for i in 0..4 {
        let lx = local[i][0] - cl[0];
        let ly = local[i][1] - cl[1];
        out[i] = [ct[0] + c * lx - s * ly, ct[1] + s * lx + c * ly];
    }
    out
}

fn mean_marker_side_px(m: &DetectedMarker) -> f64 {
    let mut total = 0.0;
    for i in 0..4 {
        let p = m.corners[i];
        let q = m.corners[(i + 1) % 4];
        total += ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt();
    }
    total / 4.0
}

/// Full still path: detect Reference Markers in an RGBA frame, estimate the
/// wall-plane homography, and render the Rectified Wall Image.
///
/// `correction_factor` is the session print-scale factor (ADR-0002): the
/// marker's true side is `MARKER_SIDE_MM * correction_factor`.
pub fn rectify_frame(
    rgba: &[u8],
    width: usize,
    height: usize,
    correction_factor: f64,
) -> Option<Rectified> {
    debug_assert_eq!(rgba.len(), width * height * 4);
    if !(correction_factor.is_finite() && correction_factor > 0.0) {
        return None;
    }
    let mut gray = vec![0u8; width * height];
    crate::grayscale(rgba, &mut gray);

    let mut markers = detect_markers(&gray, width, height);
    if markers.is_empty() {
        return None;
    }
    // Deterministic anchor: prefer the LEFT marker (ID 0) when both are seen.
    markers.sort_by_key(|m| m.id);

    let side_mm = MARKER_SIDE_MM * correction_factor;
    let (est, marker_ids, second_marker_rejected) =
        estimate_wall_homography(&markers, side_mm)?;
    let h_inv = est.h.inverse()?;

    // Output extent: back-project the frame corners onto the wall plane,
    // clamp to a sane distance from the anchor marker.
    let frame_corners = [
        [0.0, 0.0],
        [width as f64, 0.0],
        [width as f64, height as f64],
        [0.0, height as f64],
    ];
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for fc in frame_corners {
        if let Some((x, y)) = h_inv.apply(fc[0], fc[1]) {
            min_x = min_x.min(x.clamp(-MAX_EXTENT_MM, MAX_EXTENT_MM));
            max_x = max_x.max(x.clamp(-MAX_EXTENT_MM, MAX_EXTENT_MM));
            min_y = min_y.min(y.clamp(-MAX_EXTENT_MM, MAX_EXTENT_MM));
            max_y = max_y.max(y.clamp(-MAX_EXTENT_MM, MAX_EXTENT_MM));
        }
    }
    if !(min_x.is_finite() && min_y.is_finite() && max_x > min_x && max_y > min_y) {
        return None;
    }

    // Resolution: match the marker's native detail (side_mm over its px
    // side), but never exceed MAX_OUTPUT_PX in either dimension.
    let native_mm_per_px = side_mm / mean_marker_side_px(&markers[0]).max(1.0);
    let mm_per_px = native_mm_per_px
        .max((max_x - min_x) / MAX_OUTPUT_PX as f64)
        .max((max_y - min_y) / MAX_OUTPUT_PX as f64);
    let out_w = (((max_x - min_x) / mm_per_px).round() as usize).clamp(8, MAX_OUTPUT_PX);
    let out_h = (((max_y - min_y) / mm_per_px).round() as usize).clamp(8, MAX_OUTPUT_PX);

    let rgba_out = render(rgba, width, height, &est.h, [min_x, min_y], mm_per_px, out_w, out_h);
    Some(Rectified {
        rgba: rgba_out,
        width: out_w,
        height: out_h,
        mm_per_px,
        origin_mm: [min_x, min_y],
        marker_ids,
        second_marker_rejected,
        estimate: est,
    })
}

/// Inverse warp with bilinear sampling: each output pixel's wall-plane mm
/// position maps through H to a source pixel. Outside the source frame the
/// output is a flat dark gray so the Homeowner sees the captured extent.
fn render(
    rgba: &[u8],
    width: usize,
    height: usize,
    h: &Homography,
    origin_mm: [f64; 2],
    mm_per_px: f64,
    out_w: usize,
    out_h: usize,
) -> Vec<u8> {
    let mut out = vec![0u8; out_w * out_h * 4];
    let channel =
        |plane: &dyn Fn(usize, usize) -> f64, x: f64, y: f64| -> f64 {
            // Bilinear over an arbitrary channel accessor.
            let x0 = x.floor() as usize;
            let y0 = y.floor() as usize;
            let x1 = (x0 + 1).min(width - 1);
            let y1 = (y0 + 1).min(height - 1);
            let fx = x - x0 as f64;
            let fy = y - y0 as f64;
            plane(x0, y0) * (1.0 - fx) * (1.0 - fy)
                + plane(x1, y0) * fx * (1.0 - fy)
                + plane(x0, y1) * (1.0 - fx) * fy
                + plane(x1, y1) * fx * fy
        };
    for j in 0..out_h {
        let wy = origin_mm[1] + (j as f64 + 0.5) * mm_per_px;
        for i in 0..out_w {
            let wx = origin_mm[0] + (i as f64 + 0.5) * mm_per_px;
            let o = (j * out_w + i) * 4;
            match h.apply(wx, wy) {
                Some((sx, sy))
                    if sx >= 0.0
                        && sy >= 0.0
                        && sx <= (width - 1) as f64
                        && sy <= (height - 1) as f64 =>
                {
                    for ch in 0..3 {
                        let plane = |xx: usize, yy: usize| rgba[(yy * width + xx) * 4 + ch] as f64;
                        out[o + ch] = channel(&plane, sx, sy).round().clamp(0.0, 255.0) as u8;
                    }
                    out[o + 3] = 255;
                }
                _ => {
                    out[o] = 24;
                    out[o + 1] = 24;
                    out[o + 2] = 28;
                    out[o + 3] = 255;
                }
            }
        }
    }
    out
}
