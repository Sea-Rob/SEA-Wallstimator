//! Minimal hand-rolled linear algebra for homography estimation.
//!
//! Just enough (3x3 ops, cyclic Jacobi eigen-decomposition for symmetric
//! matrices up to 9x9, small Gaussian solves) to keep matrix crates out of
//! the WASM bundle (ADR-0001 size budget).

/// Multiply two 3x3 row-major matrices.
pub fn mat3_mul(a: &[f64; 9], b: &[f64; 9]) -> [f64; 9] {
    let mut out = [0.0; 9];
    for r in 0..3 {
        for c in 0..3 {
            out[r * 3 + c] =
                a[r * 3] * b[c] + a[r * 3 + 1] * b[3 + c] + a[r * 3 + 2] * b[6 + c];
        }
    }
    out
}

/// Inverse of a 3x3 row-major matrix via the adjugate, or `None` if singular.
pub fn mat3_inv(m: &[f64; 9]) -> Option<[f64; 9]> {
    let c00 = m[4] * m[8] - m[5] * m[7];
    let c01 = m[5] * m[6] - m[3] * m[8];
    let c02 = m[3] * m[7] - m[4] * m[6];
    let det = m[0] * c00 + m[1] * c01 + m[2] * c02;
    // Relative singularity test: compare against the matrix's own scale so it
    // works for homographies in either px or mm units.
    let scale = m.iter().fold(0.0f64, |acc, v| acc.max(v.abs()));
    if scale == 0.0 || det.abs() < 1e-13 * scale * scale * scale {
        return None;
    }
    let inv_det = 1.0 / det;
    Some([
        c00 * inv_det,
        (m[2] * m[7] - m[1] * m[8]) * inv_det,
        (m[1] * m[5] - m[2] * m[4]) * inv_det,
        c01 * inv_det,
        (m[0] * m[8] - m[2] * m[6]) * inv_det,
        (m[2] * m[3] - m[0] * m[5]) * inv_det,
        c02 * inv_det,
        (m[1] * m[6] - m[0] * m[7]) * inv_det,
        (m[0] * m[4] - m[1] * m[3]) * inv_det,
    ])
}

/// Eigenvector for the smallest eigenvalue of a symmetric `n`x`n` row-major
/// matrix (`n` <= 9), via cyclic Jacobi rotations. `a` is destroyed.
pub fn smallest_eigenvector_sym(a: &mut [f64], n: usize) -> Vec<f64> {
    smallest_eigenpair_sym(a, n).0
}

/// Like [`smallest_eigenvector_sym`] but also returns the smallest and
/// second-smallest eigenvalues. A near-zero gap between them means the
/// nullspace is (numerically) multi-dimensional — for DLT, that is a
/// degenerate correspondence set (e.g. collinear points) whose returned
/// "solution" is an arbitrary vector of the nullspace, not a homography.
pub fn smallest_eigenpair_sym(a: &mut [f64], n: usize) -> (Vec<f64>, f64, f64) {
    debug_assert!(a.len() == n * n && n >= 1 && n <= 9);
    // v starts as identity; columns accumulate the eigenvectors.
    let mut v = vec![0.0; n * n];
    for i in 0..n {
        v[i * n + i] = 1.0;
    }
    for _sweep in 0..60 {
        let mut off = 0.0;
        for p in 0..n {
            for q in p + 1..n {
                off += a[p * n + q].abs();
            }
        }
        if off < 1e-14 {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                let apq = a[p * n + q];
                if apq.abs() < 1e-300 {
                    continue;
                }
                let theta = (a[q * n + q] - a[p * n + p]) / (2.0 * apq);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                // Rotate rows/columns p and q of A.
                for k in 0..n {
                    let akp = a[k * n + p];
                    let akq = a[k * n + q];
                    a[k * n + p] = c * akp - s * akq;
                    a[k * n + q] = s * akp + c * akq;
                }
                for k in 0..n {
                    let apk = a[p * n + k];
                    let aqk = a[q * n + k];
                    a[p * n + k] = c * apk - s * aqk;
                    a[q * n + k] = s * apk + c * aqk;
                }
                // Accumulate the rotation into V.
                for k in 0..n {
                    let vkp = v[k * n + p];
                    let vkq = v[k * n + q];
                    v[k * n + p] = c * vkp - s * vkq;
                    v[k * n + q] = s * vkp + c * vkq;
                }
            }
        }
    }
    let mut min_i = 0;
    for i in 1..n {
        if a[i * n + i] < a[min_i * n + min_i] {
            min_i = i;
        }
    }
    let mut second = f64::INFINITY;
    for i in 0..n {
        if i != min_i {
            second = second.min(a[i * n + i]);
        }
    }
    let vec = (0..n).map(|k| v[k * n + min_i]).collect();
    (vec, a[min_i * n + min_i], second)
}

/// Solve `a * x = b` in place for a small `n`x`n` system via Gaussian
/// elimination with partial pivoting. On success the solution is left in `b`.
pub fn solve_in_place(a: &mut [f64], b: &mut [f64], n: usize) -> bool {
    debug_assert!(a.len() == n * n && b.len() == n);
    for col in 0..n {
        // Pivot.
        let mut piv = col;
        for r in col + 1..n {
            if a[r * n + col].abs() > a[piv * n + col].abs() {
                piv = r;
            }
        }
        if a[piv * n + col].abs() < 1e-12 {
            return false;
        }
        if piv != col {
            for k in 0..n {
                a.swap(col * n + k, piv * n + k);
            }
            b.swap(col, piv);
        }
        let d = a[col * n + col];
        for r in col + 1..n {
            let f = a[r * n + col] / d;
            if f == 0.0 {
                continue;
            }
            for k in col..n {
                a[r * n + k] -= f * a[col * n + k];
            }
            b[r] -= f * b[col];
        }
    }
    for col in (0..n).rev() {
        let mut s = b[col];
        for k in col + 1..n {
            s -= a[col * n + k] * b[k];
        }
        b[col] = s / a[col * n + col];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mat3_inverse_round_trips() {
        let m = [2.0, 0.0, 1.0, -1.0, 3.0, 0.5, 0.0, 1.0, 4.0];
        let inv = mat3_inv(&m).unwrap();
        let id = mat3_mul(&m, &inv);
        for r in 0..3 {
            for c in 0..3 {
                let want = if r == c { 1.0 } else { 0.0 };
                assert!((id[r * 3 + c] - want).abs() < 1e-12, "id[{r}][{c}] = {}", id[r * 3 + c]);
            }
        }
        assert!(mat3_inv(&[0.0; 9]).is_none());
    }

    #[test]
    fn jacobi_finds_smallest_eigenvector() {
        // Symmetric matrix with known eigenstructure: diag(5, 2, 0.1) rotated.
        // Build A = R D R^T with a simple rotation in the (0,2) plane.
        let (c, s) = (0.8, 0.6);
        let r = [c, 0.0, -s, 0.0, 1.0, 0.0, s, 0.0, c];
        let d = [5.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.1];
        let rt = [c, 0.0, s, 0.0, 1.0, 0.0, -s, 0.0, c];
        let a3 = mat3_mul(&mat3_mul(&r, &d), &rt);
        let mut a = a3.to_vec();
        let v = smallest_eigenvector_sym(&mut a, 3);
        // Smallest eigenvalue 0.1 has eigenvector R * e_z = (-s, 0, c).
        let dot = (v[0] * -s + v[2] * c).abs();
        assert!(dot > 0.999, "eigenvector misaligned: {v:?}");
    }

    #[test]
    fn gaussian_solve_recovers_solution() {
        let mut a = vec![0.0, 2.0, 1.0, 1.0, 1.0, 0.0, 3.0, 0.0, 1.0];
        let x_true = [1.0, -2.0, 3.0];
        let mut b = vec![
            0.0 * 1.0 + 2.0 * -2.0 + 1.0 * 3.0,
            1.0 * 1.0 + 1.0 * -2.0,
            3.0 * 1.0 + 1.0 * 3.0,
        ];
        assert!(solve_in_place(&mut a, &mut b, 3));
        for i in 0..3 {
            assert!((b[i] - x_true[i]).abs() < 1e-12);
        }
        // Singular system is rejected.
        let mut sing = vec![1.0, 2.0, 2.0, 4.0];
        let mut rhs = vec![1.0, 2.0];
        assert!(!solve_in_place(&mut sing, &mut rhs, 2));
    }
}
