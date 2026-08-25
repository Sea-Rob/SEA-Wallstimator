//! Synthetic ground-truth scene renderer for the CI tests (issue #3).
//!
//! Projects the real Reference Marker pattern (from [`crate::marker`], the
//! same source of truth the PDF and detector use) through a known wall->image
//! homography, with 3x3 supersampling (approximating optical blur) and
//! optional additive noise. Tests run the full detect -> estimate -> rectify
//! path against scenes rendered here and check metric accuracy against the
//! known geometry. Not referenced by any wasm-bindgen export, so it never
//! ships in the browser bundle.

use crate::homography::Homography;
use crate::linalg::mat3_inv;
use crate::marker::{marker_cells, CELLS_PER_SIDE, QUIET_ZONE_CELLS};

/// A marker placed on the synthetic wall.
pub struct SyntheticMarker {
    pub id: u16,
    /// Wall-plane mm of the printed square's top-left corner (unrotated).
    pub x_mm: f64,
    pub y_mm: f64,
    /// Physical side of the printed black square (mm) — 150 * print scale.
    pub side_mm: f64,
    /// Pattern rotation in quarter turns (as [`crate::marker::rotate_word_ccw`]
    /// counts them), applied about the square's center.
    pub rot_quarter: u8,
}

/// Scene description in wall-plane millimetres.
pub struct Scene {
    pub markers: Vec<SyntheticMarker>,
    /// Dark reference dots: (center mm, radius mm). The "taped known
    /// distance" of the demo.
    pub dots: Vec<([f64; 2], f64)>,
}

/// Wall-plane luma at a point, before noise.
fn scene_luma(scene: &Scene, x: f64, y: f64) -> f64 {
    for (center, radius) in &scene.dots {
        let dx = x - center[0];
        let dy = y - center[1];
        if dx * dx + dy * dy <= radius * radius {
            return 25.0;
        }
    }
    for m in &scene.markers {
        let quiet = m.side_mm / CELLS_PER_SIDE as f64 * QUIET_ZONE_CELLS as f64;
        let lx = x - m.x_mm;
        let ly = y - m.y_mm;
        // Paper (quiet zone) around the square.
        if lx >= -quiet && ly >= -quiet && lx < m.side_mm + quiet && ly < m.side_mm + quiet {
            if lx < 0.0 || ly < 0.0 || lx >= m.side_mm || ly >= m.side_mm {
                return 250.0; // quiet zone: white paper
            }
            let cells = marker_cells(m.id).expect("valid marker id");
            let cell = m.side_mm / CELLS_PER_SIDE as f64;
            let mut r = (ly / cell) as usize;
            let mut c = (lx / cell) as usize;
            r = r.min(CELLS_PER_SIDE - 1);
            c = c.min(CELLS_PER_SIDE - 1);
            // The displayed pattern is the canonical one rotated rot_quarter
            // times CCW (per rotate_word_ccw's convention): displayed(r, c) =
            // canonical(c, N-1-r) applied per quarter turn.
            for _ in 0..(m.rot_quarter % 4) {
                let (nr, nc) = (c, CELLS_PER_SIDE - 1 - r);
                r = nr;
                c = nc;
            }
            return if cells[r][c] { 20.0 } else { 250.0 };
        }
    }
    205.0 // painted wall
}

/// Tiny deterministic LCG for reproducible noise.
pub struct Lcg(pub u64);
impl Lcg {
    /// Roughly N(0,1) via the sum of 4 uniforms (Irwin-Hall, sigma-corrected).
    fn next_gauss(&mut self) -> f64 {
        let mut s = 0.0;
        for _ in 0..4 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            s += ((self.0 >> 11) as f64) / ((1u64 << 53) as f64);
        }
        (s - 2.0) * (12.0f64 / 4.0).sqrt()
    }
}

/// Render the scene into a `width` x `height` RGBA frame through `h_true`
/// (wall mm -> image px). 3x3 supersampling per pixel; `noise_sigma` gray
/// levels of additive noise.
pub fn render_scene(
    scene: &Scene,
    h_true: &[f64; 9],
    width: usize,
    height: usize,
    noise_sigma: f64,
    seed: u64,
) -> Vec<u8> {
    let h_inv = Homography(mat3_inv(h_true).expect("h_true must be invertible"));
    let mut rng = Lcg(seed);
    let mut rgba = vec![0u8; width * height * 4];
    for py in 0..height {
        for px in 0..width {
            let mut acc = 0.0;
            // Pixel centers sit at integer coordinates (the detector's and
            // renderer's shared convention), so supersample [i-0.5, i+0.5).
            for sy in 0..3 {
                for sx in 0..3 {
                    let ix = px as f64 - 0.5 + (sx as f64 + 0.5) / 3.0;
                    let iy = py as f64 - 0.5 + (sy as f64 + 0.5) / 3.0;
                    acc += match h_inv.apply(ix, iy) {
                        Some((wx, wy)) => scene_luma(scene, wx, wy),
                        None => 205.0,
                    };
                }
            }
            let mut v = acc / 9.0;
            if noise_sigma > 0.0 {
                v += noise_sigma * rng.next_gauss();
            }
            let g = v.round().clamp(0.0, 255.0) as u8;
            let o = (py * width + px) * 4;
            rgba[o] = g;
            rgba[o + 1] = g;
            rgba[o + 2] = g;
            rgba[o + 3] = 255;
        }
    }
    rgba
}

/// Project a wall-plane point through a raw homography (mm -> px).
pub fn project(h: &[f64; 9], x: f64, y: f64) -> [f64; 2] {
    let w = h[6] * x + h[7] * y + h[8];
    [
        (h[0] * x + h[1] * y + h[2]) / w,
        (h[3] * x + h[4] * y + h[5]) / w,
    ]
}

/// Wall-plane position of a marker's printed top-left corner given its
/// rotation (the pattern rotates about the square's center, so the printed
/// TL lands on a different footprint corner for each quarter turn).
pub fn printed_top_left(m: &SyntheticMarker) -> [f64; 2] {
    // Footprint corners clockwise from the footprint's own top-left.
    let s = m.side_mm;
    let fc = [
        [m.x_mm, m.y_mm],
        [m.x_mm + s, m.y_mm],
        [m.x_mm + s, m.y_mm + s],
        [m.x_mm, m.y_mm + s],
    ];
    // Rotating the pattern k quarter turns CCW (screen sense, y down) moves
    // the printed TL from footprint corner 0 to corner (4 - k) % 4... the
    // synthetic tests pin this down; see tests/still_rectification.rs.
    fc[((4 - (m.rot_quarter as usize % 4)) % 4) % 4]
}
