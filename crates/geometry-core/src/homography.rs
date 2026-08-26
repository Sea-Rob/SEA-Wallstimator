//! Planar homography estimation: normalized DLT, RANSAC for over-determined
//! sets, and Levenberg-Marquardt refinement minimizing reprojection error.
//!
//! Reusable estimation core: the still-frame rectifier (issue #3) feeds it 4
//! or 8 Reference Marker corners, and keyframe chaining (issue #4) will feed
//! it tracked correspondences between frames. Coordinates are unit-agnostic —
//! the still path maps wall-plane millimetres to image pixels.

use crate::linalg::{mat3_inv, mat3_mul, smallest_eigenpair_sym, solve_in_place};

/// A 3x3 planar homography, row-major.
#[derive(Clone, Copy, Debug)]
pub struct Homography(pub [f64; 9]);

impl Homography {
    /// Map a point; `None` if it lands on the plane at infinity.
    pub fn apply(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let h = &self.0;
        let w = h[6] * x + h[7] * y + h[8];
        let scale = h.iter().fold(0.0f64, |acc, v| acc.max(v.abs()));
        if w.abs() < 1e-12 * scale.max(1e-300) {
            return None;
        }
        Some((
            (h[0] * x + h[1] * y + h[2]) / w,
            (h[3] * x + h[4] * y + h[5]) / w,
        ))
    }

    pub fn inverse(&self) -> Option<Homography> {
        mat3_inv(&self.0).map(Homography)
    }
}

/// Hartley normalization: translate points to zero centroid, scale mean
/// distance from origin to sqrt(2). Returns the transform T (point' = T·point).
fn normalizing_transform(pts: &[[f64; 2]]) -> Option<[f64; 9]> {
    let n = pts.len() as f64;
    let (mut cx, mut cy) = (0.0, 0.0);
    for p in pts {
        cx += p[0];
        cy += p[1];
    }
    cx /= n;
    cy /= n;
    let mut mean_dist = 0.0;
    for p in pts {
        mean_dist += ((p[0] - cx).powi(2) + (p[1] - cy).powi(2)).sqrt();
    }
    mean_dist /= n;
    if mean_dist < 1e-12 {
        return None; // all points coincide
    }
    let s = std::f64::consts::SQRT_2 / mean_dist;
    Some([s, 0.0, -s * cx, 0.0, s, -s * cy, 0.0, 0.0, 1.0])
}

/// Direct Linear Transform with Hartley normalization. Needs >= 4
/// correspondences `src[i] -> dst[i]`; returns the homography with unit
/// Frobenius norm, or `None` on degenerate input.
pub fn dlt(src: &[[f64; 2]], dst: &[[f64; 2]]) -> Option<Homography> {
    let n = src.len();
    if n < 4 || dst.len() != n {
        return None;
    }
    let t_src = normalizing_transform(src)?;
    let t_dst = normalizing_transform(dst)?;
    let norm = |t: &[f64; 9], p: &[f64; 2]| {
        [t[0] * p[0] + t[1] * p[1] + t[2], t[4] * p[1] + t[3] * p[0] + t[5]]
    };

    // Accumulate A^T A directly (9x9 symmetric) instead of storing 2N x 9.
    let mut ata = [0.0f64; 81];
    let mut add_row = |row: [f64; 9]| {
        for i in 0..9 {
            if row[i] == 0.0 {
                continue;
            }
            for j in 0..9 {
                ata[i * 9 + j] += row[i] * row[j];
            }
        }
    };
    for i in 0..n {
        let s = norm(&t_src, &src[i]);
        let d = norm(&t_dst, &dst[i]);
        let (x, y, u, v) = (s[0], s[1], d[0], d[1]);
        add_row([-x, -y, -1.0, 0.0, 0.0, 0.0, u * x, u * y, u]);
        add_row([0.0, 0.0, 0.0, -x, -y, -1.0, v * x, v * y, v]);
    }

    let trace: f64 = (0..9).map(|i| ata[i * 9 + i]).sum();
    let (h_norm, _lambda_min, lambda_second) = smallest_eigenpair_sym(&mut ata, 9);
    // Degenerate correspondence sets (e.g. 3+ collinear points) leave a
    // multi-dimensional nullspace: the second-smallest eigenvalue is as tiny
    // as the smallest, and the returned vector is an arbitrary nullspace
    // element — a rank-deficient "H" that reprojects the input with
    // deceptively perfect residuals. Reject when the second eigenvalue
    // vanishes relative to the matrix scale (well-conditioned sets sit many
    // orders of magnitude above this).
    if lambda_second < 1e-12 * trace.max(f64::MIN_POSITIVE) {
        return None;
    }
    let mut hn = [0.0; 9];
    hn.copy_from_slice(&h_norm);

    // Denormalize: H = T_dst^-1 · Hn · T_src.
    let t_dst_inv = mat3_inv(&t_dst)?;
    let mut h = mat3_mul(&mat3_mul(&t_dst_inv, &hn), &t_src);
    // Scale to unit Frobenius norm for numeric stability downstream.
    let norm2: f64 = h.iter().map(|v| v * v).sum();
    if norm2 < 1e-300 {
        return None;
    }
    let inv_norm = norm2.sqrt().recip();
    for v in &mut h {
        *v *= inv_norm;
    }
    Some(Homography(h))
}

/// Per-point reprojection errors |H·src - dst| in dst units.
pub fn reprojection_errors(h: &Homography, src: &[[f64; 2]], dst: &[[f64; 2]]) -> Vec<f64> {
    src.iter()
        .zip(dst)
        .map(|(s, d)| match h.apply(s[0], s[1]) {
            Some((u, v)) => ((u - d[0]).powi(2) + (v - d[1]).powi(2)).sqrt(),
            None => f64::INFINITY,
        })
        .collect()
}

/// Levenberg-Marquardt refinement of a homography, minimizing the sum of
/// squared reprojection errors. Parametrizes the 8 DoF as h / h[8].
///
/// When h[8] is (near-)zero the h[8]=1 chart is invalid — h[8] is the
/// projective weight w at the source origin, and general chained
/// homographies (issue #4) can legitimately put the origin near the
/// vanishing line. Rather than silently returning the unrefined input, the
/// source frame is re-anchored: translate it so a point with a healthy
/// weight becomes the origin (H' = H·T has h'[8] = w(p)), refine there, and
/// map the result back (H* = H'*·T⁻¹).
pub fn refine_lm(h0: &Homography, src: &[[f64; 2]], dst: &[[f64; 2]]) -> Homography {
    let h = h0.0;
    let scale = h.iter().fold(0.0f64, |acc, v| acc.max(v.abs()));
    if h[8].abs() < 1e-6 * scale.max(1e-300) {
        // Candidate anchors: the src centroid, then the src points themselves
        // (each maps to a finite dst point, so at least one has w far from 0).
        let n = src.len() as f64;
        let centroid = src.iter().fold([0.0, 0.0], |c, p| [c[0] + p[0] / n, c[1] + p[1] / n]);
        let anchor = std::iter::once(centroid)
            .chain(src.iter().copied())
            .find(|p| (h[6] * p[0] + h[7] * p[1] + h[8]).abs() > 1e-3 * scale);
        let Some(p) = anchor else {
            return *h0; // every candidate sits on the vanishing line: give up
        };
        let t = [1.0, 0.0, p[0], 0.0, 1.0, p[1], 0.0, 0.0, 1.0];
        let t_inv = [1.0, 0.0, -p[0], 0.0, 1.0, -p[1], 0.0, 0.0, 1.0];
        let shifted = Homography(mat3_mul(&h, &t));
        let s2 = shifted.0.iter().fold(0.0f64, |acc, v| acc.max(v.abs()));
        if shifted.0[8].abs() < 1e-6 * s2.max(1e-300) {
            return *h0; // re-anchoring did not produce a valid chart
        }
        let src_shifted: Vec<[f64; 2]> = src.iter().map(|s| [s[0] - p[0], s[1] - p[1]]).collect();
        let refined = refine_lm(&shifted, &src_shifted, dst);
        return Homography(mat3_mul(&refined.0, &t_inv));
    }
    let mut p = [0.0f64; 8];
    for i in 0..8 {
        p[i] = h[i] / h[8];
    }

    let residuals = |p: &[f64; 8], out: &mut Vec<f64>| {
        out.clear();
        let hh = Homography([p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], 1.0]);
        for (s, d) in src.iter().zip(dst) {
            match hh.apply(s[0], s[1]) {
                Some((u, v)) => {
                    out.push(u - d[0]);
                    out.push(v - d[1]);
                }
                None => {
                    out.push(1e6);
                    out.push(1e6);
                }
            }
        }
    };

    let m = src.len() * 2;
    let mut r = Vec::with_capacity(m);
    let mut r_try = Vec::with_capacity(m);
    let mut r_pert = Vec::with_capacity(m);
    residuals(&p, &mut r);
    let mut cost: f64 = r.iter().map(|v| v * v).sum();
    let mut lambda = 1e-3;
    let mut jac = vec![0.0f64; m * 8];

    for _iter in 0..50 {
        // Numeric Jacobian (forward differences). 8 params, tiny system —
        // simpler and plenty accurate at these scales.
        for j in 0..8 {
            let step = 1e-7 * p[j].abs().max(1e-4);
            let mut pj = p;
            pj[j] += step;
            residuals(&pj, &mut r_pert);
            for i in 0..m {
                jac[i * 8 + j] = (r_pert[i] - r[i]) / step;
            }
        }
        // Normal equations JtJ + lambda·diag(JtJ), Jt·r.
        let mut jtj = [0.0f64; 64];
        let mut jtr = [0.0f64; 8];
        for i in 0..m {
            for a in 0..8 {
                let ja = jac[i * 8 + a];
                if ja == 0.0 {
                    continue;
                }
                jtr[a] += ja * r[i];
                for b in a..8 {
                    jtj[a * 8 + b] += ja * jac[i * 8 + b];
                }
            }
        }
        for a in 0..8 {
            for b in 0..a {
                jtj[a * 8 + b] = jtj[b * 8 + a];
            }
        }

        let mut improved = false;
        for _try in 0..8 {
            let mut a = jtj;
            for d in 0..8 {
                a[d * 8 + d] += lambda * jtj[d * 8 + d].max(1e-12);
            }
            let mut delta = [0.0f64; 8];
            for d in 0..8 {
                delta[d] = -jtr[d];
            }
            if !solve_in_place(&mut a, &mut delta, 8) {
                lambda *= 10.0;
                continue;
            }
            let mut p_try = p;
            for d in 0..8 {
                p_try[d] += delta[d];
            }
            residuals(&p_try, &mut r_try);
            let cost_try: f64 = r_try.iter().map(|v| v * v).sum();
            if cost_try < cost {
                p = p_try;
                std::mem::swap(&mut r, &mut r_try);
                cost = cost_try;
                lambda = (lambda * 0.3).max(1e-12);
                improved = true;
                break;
            }
            lambda *= 10.0;
        }
        if !improved || cost < 1e-20 {
            break;
        }
    }
    Homography([p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], 1.0])
}

/// Result of a full estimation run: the refined homography plus the
/// reprojection residuals the issue-#3 API must expose.
#[derive(Clone, Debug)]
pub struct Estimate {
    pub h: Homography,
    /// Per-correspondence reprojection error, dst units (px for the still path).
    pub residuals: Vec<f64>,
    pub rms: f64,
    pub max: f64,
    /// Correspondences the model was fit on (all of them when n == 4).
    pub inliers: usize,
}

fn summarize(h: Homography, src: &[[f64; 2]], dst: &[[f64; 2]], inliers: usize) -> Estimate {
    let residuals = reprojection_errors(&h, src, dst);
    let rms = (residuals.iter().map(|r| r * r).sum::<f64>() / residuals.len() as f64).sqrt();
    let max = residuals.iter().cloned().fold(0.0, f64::max);
    Estimate { h, residuals, rms, max, inliers }
}

/// Deterministic LCG — RANSAC sampling needs no cryptographic randomness and
/// this keeps `getrandom`/JS imports out of the WASM bundle.
struct Lcg(u64);
impl Lcg {
    fn next_below(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}

/// Full estimation pipeline: exact DLT for 4 points, RANSAC + inlier refit
/// for more, then LM refinement. `inlier_threshold` is in dst units.
pub fn estimate(src: &[[f64; 2]], dst: &[[f64; 2]], inlier_threshold: f64) -> Option<Estimate> {
    let n = src.len();
    if n < 4 || dst.len() != n {
        return None;
    }
    if n == 4 {
        let h = dlt(src, dst)?;
        return Some(summarize(refine_lm(&h, src, dst), src, dst, 4));
    }

    // RANSAC over minimal 4-point samples.
    let mut rng = Lcg(0x5EA_1157);
    let mut best_mask: Option<Vec<bool>> = None;
    let mut best_h: Option<Homography> = None;
    let mut best_count = 0usize;
    for _ in 0..200 {
        let mut idx = [0usize; 4];
        let mut k = 0;
        while k < 4 {
            let cand = rng.next_below(n);
            if !idx[..k].contains(&cand) {
                idx[k] = cand;
                k += 1;
            }
        }
        let s: Vec<[f64; 2]> = idx.iter().map(|&i| src[i]).collect();
        let d: Vec<[f64; 2]> = idx.iter().map(|&i| dst[i]).collect();
        let Some(h) = dlt(&s, &d) else { continue };
        let errs = reprojection_errors(&h, src, dst);
        let mask: Vec<bool> = errs.iter().map(|&e| e <= inlier_threshold).collect();
        let count = mask.iter().filter(|&&b| b).count();
        if count > best_count {
            best_count = count;
            best_mask = Some(mask);
            best_h = Some(h);
            if count == n {
                break;
            }
        }
    }
    let mask = best_mask?;
    if best_count < 4 {
        return None;
    }
    let s: Vec<[f64; 2]> = (0..n).filter(|&i| mask[i]).map(|i| src[i]).collect();
    let d: Vec<[f64; 2]> = (0..n).filter(|&i| mask[i]).map(|i| dst[i]).collect();
    // If the full inlier set happens to be degenerate for DLT, fall back to
    // the best minimal-sample model rather than failing the whole estimate.
    let refit = dlt(&s, &d).or_else(|| best_h.take())?;
    let h = refine_lm(&refit, &s, &d);
    Some(summarize(h, src, dst, best_count))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply_true(h: &[f64; 9], p: [f64; 2]) -> [f64; 2] {
        let w = h[6] * p[0] + h[7] * p[1] + h[8];
        [
            (h[0] * p[0] + h[1] * p[1] + h[2]) / w,
            (h[3] * p[0] + h[4] * p[1] + h[5]) / w,
        ]
    }

    // A realistic wall-to-image homography: mm plane to ~640px frame with
    // perspective tilt.
    const H_TRUE: [f64; 9] = [
        1.9, 0.12, 140.0, //
        -0.08, 1.85, 90.0, //
        1.1e-4, -0.9e-4, 1.0,
    ];

    fn square_mm() -> Vec<[f64; 2]> {
        vec![[0.0, 0.0], [150.0, 0.0], [150.0, 150.0], [0.0, 150.0]]
    }

    #[test]
    fn dlt_recovers_exact_homography_from_4_points() {
        let src = square_mm();
        let dst: Vec<[f64; 2]> = src.iter().map(|&p| apply_true(&H_TRUE, p)).collect();
        let h = dlt(&src, &dst).unwrap();
        for (s, d) in src.iter().zip(&dst) {
            let (u, v) = h.apply(s[0], s[1]).unwrap();
            assert!((u - d[0]).abs() < 1e-8 && (v - d[1]).abs() < 1e-8);
        }
        // And it generalizes to a point it was not fit on.
        let far = apply_true(&H_TRUE, [400.0, -80.0]);
        let (u, v) = h.apply(400.0, -80.0).unwrap();
        assert!((u - far[0]).abs() < 1e-6 && (v - far[1]).abs() < 1e-6);
    }

    #[test]
    fn lm_refinement_reduces_noisy_residuals() {
        // 8 points with mild noise on dst; LM must not make things worse and
        // should land near the least-squares optimum.
        let mut src = square_mm();
        src.extend([[300.0, 20.0], [460.0, 10.0], [455.0, 160.0], [305.0, 150.0]]);
        let noise = [0.3, -0.2, 0.25, -0.3, 0.2, 0.15, -0.25, 0.1];
        let dst: Vec<[f64; 2]> = src
            .iter()
            .enumerate()
            .map(|(i, &p)| {
                let d = apply_true(&H_TRUE, p);
                [d[0] + noise[i], d[1] - noise[(i + 3) % 8]]
            })
            .collect();
        let h0 = dlt(&src, &dst).unwrap();
        let cost = |h: &Homography| -> f64 {
            reprojection_errors(h, &src, &dst).iter().map(|e| e * e).sum()
        };
        let h1 = refine_lm(&h0, &src, &dst);
        assert!(cost(&h1) <= cost(&h0) + 1e-12, "LM must not increase cost");
    }

    #[test]
    fn lm_refines_even_when_h8_is_zero() {
        // h[8] = 0: the projective weight vanishes at the src origin, so the
        // h/h[8] chart is invalid — the old code silently returned the input.
        // Points live away from the vanishing line, so refinement is well
        // posed after re-anchoring the source frame.
        let h_deg: [f64; 9] = [
            1.0, 0.1, 5.0, //
            -0.1, 1.1, 3.0, //
            1e-3, 1e-3, 0.0,
        ];
        let src = vec![
            [100.0, 100.0],
            [400.0, 120.0],
            [420.0, 380.0],
            [110.0, 400.0],
            [250.0, 250.0],
            [300.0, 150.0],
        ];
        let noise = [0.4, -0.3, 0.2, -0.4, 0.3, -0.2];
        let dst: Vec<[f64; 2]> = src
            .iter()
            .enumerate()
            .map(|(i, &p)| {
                let d = apply_true(&h_deg, p);
                [d[0] + noise[i], d[1] - noise[(i + 2) % 6]]
            })
            .collect();
        let h0 = Homography(h_deg);
        let cost = |h: &Homography| -> f64 {
            reprojection_errors(h, &src, &dst).iter().map(|e| e * e).sum()
        };
        let h1 = refine_lm(&h0, &src, &dst);
        assert!(
            cost(&h1) < cost(&h0) - 1e-6,
            "h[8]=0 input must actually be refined, not returned unchanged \
             (cost {} -> {})",
            cost(&h0),
            cost(&h1)
        );
    }

    #[test]
    fn estimate_with_outliers_uses_ransac() {
        let mut src = square_mm();
        src.extend([[350.0, 0.0], [500.0, 0.0], [500.0, 150.0], [350.0, 150.0]]);
        let mut dst: Vec<[f64; 2]> = src.iter().map(|&p| apply_true(&H_TRUE, p)).collect();
        // One gross outlier.
        dst[5][0] += 40.0;
        dst[5][1] -= 35.0;
        let est = estimate(&src, &dst, 3.0).unwrap();
        assert_eq!(est.inliers, 7, "outlier must be excluded");
        assert!(est.residuals[5] > 30.0, "outlier residual stays visible");
        // Inlier residuals essentially zero.
        for (i, r) in est.residuals.iter().enumerate() {
            if i != 5 {
                assert!(*r < 0.01, "inlier {i} residual {r}");
            }
        }
    }

    #[test]
    fn estimate_rejects_degenerate_input() {
        // All four points collinear: the DLT nullspace is multi-dimensional
        // and any returned "solution" would be rank-deficient garbage with
        // deceptively perfect residuals. Must be rejected outright.
        let src = vec![[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [3.0, 0.0]];
        let dst = src.clone();
        assert!(dlt(&src, &dst).is_none(), "collinear 4-point set must be rejected");
        assert!(estimate(&src, &dst, 3.0).is_none(), "estimate must reject it too");

        // Three collinear out of four is equally degenerate.
        let src3 = vec![[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [1.0, 5.0]];
        let dst3 = src3.clone();
        assert!(dlt(&src3, &dst3).is_none(), "3-collinear set must be rejected");

        // Near-collinear (points off the line by ~1e-9) is numerically the
        // same trap.
        let src_near = vec![[0.0, 0.0], [1.0, 1e-9], [2.0, -1e-9], [3.0, 0.0]];
        assert!(dlt(&src_near, &src_near).is_none(), "near-collinear must be rejected");

        assert!(estimate(&src[..3], &dst[..3], 3.0).is_none(), "under-determined");

        // Sanity: a well-conditioned square still passes after the new gate.
        let ok_src = square_mm();
        let ok_dst: Vec<[f64; 2]> = ok_src.iter().map(|&p| apply_true(&H_TRUE, p)).collect();
        assert!(dlt(&ok_src, &ok_dst).is_some(), "well-conditioned set must survive");
    }

    #[test]
    fn residual_summary_is_consistent() {
        let src = square_mm();
        let dst: Vec<[f64; 2]> = src.iter().map(|&p| apply_true(&H_TRUE, p)).collect();
        let est = estimate(&src, &dst, 3.0).unwrap();
        assert_eq!(est.residuals.len(), 4);
        assert!(est.rms < 1e-6, "perfect points: rms {}", est.rms);
        assert!(est.max < 1e-6);
        assert_eq!(est.inliers, 4);
    }
}
