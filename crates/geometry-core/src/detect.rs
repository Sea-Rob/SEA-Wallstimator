//! Reference Marker detection: classical CV, pure Rust, no OpenCV (ADR-0001).
//!
//! Pipeline: grayscale luma plane -> adaptive threshold (integral-image mean)
//! -> connected dark components -> outer contour trace -> quad fit
//! (Douglas-Peucker) -> perspective-normalized 6x6 grid sampling -> decode
//! against the DICT_4X4_50 words in [`crate::marker`] (all 4 rotations) ->
//! sub-pixel corner refinement (edge-profile line fits intersected at the
//! corners). Non-dictionary quads are rejected.

use crate::homography::{dlt, Homography};
use crate::marker::{marker_word, rotate_word_ccw, DATA_CELLS, LEFT_MARKER_ID, RIGHT_MARKER_ID};

/// Cells across the printed black square: 4 data + border on each side.
const GRID: usize = DATA_CELLS + 2;

/// A decoded Reference Marker. `corners` are image px in the marker's own
/// canonical orientation — index 0 is the printed top-left corner, then
/// clockwise (top-right, bottom-right, bottom-left) as printed.
#[derive(Clone, Debug)]
pub struct DetectedMarker {
    pub id: u16,
    pub corners: [[f64; 2]; 4],
}

/// Detect all Reference Markers (IDs 0/1) in a luma plane. At most one of
/// each ID is returned — duplicates keep the largest quad.
pub fn detect_markers(gray: &[u8], width: usize, height: usize) -> Vec<DetectedMarker> {
    debug_assert_eq!(gray.len(), width * height);
    if width < 32 || height < 32 {
        return Vec::new();
    }
    let mask = adaptive_threshold(gray, width, height);
    let quads = find_quads(&mask, width, height);

    let mut found: Vec<(DetectedMarker, f64)> = Vec::new();
    for quad in quads {
        let Some((id, ordered)) = decode_quad(gray, width, height, &quad) else {
            continue;
        };
        let refined = refine_corners(gray, width, height, &ordered);
        let area = quad_area(&refined);
        let marker = DetectedMarker { id, corners: refined };
        match found.iter_mut().find(|(m, _)| m.id == id) {
            Some(slot) if slot.1 < area => *slot = (marker, area),
            Some(_) => {}
            None => found.push((marker, area)),
        }
    }
    found.into_iter().map(|(m, _)| m).collect()
}

// ---------------------------------------------------------------------------
// Thresholding.

/// Adaptive mean threshold over a window scaled to the frame; a pixel is
/// "dark" (1) when below the local mean minus a small offset. Robust to the
/// uneven lighting of a hand-held phone shot of a wall.
fn adaptive_threshold(gray: &[u8], width: usize, height: usize) -> Vec<u8> {
    // Summed-area table, (width+1) x (height+1).
    let iw = width + 1;
    let mut integral = vec![0u64; iw * (height + 1)];
    for y in 0..height {
        let mut row_sum = 0u64;
        for x in 0..width {
            row_sum += gray[y * width + x] as u64;
            integral[(y + 1) * iw + x + 1] = integral[y * iw + x + 1] + row_sum;
        }
    }
    let radius = (width.min(height) / 12).max(7);
    const OFFSET: i32 = 7;
    let mut mask = vec![0u8; width * height];
    for y in 0..height {
        let y0 = y.saturating_sub(radius);
        let y1 = (y + radius + 1).min(height);
        for x in 0..width {
            let x0 = x.saturating_sub(radius);
            let x1 = (x + radius + 1).min(width);
            let sum = integral[y1 * iw + x1] + integral[y0 * iw + x0]
                - integral[y0 * iw + x1]
                - integral[y1 * iw + x0];
            let count = ((y1 - y0) * (x1 - x0)) as u64;
            let mean = (sum / count) as i32;
            if (gray[y * width + x] as i32) < mean - OFFSET {
                mask[y * width + x] = 1;
            }
        }
    }
    mask
}

// ---------------------------------------------------------------------------
// Quad candidates: connected dark components -> outer contour -> polygon.

fn quad_area(c: &[[f64; 2]; 4]) -> f64 {
    let mut a = 0.0;
    for i in 0..4 {
        let j = (i + 1) % 4;
        a += c[i][0] * c[j][1] - c[j][0] * c[i][1];
    }
    a.abs() * 0.5
}

/// Minimum quad side in px: the 6x6 grid needs enough resolution to sample.
const MIN_SIDE_PX: f64 = 14.0;

fn find_quads(mask: &[u8], width: usize, height: usize) -> Vec<[[f64; 2]; 4]> {
    let mut labels = vec![0u32; width * height];
    let mut quads = Vec::new();
    let mut next_label = 1u32;
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let min_area = ((MIN_SIDE_PX * MIN_SIDE_PX) * 0.5) as usize;
    let max_area = width * height * 9 / 10;

    for sy in 0..height {
        for sx in 0..width {
            let si = sy * width + sx;
            if mask[si] == 0 || labels[si] != 0 {
                continue;
            }
            // Flood-fill this dark component (4-connectivity, matching the
            // contour tracer below).
            let label = next_label;
            next_label += 1;
            labels[si] = label;
            stack.push((sx, sy));
            let mut count = 0usize;
            // Track the component's top-most, then left-most pixel: a
            // guaranteed outer-boundary start for the contour trace.
            let (mut cx, mut cy) = (sx, sy);
            while let Some((x, y)) = stack.pop() {
                count += 1;
                if y < cy || (y == cy && x < cx) {
                    cx = x;
                    cy = y;
                }
                let mut push = |nx: usize, ny: usize| {
                    let ni = ny * width + nx;
                    if mask[ni] == 1 && labels[ni] == 0 {
                        labels[ni] = label;
                        stack.push((nx, ny));
                    }
                };
                if x > 0 {
                    push(x - 1, y);
                }
                if x + 1 < width {
                    push(x + 1, y);
                }
                if y > 0 {
                    push(x, y - 1);
                }
                if y + 1 < height {
                    push(x, y + 1);
                }
            }
            if count < min_area || count > max_area {
                continue;
            }
            let contour = trace_contour(&labels, width, height, label, cx, cy);
            if contour.len() < 16 {
                continue;
            }
            if let Some(quad) = fit_quad(&contour) {
                quads.push(quad);
            }
        }
    }
    quads
}

/// Moore-neighbor outer contour trace (8-connected walk around a
/// 4-connected component), starting from its top-left-most pixel.
fn trace_contour(
    labels: &[u32],
    width: usize,
    height: usize,
    label: u32,
    sx: usize,
    sy: usize,
) -> Vec<[i32; 2]> {
    let inside = |x: i32, y: i32| -> bool {
        x >= 0
            && y >= 0
            && (x as usize) < width
            && (y as usize) < height
            && labels[y as usize * width + x as usize] == label
    };
    // 8 directions, clockwise starting from west (screen coords, y down).
    const DIRS: [[i32; 2]; 8] = [
        [-1, 0],
        [-1, -1],
        [0, -1],
        [1, -1],
        [1, 0],
        [1, 1],
        [0, 1],
        [-1, 1],
    ];
    let start = [sx as i32, sy as i32];
    let mut contour = vec![start];
    let mut cur = start;
    // The pixel above the start is guaranteed outside (top-most row of the
    // component): begin the neighborhood scan from it.
    let mut backtrack_dir = 2usize; // pointing north
    let cap = 4 * (width + height) * 4; // generous safety bound
    loop {
        let mut found = false;
        // Scan clockwise from the backtrack direction.
        for k in 1..=8 {
            let dir = (backtrack_dir + k) % 8;
            let nx = cur[0] + DIRS[dir][0];
            let ny = cur[1] + DIRS[dir][1];
            if inside(nx, ny) {
                // New backtrack: the direction we came from (opposite),
                // rotated so the scan resumes just past the previous outside
                // neighbor.
                backtrack_dir = (dir + 4) % 8;
                cur = [nx, ny];
                found = true;
                break;
            }
        }
        if !found {
            break; // isolated pixel
        }
        if cur == start && contour.len() > 2 {
            break;
        }
        contour.push(cur);
        if contour.len() > cap {
            break; // pathological; bail out
        }
    }
    contour
}

/// Fit a convex quadrilateral to a closed contour via Douglas-Peucker
/// (ArUco-style: epsilon proportional to the perimeter). `None` unless the
/// polygon simplifies to exactly 4 convex, well-separated vertices.
fn fit_quad(contour: &[[i32; 2]]) -> Option<[[f64; 2]; 4]> {
    let n = contour.len();
    // Split the closed contour at its two mutually farthest points (approx:
    // farthest from point 0, then farthest from that).
    let d2 = |a: [i32; 2], b: [i32; 2]| -> i64 {
        let dx = (a[0] - b[0]) as i64;
        let dy = (a[1] - b[1]) as i64;
        dx * dx + dy * dy
    };
    let i1 = (0..n).max_by_key(|&i| d2(contour[0], contour[i]))?;
    let i2 = (0..n).max_by_key(|&i| d2(contour[i1], contour[i]))?;
    let (a, b) = (i1.min(i2), i1.max(i2));

    let perimeter: f64 = (0..n)
        .map(|i| {
            let p = contour[i];
            let q = contour[(i + 1) % n];
            (d2(p, q) as f64).sqrt()
        })
        .sum();
    let eps = 0.03 * perimeter;

    let mut poly: Vec<[i32; 2]> = Vec::new();
    let chain1: Vec<[i32; 2]> = contour[a..=b].to_vec();
    let mut chain2: Vec<[i32; 2]> = contour[b..].to_vec();
    chain2.extend_from_slice(&contour[..=a]);
    douglas_peucker(&chain1, eps, &mut poly);
    poly.pop(); // chain end == chain2 start
    douglas_peucker(&chain2, eps, &mut poly);
    poly.pop(); // chain2 end == chain1 start

    if poly.len() != 4 {
        return None;
    }
    let mut quad = [[0.0f64; 2]; 4];
    for (i, p) in poly.iter().enumerate() {
        quad[i] = [p[0] as f64, p[1] as f64];
    }

    // Convexity + consistent winding: all cross products share a sign.
    let mut sign = 0.0f64;
    for i in 0..4 {
        let p0 = quad[i];
        let p1 = quad[(i + 1) % 4];
        let p2 = quad[(i + 2) % 4];
        let cross = (p1[0] - p0[0]) * (p2[1] - p1[1]) - (p1[1] - p0[1]) * (p2[0] - p1[0]);
        if cross.abs() < 1e-9 {
            return None;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return None;
        }
    }
    // Enforce clockwise order in screen coords (y down): positive cross.
    if sign < 0.0 {
        quad.swap(1, 3);
    }
    // Reject slivers and under-resolved quads.
    for i in 0..4 {
        let p = quad[i];
        let q = quad[(i + 1) % 4];
        let side = ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt();
        if side < MIN_SIDE_PX {
            return None;
        }
    }
    Some(quad)
}

/// Douglas-Peucker on an open chain; appends the simplified points including
/// the chain's first and last point.
fn douglas_peucker(chain: &[[i32; 2]], eps: f64, out: &mut Vec<[i32; 2]>) {
    if chain.is_empty() {
        return;
    }
    if chain.len() <= 2 {
        out.extend_from_slice(chain);
        return;
    }
    let (first, last) = (chain[0], chain[chain.len() - 1]);
    let (fx, fy) = (first[0] as f64, first[1] as f64);
    let (dx, dy) = (last[0] as f64 - fx, last[1] as f64 - fy);
    let len = (dx * dx + dy * dy).sqrt();
    let mut max_d = -1.0;
    let mut max_i = 0;
    for (i, p) in chain.iter().enumerate().skip(1).take(chain.len() - 2) {
        let (px, py) = (p[0] as f64 - fx, p[1] as f64 - fy);
        let d = if len < 1e-9 {
            (px * px + py * py).sqrt()
        } else {
            (px * dy - py * dx).abs() / len
        };
        if d > max_d {
            max_d = d;
            max_i = i;
        }
    }
    if max_d > eps {
        douglas_peucker(&chain[..=max_i], eps, out);
        out.pop(); // shared vertex
        douglas_peucker(&chain[max_i..], eps, out);
    } else {
        out.push(first);
        out.push(last);
    }
}

// ---------------------------------------------------------------------------
// Decoding.

/// Bilinear grayscale sample; clamps to the frame.
pub(crate) fn sample_bilinear(gray: &[u8], width: usize, height: usize, x: f64, y: f64) -> f64 {
    let x = x.clamp(0.0, (width - 1) as f64);
    let y = y.clamp(0.0, (height - 1) as f64);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let fx = x - x0 as f64;
    let fy = y - y0 as f64;
    let g = |xx: usize, yy: usize| gray[yy * width + xx] as f64;
    g(x0, y0) * (1.0 - fx) * (1.0 - fy)
        + g(x1, y0) * fx * (1.0 - fy)
        + g(x0, y1) * (1.0 - fx) * fy
        + g(x1, y1) * fx * fy
}

/// Perspective-normalize the quad, sample the 6x6 cell grid, and decode the
/// payload against the dictionary in all four rotations. Returns the marker
/// ID and the corners reordered to canonical print orientation.
fn decode_quad(
    gray: &[u8],
    width: usize,
    height: usize,
    quad: &[[f64; 2]; 4],
) -> Option<(u16, [[f64; 2]; 4])> {
    // Homography from the unit square (canonical marker) to the image quad.
    let unit = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let h = dlt(&unit, quad)?;

    // Mean gray per cell, sampled on a 3x3 sub-grid inside each cell.
    let mut cells = [[0.0f64; GRID]; GRID];
    for r in 0..GRID {
        for c in 0..GRID {
            let mut acc = 0.0;
            for sr in 0..3 {
                for sc in 0..3 {
                    let u = (c as f64 + 0.3 + 0.2 * sc as f64) / GRID as f64;
                    let v = (r as f64 + 0.3 + 0.2 * sr as f64) / GRID as f64;
                    let (x, y) = h.apply(u, v)?;
                    acc += sample_bilinear(gray, width, height, x, y);
                }
            }
            cells[r][c] = acc / 9.0;
        }
    }

    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for row in &cells {
        for &v in row {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    if hi - lo < 30.0 {
        return None; // not enough contrast to be a printed marker
    }
    let threshold = (lo + hi) * 0.5;
    let is_white = |r: usize, c: usize| cells[r][c] > threshold;

    // Border ring must be entirely black.
    for i in 0..GRID {
        if is_white(0, i) || is_white(GRID - 1, i) || is_white(i, 0) || is_white(i, GRID - 1) {
            return None;
        }
    }

    // Payload word, sampled orientation (row 0 = quad's top edge).
    let mut word = 0u16;
    for r in 0..DATA_CELLS {
        for c in 0..DATA_CELLS {
            word = (word << 1) | is_white(r + 1, c + 1) as u16;
        }
    }

    // Match against both Reference Marker IDs in all four rotations.
    // If rotate_ccw^j(decoded) == canonical, the printed top-left corner sits
    // at quad corner j and canonical corner i is quad corner (j + i) % 4
    // (verified against synthetic rotated renders in the crate tests).
    for id in [LEFT_MARKER_ID, RIGHT_MARKER_ID] {
        let canonical = marker_word(id)?;
        let mut w = word;
        for j in 0..4 {
            if w == canonical {
                let mut ordered = [[0.0f64; 2]; 4];
                for i in 0..4 {
                    ordered[i] = quad[(j + i) % 4];
                }
                return Some((id, ordered));
            }
            w = rotate_word_ccw(w);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Sub-pixel corner refinement.

/// Refine the quad corners: for each edge, locate the black/white transition
/// at sub-pixel accuracy along several profiles perpendicular to the edge,
/// least-squares fit a line through those edge points, and intersect
/// adjacent lines. Falls back to the input corner when refinement diverges.
fn refine_corners(
    gray: &[u8],
    width: usize,
    height: usize,
    quad: &[[f64; 2]; 4],
) -> [[f64; 2]; 4] {
    let mut lines: [Option<[f64; 3]>; 4] = [None; 4];
    for e in 0..4 {
        lines[e] = fit_edge_line(gray, width, height, quad[e], quad[(e + 1) % 4]);
    }
    let mut out = *quad;
    for i in 0..4 {
        // Corner i is the intersection of edge (i-1) and edge i.
        let prev = lines[(i + 3) % 4];
        let cur = lines[i];
        if let (Some(l1), Some(l2)) = (prev, cur) {
            if let Some(p) = intersect_lines(l1, l2) {
                let dx = p[0] - quad[i][0];
                let dy = p[1] - quad[i][1];
                if (dx * dx + dy * dy).sqrt() <= 2.5 {
                    out[i] = p;
                }
            }
        }
    }
    out
}

/// Fit the sub-pixel line of the dark-to-light edge between corners `a` and
/// `b`. Returns homogeneous line coefficients [nx, ny, d] (nx*x + ny*y + d = 0).
fn fit_edge_line(
    gray: &[u8],
    width: usize,
    height: usize,
    a: [f64; 2],
    b: [f64; 2],
) -> Option<[f64; 3]> {
    let ex = b[0] - a[0];
    let ey = b[1] - a[1];
    let len = (ex * ex + ey * ey).sqrt();
    if len < MIN_SIDE_PX {
        return None;
    }
    // Unit normal to the edge.
    let nx = -ey / len;
    let ny = ex / len;

    const PROFILES: usize = 12;
    const HALF_SPAN: f64 = 2.5; // px each side of the nominal edge
    const STEP: f64 = 0.5;
    let samples_per_profile = (2.0 * HALF_SPAN / STEP) as usize + 1;

    let mut pts: Vec<[f64; 2]> = Vec::with_capacity(PROFILES);
    for k in 0..PROFILES {
        // Middle 60% of the edge: corners themselves are rounded by blur.
        let t = 0.2 + 0.6 * (k as f64 + 0.5) / PROFILES as f64;
        let px = a[0] + t * ex;
        let py = a[1] + t * ey;
        // Sample the intensity profile along the normal.
        let mut profile = [0.0f64; 16];
        debug_assert!(samples_per_profile <= profile.len());
        let mut oob = false;
        for s in 0..samples_per_profile {
            let d = -HALF_SPAN + s as f64 * STEP;
            let x = px + d * nx;
            let y = py + d * ny;
            if x < 0.0 || y < 0.0 || x > (width - 1) as f64 || y > (height - 1) as f64 {
                oob = true;
                break;
            }
            profile[s] = sample_bilinear(gray, width, height, x, y);
        }
        if oob {
            continue;
        }
        // Gradient-magnitude-weighted centroid of the transition.
        let mut wsum = 0.0;
        let mut dsum = 0.0;
        for s in 0..samples_per_profile - 1 {
            let g = profile[s + 1] - profile[s];
            let w = g * g;
            let d_mid = -HALF_SPAN + (s as f64 + 0.5) * STEP;
            wsum += w;
            dsum += w * d_mid;
        }
        if wsum < 1e-6 {
            continue;
        }
        let d_star = dsum / wsum;
        pts.push([px + d_star * nx, py + d_star * ny]);
    }
    if pts.len() < 6 {
        return None;
    }

    // Total least squares line through pts (PCA on the 2x2 scatter).
    let n = pts.len() as f64;
    let (mut mx, mut my) = (0.0, 0.0);
    for p in &pts {
        mx += p[0];
        my += p[1];
    }
    mx /= n;
    my /= n;
    let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
    for p in &pts {
        let dx = p[0] - mx;
        let dy = p[1] - my;
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    // Normal of the best-fit line = eigenvector of the smaller eigenvalue.
    let tr = sxx + syy;
    let det = sxx * syy - sxy * sxy;
    let disc = (tr * tr / 4.0 - det).max(0.0).sqrt();
    let lam_small = tr / 2.0 - disc;
    let (mut lnx, mut lny) = if sxy.abs() > 1e-12 {
        (lam_small - syy, sxy)
    } else if sxx <= syy {
        (1.0, 0.0)
    } else {
        (0.0, 1.0)
    };
    let norm = (lnx * lnx + lny * lny).sqrt();
    if norm < 1e-12 {
        return None;
    }
    lnx /= norm;
    lny /= norm;
    Some([lnx, lny, -(lnx * mx + lny * my)])
}

fn intersect_lines(l1: [f64; 3], l2: [f64; 3]) -> Option<[f64; 2]> {
    let det = l1[0] * l2[1] - l2[0] * l1[1];
    if det.abs() < 1e-9 {
        return None;
    }
    Some([
        (l1[1] * l2[2] - l2[1] * l1[2]) / det,
        (l2[0] * l1[2] - l1[0] * l2[2]) / det,
    ])
}

/// Homography from canonical marker corners (unit square, canonical print
/// orientation) to the detected image corners.
pub fn marker_unit_homography(m: &DetectedMarker) -> Option<Homography> {
    let unit = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    dlt(&unit, &m.corners)
}
