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
    render_with(scene, width, height, noise_sigma, seed, |ix, iy| {
        h_inv.apply(ix, iy)
    })
}

/// Render the scene through `h_true` AND a real lens model (issue #6): the
/// ideal pinhole projection is bent by division-model radial distortion, the
/// same model the self-calibration estimates. Each output pixel lives in
/// DISTORTED image coordinates; sampling undistorts the supersample position
/// (closed form) before back-projecting to the wall, so projected points and
/// rendered pixels are consistent: a wall point projected through `h_true`
/// then bent by `dist.distort` lands exactly on its rendered image.
pub fn render_scene_distorted(
    scene: &Scene,
    h_true: &[f64; 9],
    width: usize,
    height: usize,
    noise_sigma: f64,
    seed: u64,
    dist: &crate::calib::Distortion,
) -> Vec<u8> {
    let h_inv = Homography(mat3_inv(h_true).expect("h_true must be invertible"));
    render_with(scene, width, height, noise_sigma, seed, |ix, iy| {
        let u = dist.undistort([ix, iy]);
        h_inv.apply(u[0], u[1])
    })
}

/// Shared supersampling render loop over a px -> wall-mm mapping.
fn render_with(
    scene: &Scene,
    width: usize,
    height: usize,
    noise_sigma: f64,
    seed: u64,
    px_to_wall: impl Fn(f64, f64) -> Option<(f64, f64)>,
) -> Vec<u8> {
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
                    acc += match px_to_wall(ix, iy) {
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

/// Camera model for synthetic pan sequences (issue #4): a pinhole camera at
/// distance `distance_mm` from the wall plane, translating from
/// `start_center_mm` to `end_center_mm` (the wall x it points at), with a
/// hand-held wobble in yaw / pitch / height. Returns one wall-mm -> image-px
/// homography per frame.
///
/// Geometry: wall plane z = 0, x right, y down (both mm); camera at
/// z = -distance looking along +z. For plane points, H = K·[r1 r2 | -R·C].
pub struct PanCamera {
    /// Focal length in pixels.
    pub focal_px: f64,
    pub width: usize,
    pub height: usize,
    pub distance_mm: f64,
    pub start_center_mm: [f64; 2],
    pub end_center_mm: [f64; 2],
    /// Peak yaw wobble (radians) over the pan.
    pub yaw_amp: f64,
    /// Peak pitch wobble (radians).
    pub pitch_amp: f64,
}

/// Homography (wall mm -> image px) of a pinhole camera at position `cam`
/// (wall frame, z toward the wall is +; the camera sits at negative z) with
/// yaw/pitch, focal `focal_px`, principal point at the frame centre. Shared
/// by [`PanCamera::homography_at`] and rotate-in-place coaching fixtures
/// (issue #5), which sweep yaw with a fixed position.
pub fn pose_homography(
    focal_px: f64,
    width: usize,
    height: usize,
    cam: [f64; 3],
    yaw: f64,
    pitch: f64,
) -> [f64; 9] {
    // R = Ry(yaw) · Rx(pitch), row-major.
    let (sy, cyw) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let r = [
        cyw,
        sy * sp,
        sy * cp,
        0.0,
        cp,
        -sp,
        -sy,
        cyw * sp,
        cyw * cp,
    ];
    // -R·C
    let tvec = [
        -(r[0] * cam[0] + r[1] * cam[1] + r[2] * cam[2]),
        -(r[3] * cam[0] + r[4] * cam[1] + r[5] * cam[2]),
        -(r[6] * cam[0] + r[7] * cam[1] + r[8] * cam[2]),
    ];
    // M = [r1 r2 t] (columns 0, 1 of R and the translation).
    let m = [
        r[0], r[1], tvec[0], //
        r[3], r[4], tvec[1], //
        r[6], r[7], tvec[2],
    ];
    // K·M with K = [f 0 w/2; 0 f h/2; 0 0 1].
    let (f, u0, v0) = (focal_px, width as f64 / 2.0, height as f64 / 2.0);
    [
        f * m[0] + u0 * m[6],
        f * m[1] + u0 * m[7],
        f * m[2] + u0 * m[8],
        f * m[3] + v0 * m[6],
        f * m[4] + v0 * m[7],
        f * m[5] + v0 * m[8],
        m[6],
        m[7],
        m[8],
    ]
}

impl PanCamera {
    /// Homography (wall mm -> image px) at pan progress `t` in [0, 1].
    pub fn homography_at(&self, t: f64) -> [f64; 9] {
        let cx = self.start_center_mm[0] + t * (self.end_center_mm[0] - self.start_center_mm[0]);
        let cy = self.start_center_mm[1] + t * (self.end_center_mm[1] - self.start_center_mm[1]);
        // Hand-held wobble: smooth pseudo-random oscillations.
        let yaw = self.yaw_amp * (t * 11.0).sin();
        let pitch = self.pitch_amp * (t * 8.5 + 1.2).sin();
        let cam = [cx, cy + 12.0 * (t * 6.3).sin(), -self.distance_mm];
        pose_homography(self.focal_px, self.width, self.height, cam, yaw, pitch)
    }

    /// Homographies for an `n`-frame pan (t linearly spaced over [0, 1]).
    pub fn sequence(&self, n: usize) -> Vec<[f64; 9]> {
        (0..n)
            .map(|i| self.homography_at(i as f64 / (n.max(2) - 1) as f64))
            .collect()
    }
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
