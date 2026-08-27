//! Self-calibrated intrinsics (issue #6): focal length and one radial
//! distortion coefficient, refined jointly with the keyframe poses (and
//! therefore the wall->keyframe homographies) over a recorded pan's
//! correspondences.
//!
//! # Why a pan can self-calibrate and a still cannot (ADR-0001)
//!
//! A homography absorbs any focal length: from a single still there is no
//! way to separate "lens" from "pose", so the still path stays pinhole with
//! k1 = 0 (see [`crate::rectify`]). A pan's keyframe set is different: every
//! keyframe must be explained by the SAME camera (one focal, one k1) moving
//! rigidly past a plane, and the Reference Marker corners plus tracked
//! features supply hundreds of well-spread observations. Parametrizing each
//! keyframe as a 6-DoF pose behind a shared K makes the focal observable
//! through the rotation wobble of a hand-held pan (the classic plane-based
//! constraint: H = K·[r1 r2 t] with orthonormal r1, r2 — Zhang/Triggs), and
//! k1 observable from the systematic radial bending of off-centre
//! correspondences. The pan's calibrated intrinsics could later be reused by
//! still captures in the same session; that plumbing is deliberately not
//! built yet.
//!
//! # Distortion model: one-parameter DIVISION model
//!
//! `ideal = centre + (measured - centre) / (1 + k1 · r²)` with
//! `r = |measured - centre| / r0`, r0 the half frame diagonal (so k1 is
//! dimensionless and decoupled from the focal estimate — they correlate only
//! through geometry, not through the parametrization). Chosen over a
//! Brown/polynomial k1 because with a single coefficient the division model
//! fits typical phone barrel distortion at least as well (Fitzgibbon 2001)
//! and BOTH directions are closed-form: undistortion is one division,
//! distortion is the stable root of a quadratic — no iterative inversion in
//! the per-pixel stitching loop. Barrel distortion has k1 < 0. The principal
//! point is fixed at the frame centre (it is not observable from these
//! pans, and phone lenses keep it within a few px of centre).
//!
//! # Degeneracy honesty
//!
//! Self-calibration from a short, barely-rotating, near-fronto-parallel
//! chain is ill-conditioned: the LM cost surface is nearly flat along focal,
//! and a confidently wrong focal is worse than no focal. [`self_calibrate`]
//! therefore refuses to return a result unless the LM curvature bounds the
//! parameters: the 1-sigma focal uncertainty (from the inverse Gauss-Newton
//! Hessian) must be within [`MAX_FOCAL_REL_SIGMA`] of the estimate, k1's
//! within [`MAX_K1_SIGMA`], the chain must have at least
//! [`MIN_CALIB_KEYFRAMES`] keyframes, the refined values must sit in a
//! physically plausible range, and the refinement must not have worsened the
//! reprojection RMS.
//!
//! The two gates fail INDEPENDENTLY, and that asymmetry matters: k1 is
//! observable from radial bending alone, so a low-wobble pan routinely pins
//! k1 to ±0.0002 while leaving the focal in a shallow valley (σ ≫ 10%).
//! Hence the three-way [`CalibOutcome`]: `Full` (both gates passed),
//! `DistortionOnly` (k1 and the bundle's chain are applied, no focal is
//! claimed — a flat cost along focal means the chain is insensitive to it;
//! see the variant docs), `Refused` (nothing provable; the wider
//! uncalibrated Error Bound stands). Discarding a provable k1 was measured
//! to leave the fallback chain fighting real lens distortion the closure
//! cannot absorb — with Error Bounds that did NOT cover the true error.

use crate::linalg::{mat3_inv, solve_in_place};

/// Minimum keyframes before self-calibration is even attempted: shorter
/// chains cannot separate focal from pose regardless of what the curvature
/// check says about the noise realization at hand.
pub const MIN_CALIB_KEYFRAMES: usize = 4;

/// Conditioning gate: claim a calibration only when the LM curvature bounds
/// the focal to within this relative 1-sigma uncertainty. 10% is deliberate:
/// k1 (the parameter that actually straightens geometry and carries its own
/// much tighter gate below) is observable from radial bending alone, while
/// the focal rides on the pan's few degrees of rotation wobble — a typical
/// clean hand-held pan determines it to ~5-8%. Demanding better would
/// discard honest calibrations; anything worse than 10% means the chain is
/// effectively fronto-parallel and the focal would be an artefact.
pub const MAX_FOCAL_REL_SIGMA: f64 = 0.10;

/// Conditioning gate on k1's absolute 1-sigma uncertainty (k1 is
/// dimensionless; typical phone barrel is |k1| ~ 0.03-0.15).
pub const MAX_K1_SIGMA: f64 = 0.02;

/// Plausible focal range in units of the larger frame dimension
/// (~7°-110° horizontal FOV — anything outside is a fit artefact).
const MIN_FOCAL_FRAC: f64 = 0.4;
const MAX_FOCAL_FRAC: f64 = 8.0;

/// |k1| beyond this is not a phone lens; refuse rather than model it.
const MAX_ABS_K1: f64 = 0.35;

/// Marker-corner residuals get this weight in the bundle: sub-pixel corners
/// are the metric anchor, tracked NCC features are noisier and far more
/// numerous.
const MARKER_WEIGHT: f64 = 2.0;

/// Per-link cap on feature matches entering the bundle (evenly subsampled
/// from the link's RANSAC inliers): keeps the numeric-Jacobian LM cheap
/// without starving any link of observations.
pub(crate) const CALIB_MATCHES_PER_LINK: usize = 40;

const LM_ITERATIONS: usize = 120;

// ---------------------------------------------------------------------------
// Division-model radial distortion.

/// One-parameter division-model radial distortion about a fixed centre.
/// `k1 == 0.0` is the exact identity (both mappings return the input).
#[derive(Clone, Copy, Debug)]
pub struct Distortion {
    pub k1: f64,
    /// Distortion centre (= principal point, fixed at the frame centre).
    pub cx: f64,
    pub cy: f64,
    /// Radius normalization (half frame diagonal, px): keeps k1
    /// dimensionless and decoupled from the focal.
    pub r0: f64,
}

impl Distortion {
    pub fn new(width: usize, height: usize, k1: f64) -> Distortion {
        let cx = width as f64 / 2.0;
        let cy = height as f64 / 2.0;
        Distortion { k1, cx, cy, r0: (cx * cx + cy * cy).sqrt().max(1.0) }
    }

    /// The pinhole identity for this frame size.
    pub fn none(width: usize, height: usize) -> Distortion {
        Distortion::new(width, height, 0.0)
    }

    /// Measured (distorted) px -> ideal pinhole px. Closed form.
    pub fn undistort(&self, p: [f64; 2]) -> [f64; 2] {
        if self.k1 == 0.0 {
            return p;
        }
        let dx = p[0] - self.cx;
        let dy = p[1] - self.cy;
        let rn2 = (dx * dx + dy * dy) / (self.r0 * self.r0);
        let mut s = 1.0 + self.k1 * rn2;
        // Pathological k1 could make the divisor vanish inside the frame;
        // clamp rather than explode (the calibration gates reject such k1,
        // but the LM explores parameter space on the way there).
        if s.abs() < 1e-3 {
            s = if s < 0.0 { -1e-3 } else { 1e-3 };
        }
        [self.cx + dx / s, self.cy + dy / s]
    }

    /// Ideal pinhole px -> measured (distorted) px. Closed form: the stable
    /// root of `k1·r_u·r_d² - r_d + r_u = 0`.
    pub fn distort(&self, p: [f64; 2]) -> [f64; 2] {
        if self.k1 == 0.0 {
            return p;
        }
        let dx = p[0] - self.cx;
        let dy = p[1] - self.cy;
        let ru = (dx * dx + dy * dy).sqrt() / self.r0;
        if ru < 1e-12 {
            return p;
        }
        let disc = 1.0 - 4.0 * self.k1 * ru * ru;
        if disc < 0.0 {
            // Beyond the model's invertible range (needs |k1| well past any
            // phone lens at the frame corner): saturate to the identity.
            return p;
        }
        // (1 - sqrt(disc)) / (2 k1 ru) in cancellation-free form.
        let scale = 2.0 / (1.0 + disc.sqrt());
        [self.cx + dx * scale, self.cy + dy * scale]
    }
}

// ---------------------------------------------------------------------------
// Rotations (axis-angle <-> matrix) and the pose -> homography map.

/// Rodrigues: axis-angle (angle = |r|) to a row-major rotation matrix.
fn rodrigues(r: &[f64; 3]) -> [f64; 9] {
    let th2 = r[0] * r[0] + r[1] * r[1] + r[2] * r[2];
    let th = th2.sqrt();
    if th < 1e-12 {
        // First-order: I + [r]x.
        return [1.0, -r[2], r[1], r[2], 1.0, -r[0], -r[1], r[0], 1.0];
    }
    let (s, c) = th.sin_cos();
    let (x, y, z) = (r[0] / th, r[1] / th, r[2] / th);
    let c1 = 1.0 - c;
    [
        c + x * x * c1,
        x * y * c1 - z * s,
        x * z * c1 + y * s,
        y * x * c1 + z * s,
        c + y * y * c1,
        y * z * c1 - x * s,
        z * x * c1 - y * s,
        z * y * c1 + x * s,
        c + z * z * c1,
    ]
}

/// Inverse Rodrigues (rotations here stay well below pi: pan wobble).
fn rodrigues_inv(m: &[f64; 9]) -> [f64; 3] {
    let cos = ((m[0] + m[4] + m[8] - 1.0) / 2.0).clamp(-1.0, 1.0);
    let th = cos.acos();
    let v = [m[7] - m[5], m[2] - m[6], m[3] - m[1]]; // 2 sin(th) * axis
    let sin = (1.0 - cos * cos).sqrt();
    if sin < 1e-9 {
        return [v[0] / 2.0, v[1] / 2.0, v[2] / 2.0];
    }
    let s = th / (2.0 * sin);
    [v[0] * s, v[1] * s, v[2] * s]
}

/// Wall (mm, z = 0) -> ideal px homography of a camera with focal `f`,
/// principal point (cx, cy), rotation `rvec` (axis-angle) and centre `c`
/// (wall frame mm, negative z in front of the wall). Same construction as
/// [`crate::synthetic::pose_homography`], generalized to a full 3-DoF
/// rotation.
fn pose_h(f: f64, cx: f64, cy: f64, rvec: &[f64; 3], c: &[f64; 3]) -> [f64; 9] {
    let r = rodrigues(rvec);
    let t = [
        -(r[0] * c[0] + r[1] * c[1] + r[2] * c[2]),
        -(r[3] * c[0] + r[4] * c[1] + r[5] * c[2]),
        -(r[6] * c[0] + r[7] * c[1] + r[8] * c[2]),
    ];
    // M = [r1 r2 t] (plane z = 0 drops R's third column).
    let m = [r[0], r[1], t[0], r[3], r[4], t[1], r[6], r[7], t[2]];
    [
        f * m[0] + cx * m[6],
        f * m[1] + cx * m[7],
        f * m[2] + cx * m[8],
        f * m[3] + cy * m[6],
        f * m[4] + cy * m[7],
        f * m[5] + cy * m[8],
        m[6],
        m[7],
        m[8],
    ]
}

/// Project through a raw wall->px homography; far-off-plane points get a
/// huge finite value so the LM sees a large (not NaN) residual.
fn project(h: &[f64; 9], x: f64, y: f64) -> [f64; 2] {
    let w = h[6] * x + h[7] * y + h[8];
    if w.abs() < 1e-12 {
        return [1e6, 1e6];
    }
    [
        (h[0] * x + h[1] * y + h[2]) / w,
        (h[3] * x + h[4] * y + h[5]) / w,
    ]
}

/// Closed-form initial focal from the chained wall->px homographies:
/// plane-based calibration with omega = diag(1/f², 1/f², 1) after moving the
/// principal point to the origin. Each (unit-normalized) H yields two linear
/// equations in a = 1/f²; least squares over the chain. Returns `None` when
/// the equations put a on the wrong side of zero (near-fronto-parallel
/// chains do this — the caller falls back to a nominal phone focal).
fn init_focal(w_chain: &[[f64; 9]], cx: f64, cy: f64) -> Option<f64> {
    let (mut uu, mut uv) = (0.0f64, 0.0f64);
    for w in w_chain {
        // Centre: H' = T(-c) · W.
        let mut h = [
            w[0] - cx * w[6],
            w[1] - cx * w[7],
            w[2] - cx * w[8],
            w[3] - cy * w[6],
            w[4] - cy * w[7],
            w[5] - cy * w[8],
            w[6],
            w[7],
            w[8],
        ];
        let norm = h.iter().map(|v| v * v).sum::<f64>().sqrt();
        if norm < 1e-300 {
            continue;
        }
        for v in &mut h {
            *v /= norm;
        }
        let (h11, h12, h21, h22, h31, h32) = (h[0], h[1], h[3], h[4], h[6], h[7]);
        // h1' omega h2 = 0  and  h1' omega h1 = h2' omega h2.
        let eqs = [
            (h11 * h12 + h21 * h22, h31 * h32),
            (h11 * h11 + h21 * h21 - h12 * h12 - h22 * h22, h31 * h31 - h32 * h32),
        ];
        for (u, v) in eqs {
            uu += u * u;
            uv += u * v;
        }
    }
    if uu < 1e-30 {
        return None;
    }
    let a = -uv / uu;
    if !(a.is_finite() && a > 0.0) {
        return None;
    }
    Some(a.sqrt().recip())
}

/// Decompose a wall->px homography into (rvec, camera centre) given a focal.
fn decompose_pose(w: &[f64; 9], f: f64, cx: f64, cy: f64) -> Option<([f64; 3], [f64; 3])> {
    // M = K^-1 · W.
    let m = [
        (w[0] - cx * w[6]) / f,
        (w[1] - cx * w[7]) / f,
        (w[2] - cx * w[8]) / f,
        (w[3] - cy * w[6]) / f,
        (w[4] - cy * w[7]) / f,
        (w[5] - cy * w[8]) / f,
        w[6],
        w[7],
        w[8],
    ];
    let n1 = (m[0] * m[0] + m[3] * m[3] + m[6] * m[6]).sqrt();
    let n2 = (m[1] * m[1] + m[4] * m[4] + m[7] * m[7]).sqrt();
    if !(n1.is_finite() && n2.is_finite()) || n1 + n2 < 1e-12 {
        return None;
    }
    let mut lam = 2.0 / (n1 + n2);
    // The wall origin's depth is t_z: it must be in front of the camera.
    if lam * m[8] < 0.0 {
        lam = -lam;
    }
    let r1 = [lam * m[0], lam * m[3], lam * m[6]];
    let r2 = [lam * m[1], lam * m[4], lam * m[7]];
    let t = [lam * m[2], lam * m[5], lam * m[8]];
    // Orthonormalize (Gram-Schmidt; the LM refines away the remainder).
    let norm = |v: [f64; 3]| -> Option<[f64; 3]> {
        let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if n < 1e-12 {
            return None;
        }
        Some([v[0] / n, v[1] / n, v[2] / n])
    };
    let r1n = norm(r1)?;
    let d = r2[0] * r1n[0] + r2[1] * r1n[1] + r2[2] * r1n[2];
    let r2n = norm([r2[0] - d * r1n[0], r2[1] - d * r1n[1], r2[2] - d * r1n[2]])?;
    let r3 = [
        r1n[1] * r2n[2] - r1n[2] * r2n[1],
        r1n[2] * r2n[0] - r1n[0] * r2n[2],
        r1n[0] * r2n[1] - r1n[1] * r2n[0],
    ];
    let r = [
        r1n[0], r2n[0], r3[0], //
        r1n[1], r2n[1], r3[1], //
        r1n[2], r2n[2], r3[2],
    ];
    // C = -R^T t.
    let c = [
        -(r[0] * t[0] + r[3] * t[1] + r[6] * t[2]),
        -(r[1] * t[0] + r[4] * t[1] + r[7] * t[2]),
        -(r[2] * t[0] + r[5] * t[1] + r[8] * t[2]),
    ];
    Some((rodrigues_inv(&r), c))
}

// ---------------------------------------------------------------------------
// The joint bundle.

/// Everything the pan pipeline hands to [`self_calibrate`]. All point
/// coordinates are RAW measured px (the bundle applies its own distortion
/// model); `w_chain` is the uncalibrated wall->px chain used only for
/// initialization.
pub(crate) struct CalibInput<'a> {
    pub width: usize,
    pub height: usize,
    /// Physical marker side (mm, print-scale corrected).
    pub side_mm: f64,
    /// Starting k1 for the LM (from the caller's coarse bootstrap scan; the
    /// chain below must be consistent with it — i.e. built on points
    /// undistorted with this k1).
    pub k1_init: f64,
    pub w_chain: &'a [[f64; 9]],
    /// Marker A observations: (keyframe index, measured corners px). A's
    /// wall corners are known — they anchor the gauge (origin + scale).
    ///
    /// Marker B is deliberately NOT an observation: it stays the pan's
    /// INDEPENDENT far-end check — the loop-closure measurement that feeds
    /// the Error Bound (see [`crate::pan`]) — rather than being absorbed
    /// into the very model it is supposed to validate.
    pub marker_a: &'a [(usize, [[f64; 2]; 4])],
    /// Per-link RANSAC-inlier matches: (points in keyframe i, matched points
    /// in keyframe i+1).
    pub links: &'a [(Vec<[f64; 2]>, Vec<[f64; 2]>)],
}

/// A self-calibration that passed every conditioning gate.
#[derive(Clone, Debug)]
pub struct SelfCalibration {
    pub focal_px: f64,
    pub k1: f64,
    /// 1-sigma uncertainties from the LM curvature (Gauss-Newton Hessian
    /// inverse scaled by the residual variance).
    pub focal_sigma_px: f64,
    pub k1_sigma: f64,
    /// Weighted bundle reprojection RMS (px) at the winning multi-start
    /// initialization (which already includes the caller's bootstrap k1 —
    /// NOT the pinhole pass) and after joint refinement; `after <= before`
    /// is one of the acceptance gates.
    pub rms_before_px: f64,
    pub rms_after_px: f64,
}

/// What the conditioning gates allowed [`self_calibrate`] to claim (see the
/// module docs on why the two gates fail independently).
pub(crate) enum CalibOutcome {
    /// Both gates passed: full calibration plus the wall->ideal-px chain
    /// rebuilt from the bundle's jointly refined poses.
    Full(SelfCalibration, Vec<[f64; 9]>),
    /// k1 passed its gate but the focal did not (chain too fronto-parallel
    /// to pin it): the distortion is real and well-determined, the focal
    /// would be an artefact — no focal is ever reported. The bundle's chain
    /// IS still returned and used: an unidentifiable focal means the cost
    /// is flat along it, i.e. the fitted wall->ideal-px homographies are
    /// insensitive to where in the valley the focal sits — the chain is
    /// well-determined even when the focal is not (the wrongness lives in
    /// the pose decomposition, which nothing downstream consumes). The
    /// alternative — re-running the PAIRWISE chain on undistorted points —
    /// was measured to compound a systematic ~1%/link bias into a ~10%
    /// scale error at Marker B on low-wobble pans (constant overlap
    /// geometry, so per-link bias never cancels), which the closure
    /// plausibility guard rightly refuses. Marker B's loop closure stays
    /// the independent downstream check on this chain either way.
    DistortionOnly { k1: f64, chain: Vec<[f64; 9]> },
    /// Nothing provable: stay pinhole with the wider uncalibrated bound.
    Refused,
}

/// Parameter layout: [f, k1, (rvec, C) * n].
struct Bundle<'a> {
    input: &'a CalibInput<'a>,
    n: usize,
    world_a: [[f64; 2]; 4],
}

impl Bundle<'_> {
    fn param_count(&self) -> usize {
        2 + 6 * self.n
    }

    /// Evaluate all residuals into `out` (cleared first).
    fn residuals(&self, p: &[f64], out: &mut Vec<f64>) {
        out.clear();
        let f = p[0];
        let dist = Distortion {
            k1: p[1],
            cx: self.input.width as f64 / 2.0,
            cy: self.input.height as f64 / 2.0,
            r0: Distortion::new(self.input.width, self.input.height, 0.0).r0,
        };
        let (cx, cy) = (dist.cx, dist.cy);
        let mut w = Vec::with_capacity(self.n);
        let mut winv = Vec::with_capacity(self.n);
        for i in 0..self.n {
            let rv = [p[2 + 6 * i], p[3 + 6 * i], p[4 + 6 * i]];
            let c = [p[5 + 6 * i], p[6 + 6 * i], p[7 + 6 * i]];
            let wi = pose_h(f, cx, cy, &rv, &c);
            winv.push(mat3_inv(&wi));
            w.push(wi);
        }
        let push_pt = |pred: [f64; 2], meas: [f64; 2], weight: f64, out: &mut Vec<f64>| {
            out.push(weight * (pred[0] - meas[0]).clamp(-1e6, 1e6));
            out.push(weight * (pred[1] - meas[1]).clamp(-1e6, 1e6));
        };
        // Marker A: known wall corners.
        for (kf, corners) in self.input.marker_a {
            for k in 0..4 {
                let pred = dist.distort(project(&w[*kf], self.world_a[k][0], self.world_a[k][1]));
                push_pt(pred, corners[k], MARKER_WEIGHT, out);
            }
        }
        // Tracked features: transfer error through the wall plane,
        // frame i -> frame i+1 (points on the plane need no per-point
        // parameters — the plane and the two poses determine the transfer).
        for (i, (src, dst)) in self.input.links.iter().enumerate() {
            match &winv[i] {
                Some(inv) => {
                    for (pt, q) in src.iter().zip(dst) {
                        let pu = dist.undistort(*pt);
                        let wall = project(inv, pu[0], pu[1]);
                        let pred = dist.distort(project(&w[i + 1], wall[0], wall[1]));
                        push_pt(pred, *q, 1.0, out);
                    }
                }
                None => {
                    for _ in 0..src.len() {
                        out.push(1e4);
                        out.push(1e4);
                    }
                }
            }
        }
    }
}

/// Forward-difference step for parameter `j` (layout: f, k1, 6-DoF poses).
fn param_step(j: usize, p: &[f64]) -> f64 {
    if j == 0 {
        1e-6 * p[0].abs().max(1.0) // focal (px)
    } else if j == 1 {
        1e-6 // k1 (dimensionless, |k1| <~ 0.35)
    } else if (j - 2) % 6 < 3 {
        1e-7 // rotation (rad)
    } else {
        1e-4 * p[j].abs().max(10.0) // positions (mm)
    }
}

/// Numeric-Jacobian Levenberg-Marquardt on the bundle from one starting
/// point. Returns the refined parameters, final cost and final residuals.
fn run_lm(
    bundle: &Bundle<'_>,
    mut params: Vec<f64>,
    mut r: Vec<f64>,
    mut cost: f64,
    np: usize,
    m: usize,
) -> (Vec<f64>, f64, Vec<f64>) {
    let mut jac = vec![0.0f64; m * np];
    let mut jtj = vec![0.0f64; np * np];
    let mut jtr = vec![0.0f64; np];
    let mut r_try = Vec::with_capacity(m);
    let mut r_pert = Vec::with_capacity(m);
    let mut lambda = 1e-3;

    for _iter in 0..LM_ITERATIONS {
        for j in 0..np {
            let step = param_step(j, &params);
            let mut pj = params.clone();
            pj[j] += step;
            bundle.residuals(&pj, &mut r_pert);
            for i in 0..m {
                jac[i * np + j] = (r_pert[i] - r[i]) / step;
            }
        }
        jtj.fill(0.0);
        jtr.fill(0.0);
        for i in 0..m {
            let row = &jac[i * np..(i + 1) * np];
            let ri = r[i];
            for a in 0..np {
                let ja = row[a];
                if ja == 0.0 {
                    continue;
                }
                jtr[a] += ja * ri;
                for b in a..np {
                    jtj[a * np + b] += ja * row[b];
                }
            }
        }
        for a in 0..np {
            for b in 0..a {
                jtj[a * np + b] = jtj[b * np + a];
            }
        }

        let mut improved = false;
        for _try in 0..8 {
            let mut a = jtj.clone();
            for d in 0..np {
                a[d * np + d] += lambda * jtj[d * np + d].max(1e-12);
            }
            let mut delta: Vec<f64> = jtr.iter().map(|v| -v).collect();
            if !solve_in_place(&mut a, &mut delta, np) {
                lambda *= 10.0;
                continue;
            }
            let mut p_try = params.clone();
            for d in 0..np {
                p_try[d] += delta[d];
            }
            bundle.residuals(&p_try, &mut r_try);
            let cost_try: f64 = r_try.iter().map(|v| v * v).sum();
            if cost_try < cost {
                params = p_try;
                std::mem::swap(&mut r, &mut r_try);
                let rel = (cost - cost_try) / cost.max(1e-30);
                cost = cost_try;
                lambda = (lambda * 0.3).max(1e-12);
                improved = true;
                if rel < 1e-10 {
                    lambda = -1.0; // converged: sentinel breaks the outer loop
                }
                break;
            }
            lambda *= 10.0;
        }
        if !improved || lambda < 0.0 {
            break;
        }
    }
    (params, cost, r)
}

/// Jointly refine {focal, k1, keyframe poses (=> homographies)} by LM over
/// the pan's correspondences, then apply the conditioning gates. On full
/// success returns the calibration AND the refined wall->ideal-px chain
/// rebuilt from the bundle's poses (`pose_h` per keyframe) — jointly optimal
/// over ALL observations at once, unlike pairwise link composition, which
/// compounds per-link perspective errors multiplicatively along the chain.
/// See [`CalibOutcome`] for the partial (k1-only) and refused outcomes.
pub(crate) fn self_calibrate(input: &CalibInput<'_>) -> CalibOutcome {
    let n = input.w_chain.len();
    if n < MIN_CALIB_KEYFRAMES || input.marker_a.is_empty() || input.links.len() != n - 1 {
        return CalibOutcome::Refused;
    }
    let cx = input.width as f64 / 2.0;
    let cy = input.height as f64 / 2.0;
    let dim = input.width.max(input.height) as f64;

    // --- Initialization.
    let side = input.side_mm;
    let world_a = [[0.0, 0.0], [side, 0.0], [side, side], [0.0, side]];

    let bundle = Bundle { input, n, world_a };
    let np = bundle.param_count();

    // Focal starting points: the closed-form plane-calibration estimate
    // when it lands in range, plus nominal phone-ish brackets. The cost
    // along focal is a long shallow valley on low-wobble pans, so a single
    // bad start can settle in a wrong basin; several cheap starts + keep
    // the lowest cost is the insurance.
    let mut f0s: Vec<f64> = Vec::new();
    if let Some(fi) = init_focal(input.w_chain, cx, cy)
        .filter(|f| (MIN_FOCAL_FRAC * dim..=MAX_FOCAL_FRAC * dim).contains(f))
    {
        f0s.push(fi);
    }
    for cand in [0.7 * dim, 1.05 * dim, 1.6 * dim] {
        if f0s.iter().all(|f| (f - cand).abs() / cand > 0.10) {
            f0s.push(cand);
        }
    }

    let init_params = |f0: f64| -> Option<Vec<f64>> {
        let mut params = vec![0.0f64; 2 + 6 * n];
        params[0] = f0;
        params[1] = input.k1_init;
        for i in 0..n {
            let (rv, c) = decompose_pose(&input.w_chain[i], f0, cx, cy)?;
            params[2 + 6 * i..5 + 6 * i].copy_from_slice(&rv);
            params[5 + 6 * i..8 + 6 * i].copy_from_slice(&c);
        }
        Some(params)
    };

    let mut best: Option<(Vec<f64>, f64, Vec<f64>, f64)> = None; // params, cost, r, rms_before
    let mut m_total = 0usize;
    for f0 in f0s {
        let Some(params0) = init_params(f0) else { continue };
        let mut r = Vec::new();
        bundle.residuals(&params0, &mut r);
        let m = r.len();
        if m < np + 20 {
            // Not enough observations to also certify a result.
            return CalibOutcome::Refused;
        }
        m_total = m;
        let cost0: f64 = r.iter().map(|v| v * v).sum();
        let rms_before = (cost0 / m as f64).sqrt();
        let (params, cost, r) = run_lm(&bundle, params0, r, cost0, np, m);
        if best.as_ref().is_none_or(|b| cost < b.1) {
            best = Some((params, cost, r, rms_before));
        }
    }
    let Some((params, cost, r, rms_before)) = best else {
        return CalibOutcome::Refused;
    };
    let m = m_total;

    let mut jac = vec![0.0f64; m * np];
    let mut jtj = vec![0.0f64; np * np];
    let mut r_pert = Vec::with_capacity(m);

    // --- Conditioning gates (see module docs). The final Jacobian is
    // recomputed at the accepted parameters for an honest curvature.
    let f = params[0];
    let k1 = params[1];
    if !(f.is_finite() && k1.is_finite()) || k1.abs() > MAX_ABS_K1 {
        return CalibOutcome::Refused;
    }
    let rms_after = (cost / m as f64).sqrt();
    if rms_after > rms_before {
        return CalibOutcome::Refused; // refinement must not have made things worse
    }
    for j in 0..np {
        let step = param_step(j, &params);
        let mut pj = params.clone();
        pj[j] += step;
        bundle.residuals(&pj, &mut r_pert);
        for i in 0..m {
            jac[i * np + j] = (r_pert[i] - r[i]) / step;
        }
    }
    jtj.fill(0.0);
    for i in 0..m {
        let row = &jac[i * np..(i + 1) * np];
        for a in 0..np {
            let ja = row[a];
            if ja == 0.0 {
                continue;
            }
            for b in a..np {
                jtj[a * np + b] += ja * row[b];
            }
        }
    }
    for a in 0..np {
        for b in 0..a {
            jtj[a * np + b] = jtj[b * np + a];
        }
    }
    let sigma2 = cost / (m - np) as f64;
    let var_of = |idx: usize| -> Option<f64> {
        let mut a = jtj.clone();
        let mut e = vec![0.0f64; np];
        e[idx] = 1.0;
        if !solve_in_place(&mut a, &mut e, np) {
            return None;
        }
        let v = sigma2 * e[idx];
        (v.is_finite() && v > 0.0).then_some(v)
    };
    let (Some(focal_var), Some(k1_var)) = (var_of(0), var_of(1)) else {
        return CalibOutcome::Refused; // singular curvature: nothing certifiable
    };
    let focal_sigma = focal_var.sqrt();
    let k1_sigma = k1_var.sqrt();
    if k1_sigma > MAX_K1_SIGMA {
        return CalibOutcome::Refused; // ill-conditioned: a confident number would be a lie
    }
    // Rebuild the wall->ideal-px chain from the refined poses. Valid on
    // both outcomes below: when the focal gate fails, the cost was flat
    // along focal, so these composite homographies are insensitive to the
    // unclaimed focal (see [`CalibOutcome::DistortionOnly`]).
    let chain: Vec<[f64; 9]> = (0..n)
        .map(|i| {
            let rv = [params[2 + 6 * i], params[3 + 6 * i], params[4 + 6 * i]];
            let c = [params[5 + 6 * i], params[6 + 6 * i], params[7 + 6 * i]];
            pose_h(f, cx, cy, &rv, &c)
        })
        .collect();

    let focal_ok = (MIN_FOCAL_FRAC * dim..=MAX_FOCAL_FRAC * dim).contains(&f)
        && focal_sigma / f <= MAX_FOCAL_REL_SIGMA;
    if !focal_ok {
        // The focal sits in a shallow valley (or drifted out of the
        // physically plausible bracket) but k1 proved itself on radial
        // bending alone — hand over k1 and the chain, claim no focal.
        return CalibOutcome::DistortionOnly { k1, chain };
    }

    CalibOutcome::Full(
        SelfCalibration {
            focal_px: f,
            k1,
            focal_sigma_px: focal_sigma,
            k1_sigma,
            rms_before_px: rms_before,
            rms_after_px: rms_after,
        },
        chain,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distort_undistort_round_trip() {
        for k1 in [-0.15, -0.05, 0.0, 0.04] {
            let d = Distortion::new(640, 360, k1);
            for p in [[10.0, 10.0], [320.0, 180.0], [600.0, 40.0], [50.0, 340.0]] {
                let q = d.undistort(d.distort(p));
                assert!(
                    (q[0] - p[0]).abs() < 1e-9 && (q[1] - p[1]).abs() < 1e-9,
                    "round trip failed for k1={k1}, p={p:?} -> {q:?}"
                );
                let q2 = d.distort(d.undistort(p));
                assert!((q2[0] - p[0]).abs() < 1e-9 && (q2[1] - p[1]).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn barrel_distortion_pulls_points_toward_the_centre() {
        let d = Distortion::new(640, 360, -0.10);
        let ideal = [630.0, 350.0]; // near the corner
        let meas = d.distort(ideal);
        let r_ideal = ((ideal[0] - 320.0f64).powi(2) + (ideal[1] - 180.0).powi(2)).sqrt();
        let r_meas = ((meas[0] - 320.0f64).powi(2) + (meas[1] - 180.0).powi(2)).sqrt();
        assert!(r_meas < r_ideal, "barrel (k1<0) must pull inward: {r_meas} vs {r_ideal}");
    }

    #[test]
    fn rodrigues_round_trip() {
        for rv in [
            [0.0, 0.0, 0.0],
            [0.05, -0.02, 0.01],
            [0.0, 0.4, 0.0],
            [-0.3, 0.1, 0.25],
        ] {
            let r = rodrigues(&rv);
            let back = rodrigues_inv(&r);
            for k in 0..3 {
                assert!((back[k] - rv[k]).abs() < 1e-9, "{rv:?} -> {back:?}");
            }
        }
    }

    #[test]
    fn pose_decomposition_round_trips_through_the_homography() {
        let (f, cx, cy) = (700.0, 320.0, 180.0);
        let rv = [0.02, -0.06, 0.015];
        let c = [800.0, 300.0, -1400.0];
        let w = pose_h(f, cx, cy, &rv, &c);
        let (rv2, c2) = decompose_pose(&w, f, cx, cy).expect("decompose");
        for k in 0..3 {
            assert!((rv2[k] - rv[k]).abs() < 1e-9, "rvec {rv:?} -> {rv2:?}");
            assert!((c2[k] - c[k]).abs() < 1e-6, "centre {c:?} -> {c2:?}");
        }
    }

    #[test]
    fn init_focal_recovers_truth_from_rotated_views() {
        let (f, cx, cy) = (700.0f64, 320.0, 180.0);
        let mut chain = Vec::new();
        for i in 0..8 {
            let yaw = -0.08 + 0.02 * i as f64;
            let pitch = 0.03 * ((i % 3) as f64 - 1.0);
            let rv = [pitch, yaw, 0.0];
            let c = [400.0 * i as f64, 280.0, -1400.0];
            chain.push(pose_h(f, cx, cy, &rv, &c));
        }
        let f_est = init_focal(&chain, cx, cy).expect("solvable");
        assert!(
            (f_est - f).abs() / f < 0.02,
            "closed-form focal {f_est:.1} vs true {f:.1}"
        );
    }
}
