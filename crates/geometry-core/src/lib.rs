//! Wallstimator geometry core.
//!
//! Live path (walking skeleton): the capture page writes camera frames
//! (RGBA) directly into WASM memory via [`FrameProcessor::input_ptr`], calls
//! [`FrameProcessor::process`], and reads the visibly-processed overlay back
//! out via [`FrameProcessor::output_ptr`]. No per-frame allocation or
//! marshalling copies across the wasm-bindgen boundary itself (the JS side
//! still copies pixels in and out of its canvases).
//!
//! Still path (issue #3): [`FrameProcessor::rectify_captured`] runs the full
//! classical pipeline on the current input frame — Reference Marker
//! detection ([`detect`]), wall-plane homography estimation via DLT + RANSAC
//! + LM ([`homography`]), and metric inverse-warp rendering ([`rectify`]) —
//! and returns a [`RectifiedWallImage`] with a mm/px scale and the
//! reprojection residuals.

use wasm_bindgen::prelude::*;

pub mod detect;
pub mod homography;
pub mod linalg;
pub mod marker;
pub mod rectify;
pub mod synthetic;

/// Nominal printed length of the Reference Marker PDF's ruler strip (mm).
/// The capture page divides the Homeowner's measured length by this to get
/// the session's print-scale correction factor (ADR-0002).
///
/// Consumption rule for all metric math in this crate (issue #3 onward):
/// the marker's true physical side is `marker_side_mm() * correction_factor`
/// — MULTIPLY nominal printed dimensions by the factor, never divide. A sheet
/// printed at 94% has factor 0.94 and a 141 mm marker.
#[wasm_bindgen]
pub fn ruler_nominal_mm() -> f64 {
    marker::RULER_LENGTH_MM
}

/// Nominal printed side length of a Reference Marker's black square (mm).
#[wasm_bindgen]
pub fn marker_side_mm() -> f64 {
    marker::MARKER_SIDE_MM
}

/// BT.601 integer luma approximation of one RGBA pixel.
#[inline]
pub fn rgba_to_luma(r: u8, g: u8, b: u8) -> u8 {
    ((77 * r as u32 + 150 * g as u32 + 29 * b as u32) >> 8) as u8
}

/// Convert a tightly-packed RGBA frame to a luma plane.
///
/// `gray` must hold `width * height` bytes; `rgba` four times that.
pub fn grayscale(rgba: &[u8], gray: &mut [u8]) {
    debug_assert_eq!(rgba.len(), gray.len() * 4);
    for (px, g) in rgba.chunks_exact(4).zip(gray.iter_mut()) {
        *g = rgba_to_luma(px[0], px[1], px[2]);
    }
}

/// Sobel gradient magnitude (clamped to 0..=255) at interior pixels.
///
/// Border pixels are written as 0. `gray` and `edges` are `width * height`
/// luma planes.
pub fn sobel_edges(gray: &[u8], edges: &mut [u8], width: usize, height: usize) {
    debug_assert_eq!(gray.len(), width * height);
    debug_assert_eq!(edges.len(), width * height);
    edges.fill(0);
    if width < 3 || height < 3 {
        return;
    }
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let at = |dx: isize, dy: isize| -> i32 {
                let ix = (x as isize + dx) as usize;
                let iy = (y as isize + dy) as usize;
                gray[iy * width + ix] as i32
            };
            let gx = -at(-1, -1) - 2 * at(-1, 0) - at(-1, 1)
                + at(1, -1) + 2 * at(1, 0) + at(1, 1);
            let gy = -at(-1, -1) - 2 * at(0, -1) - at(1, -1)
                + at(-1, 1) + 2 * at(0, 1) + at(1, 1);
            let mag = (gx.abs() + gy.abs()).min(255);
            edges[y * width + x] = mag as u8;
        }
    }
}

/// Compose the overlay frame: grayscale base with strong edges tinted green,
/// so the Homeowner-facing capture page shows unmistakably processed output.
pub fn compose_overlay(gray: &[u8], edges: &[u8], rgba_out: &mut [u8]) {
    debug_assert_eq!(rgba_out.len(), gray.len() * 4);
    const EDGE_THRESHOLD: u8 = 96;
    for ((&g, &e), px) in gray
        .iter()
        .zip(edges.iter())
        .zip(rgba_out.chunks_exact_mut(4))
    {
        if e >= EDGE_THRESHOLD {
            px[0] = 0;
            px[1] = 255;
            px[2] = 96;
        } else {
            px[0] = g;
            px[1] = g;
            px[2] = g;
        }
        px[3] = 255;
    }
}

/// Pixel count for a `width` x `height` frame, or `None` if either dimension
/// is zero or the RGBA byte length (`w * h * 4`) would overflow `usize`
/// (which is 32-bit on wasm32).
fn checked_frame_pixels(width: u32, height: u32) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)
        .filter(|&n| n > 0 && n.checked_mul(4).is_some())
}

/// Fixed-size frame processor owning all buffers on the WASM side.
#[wasm_bindgen]
pub struct FrameProcessor {
    width: usize,
    height: usize,
    input_rgba: Vec<u8>,
    gray: Vec<u8>,
    edges: Vec<u8>,
    output_rgba: Vec<u8>,
}

#[wasm_bindgen]
impl FrameProcessor {
    /// Allocate a processor for `width` x `height` RGBA frames.
    ///
    /// Rejects zero or overflowing dimensions: a panic here would trap and
    /// kill the WASM instance, so invalid sizes surface as a JS error instead.
    #[wasm_bindgen(constructor)]
    pub fn new(width: u32, height: u32) -> Result<FrameProcessor, JsError> {
        let n = checked_frame_pixels(width, height)
            .ok_or_else(|| JsError::new("invalid frame dimensions"))?;
        Ok(FrameProcessor {
            width: width as usize,
            height: height as usize,
            input_rgba: vec![0; n * 4],
            gray: vec![0; n],
            edges: vec![0; n],
            output_rgba: vec![0; n * 4],
        })
    }

    /// Pointer into WASM memory where the capture page writes the next
    /// RGBA frame (`width * height * 4` bytes).
    pub fn input_ptr(&mut self) -> *mut u8 {
        self.input_rgba.as_mut_ptr()
    }

    /// Pointer to the processed RGBA overlay (`width * height * 4` bytes).
    pub fn output_ptr(&self) -> *const u8 {
        self.output_rgba.as_ptr()
    }

    /// Byte length of one RGBA frame at this processor's dimensions.
    pub fn frame_len(&self) -> usize {
        self.input_rgba.len()
    }

    /// Run the skeleton pipeline on the frame currently in the input buffer:
    /// grayscale -> Sobel edges -> composed overlay.
    pub fn process(&mut self) {
        grayscale(&self.input_rgba, &mut self.gray);
        sobel_edges(&self.gray, &mut self.edges, self.width, self.height);
        compose_overlay(&self.gray, &self.edges, &mut self.output_rgba);
    }

    /// Still-frame rectification (issue #3): detect Reference Marker(s) in
    /// the frame currently in the input buffer, estimate the wall-plane
    /// homography (DLT + RANSAC + LM), and render the Rectified Wall Image.
    ///
    /// `correction_factor` is the session's print-scale factor (ADR-0002):
    /// the marker's true physical side is `marker_side_mm() *
    /// correction_factor` (MULTIPLY, never divide). Returns `null` when no
    /// Reference Marker is detected.
    pub fn rectify_captured(&self, correction_factor: f64) -> Option<RectifiedWallImage> {
        let r = rectify::rectify_frame(&self.input_rgba, self.width, self.height, correction_factor)?;
        Some(RectifiedWallImage::from_core(r))
    }
}

/// A Rectified Wall Image: the captured frame re-projected to
/// fronto-parallel metric coordinates. Pixels are exposed zero-copy via
/// [`RectifiedWallImage::pixels_ptr`]; the metric mapping is
/// `wall_mm = origin_mm + pixel * mm_per_px` on both axes.
#[wasm_bindgen]
pub struct RectifiedWallImage {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    mm_per_px: f64,
    origin_x_mm: f64,
    origin_y_mm: f64,
    marker_ids: Vec<u16>,
    second_marker_rejected: bool,
    residual_rms_px: f64,
    residual_max_px: f64,
    points_used: u32,
    inliers: u32,
}

impl RectifiedWallImage {
    fn from_core(r: rectify::Rectified) -> Self {
        RectifiedWallImage {
            rgba: r.rgba,
            width: r.width as u32,
            height: r.height as u32,
            mm_per_px: r.mm_per_px,
            origin_x_mm: r.origin_mm[0],
            origin_y_mm: r.origin_mm[1],
            marker_ids: r.marker_ids,
            second_marker_rejected: r.second_marker_rejected,
            residual_rms_px: r.estimate.rms,
            residual_max_px: r.estimate.max,
            points_used: r.estimate.residuals.len() as u32,
            inliers: r.estimate.inliers as u32,
        }
    }
}

#[wasm_bindgen]
impl RectifiedWallImage {
    /// Pointer to the tightly packed RGBA pixels in WASM memory.
    pub fn pixels_ptr(&self) -> *const u8 {
        self.rgba.as_ptr()
    }

    pub fn pixels_len(&self) -> usize {
        self.rgba.len()
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Millimetres of wall per rectified pixel (isotropic, print-scale
    /// correction already applied).
    pub fn mm_per_px(&self) -> f64 {
        self.mm_per_px
    }

    /// Wall-plane mm coordinate of the image's top-left corner (the anchor
    /// marker's printed top-left corner is the plane origin).
    pub fn origin_x_mm(&self) -> f64 {
        self.origin_x_mm
    }

    pub fn origin_y_mm(&self) -> f64 {
        self.origin_y_mm
    }

    /// IDs of the Reference Markers used for the estimate, anchor first.
    pub fn marker_ids(&self) -> Vec<u16> {
        self.marker_ids.clone()
    }

    /// True when a second marker was detected but its constraint was
    /// discarded (bad detection or inconsistent joint fit): the result
    /// degraded to single-marker extrapolation and the page must say so.
    pub fn second_marker_rejected(&self) -> bool {
        self.second_marker_rejected
    }

    /// RMS reprojection error of the marker corners under the estimated
    /// homography, in source-frame pixels. The Error Bound story (see
    /// CONTEXT.md) starts from this number.
    pub fn residual_rms_px(&self) -> f64 {
        self.residual_rms_px
    }

    /// Worst single-corner reprojection error, source-frame pixels.
    pub fn residual_max_px(&self) -> f64 {
        self.residual_max_px
    }

    /// Marker-corner correspondences fed to the estimator (4 or 8).
    pub fn points_used(&self) -> u32 {
        self.points_used
    }

    /// Correspondences the final model kept as inliers.
    pub fn inliers(&self) -> u32 {
        self.inliers
    }
}

/// Full printed cell grid of a Reference Marker, row-major, 36 bytes,
/// 1 = black cell. Debug/test surface: lets the page render a ground-truth
/// marker (e.g. the browser smoke test's synthetic camera) from the same
/// dictionary the detector matches against.
#[wasm_bindgen]
pub fn marker_pattern(id: u16) -> Option<Vec<u8>> {
    marker::marker_cells(id).map(|cells| {
        cells
            .iter()
            .flat_map(|row| row.iter().map(|&black| black as u8))
            .collect()
    })
}

/// Core version string, handy for the capture page to prove the module loaded.
#[wasm_bindgen]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luma_of_white_is_near_255_and_black_is_0() {
        assert_eq!(rgba_to_luma(0, 0, 0), 0);
        assert!(rgba_to_luma(255, 255, 255) >= 254);
    }

    #[test]
    fn grayscale_converts_known_pixels() {
        // One red, one green pixel.
        let rgba = [255, 0, 0, 255, 0, 255, 0, 255];
        let mut gray = [0u8; 2];
        grayscale(&rgba, &mut gray);
        assert_eq!(gray[0], (77 * 255 >> 8) as u8);
        assert_eq!(gray[1], (150 * 255 >> 8) as u8);
    }

    #[test]
    fn sobel_fires_on_a_vertical_step_edge_only() {
        // 8x5 luma plane: left half black, right half white.
        let (w, h) = (8usize, 5usize);
        let mut gray = vec![0u8; w * h];
        for y in 0..h {
            for x in 4..w {
                gray[y * w + x] = 255;
            }
        }
        let mut edges = vec![0u8; w * h];
        sobel_edges(&gray, &mut edges, w, h);
        let mid = 2 * w; // an interior row
        assert_eq!(edges[mid + 1], 0, "flat black region must be edge-free");
        assert_eq!(edges[mid + 6], 0, "flat white region must be edge-free");
        assert_eq!(edges[mid + 3], 255, "step edge must saturate");
        assert_eq!(edges[mid + 4], 255, "step edge must saturate");
        // Borders are always zero.
        assert!(edges[..w].iter().all(|&e| e == 0));
    }

    #[test]
    fn sobel_handles_degenerate_sizes() {
        let gray = [10u8, 20, 30, 40];
        let mut edges = [1u8; 4];
        sobel_edges(&gray, &mut edges, 2, 2);
        assert_eq!(edges, [0; 4]);
    }

    #[test]
    fn frame_dimensions_are_validated() {
        assert_eq!(checked_frame_pixels(8, 5), Some(40));
        assert_eq!(checked_frame_pixels(0, 5), None);
        assert_eq!(checked_frame_pixels(8, 0), None);
        // w * h * 4 must not overflow usize (32-bit on wasm32).
        assert_eq!(checked_frame_pixels(u32::MAX, u32::MAX), None);
    }

    #[test]
    fn frame_processor_round_trips_a_frame() {
        let mut fp = FrameProcessor::new(8, 5).expect("valid dimensions");
        assert_eq!(fp.frame_len(), 8 * 5 * 4);
        // Fill input: left half black, right half white.
        for y in 0..5 {
            for x in 0..8 {
                let v = if x >= 4 { 255 } else { 0 };
                let i = (y * 8 + x) * 4;
                fp.input_rgba[i..i + 3].fill(v);
                fp.input_rgba[i + 3] = 255;
            }
        }
        fp.process();
        // Interior step-edge pixel is tinted green; flat pixel stays gray.
        let edge_px = &fp.output_rgba[(2 * 8 + 3) * 4..(2 * 8 + 3) * 4 + 4];
        assert_eq!(edge_px, &[0, 255, 96, 255]);
        let flat_px = &fp.output_rgba[(2 * 8 + 1) * 4..(2 * 8 + 1) * 4 + 4];
        assert_eq!(flat_px, &[0, 0, 0, 255]);
    }
}
