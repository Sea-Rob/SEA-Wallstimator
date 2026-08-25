//! Synthetic ground-truth tests for the still-frame rectification path
//! (issue #3): render the real Reference Marker pattern through a known
//! homography with blur (supersampling) and noise, run the full detect ->
//! estimate -> rectify pipeline, and verify a known reference length
//! measures within 1% near the marker — plus the degenerate cases.

use geometry_core::detect::detect_markers;
use geometry_core::grayscale;
use geometry_core::rectify::rectify_frame;
use geometry_core::synthetic::{
    printed_top_left, project, render_scene, Scene, SyntheticMarker,
};

const W: usize = 640;
const H: usize = 480;

/// A plausible phone-shot homography: wall mm -> image px, mild perspective
/// tilt, marker (150 mm at the origin) roughly 190 px across in frame.
const H_TRUE: [f64; 9] = [
    1.25, 0.06, 120.0, //
    -0.05, 1.22, 70.0, //
    2.2e-4, 1.4e-4, 1.0,
];

fn marker_at_origin(rot_quarter: u8, side_mm: f64) -> SyntheticMarker {
    SyntheticMarker { id: 0, x_mm: 0.0, y_mm: 0.0, side_mm, rot_quarter }
}

/// Centroid of dark pixels within `radius_px` of an expected position in a
/// rectified RGBA image — how the tests "tap" a reference dot the way the
/// Homeowner taps the measure tool.
fn dark_centroid(rgba: &[u8], w: usize, h: usize, near: [f64; 2], radius_px: f64) -> [f64; 2] {
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0u32);
    for y in 0..h {
        for x in 0..w {
            let dx = x as f64 - near[0];
            let dy = y as f64 - near[1];
            if dx * dx + dy * dy > radius_px * radius_px {
                continue;
            }
            if rgba[(y * w + x) * 4] < 90 {
                sx += x as f64;
                sy += y as f64;
                n += 1;
            }
        }
    }
    assert!(n > 3, "no dark dot found near {near:?}");
    [sx / n as f64, sy / n as f64]
}

/// Measure the distance between two reference dots on the rectified image
/// (in mm) exactly as the two-point measure tool does: pixel distance times
/// mm_per_px, with the "taps" found from the image content itself.
fn measure_dots_mm(
    r: &geometry_core::rectify::Rectified,
    dot_a_mm: [f64; 2],
    dot_b_mm: [f64; 2],
) -> f64 {
    let expect = |d: [f64; 2]| -> [f64; 2] {
        [
            (d[0] - r.origin_mm[0]) / r.mm_per_px,
            (d[1] - r.origin_mm[1]) / r.mm_per_px,
        ]
    };
    let ca = dark_centroid(&r.rgba, r.width, r.height, expect(dot_a_mm), 14.0);
    let cb = dark_centroid(&r.rgba, r.width, r.height, expect(dot_b_mm), 14.0);
    ((ca[0] - cb[0]).powi(2) + (ca[1] - cb[1]).powi(2)).sqrt() * r.mm_per_px
}

#[test]
fn reference_length_near_marker_measures_within_1_percent() {
    // Two taped dots 300 mm apart, just right of the marker.
    let dot_a = [230.0, 30.0];
    let dot_b = [230.0, 330.0];
    let scene = Scene {
        markers: vec![marker_at_origin(0, 150.0)],
        dots: vec![(dot_a, 5.0), (dot_b, 5.0)],
    };
    let rgba = render_scene(&scene, &H_TRUE, W, H, 3.0, 42);
    let r = rectify_frame(&rgba, W, H, 1.0).expect("marker must be detected");

    assert_eq!(r.marker_ids, vec![0]);
    assert_eq!(r.estimate.residuals.len(), 4);
    assert!(
        r.estimate.rms < 0.5,
        "corner reprojection RMS too high: {} px",
        r.estimate.rms
    );

    let measured = measure_dots_mm(&r, dot_a, dot_b);
    let err_pct = (measured - 300.0).abs() / 300.0 * 100.0;
    assert!(
        err_pct < 1.0,
        "reference length off by {err_pct:.2}% (measured {measured:.2} mm, true 300 mm)"
    );
}

#[test]
fn print_scale_correction_factor_multiplies_into_the_metric() {
    // A sheet printed at 94%: the physical marker is 141 mm. Dots are a true
    // 300 mm apart regardless of the print.
    let dot_a = [220.0, 30.0];
    let dot_b = [220.0, 330.0];
    let scene = Scene {
        markers: vec![marker_at_origin(0, 150.0 * 0.94)],
        dots: vec![(dot_a, 5.0), (dot_b, 5.0)],
    };
    let rgba = render_scene(&scene, &H_TRUE, W, H, 2.0, 7);

    let corrected = rectify_frame(&rgba, W, H, 0.94).expect("detected");
    let measured = measure_dots_mm(&corrected, dot_a, dot_b);
    let err_pct = (measured - 300.0).abs() / 300.0 * 100.0;
    assert!(err_pct < 1.0, "corrected measurement off by {err_pct:.2}%");

    // Without the correction the same scene must read ~6% long — proves the
    // factor is really consumed (MULTIPLY rule) and not silently ignored.
    // (In the uncorrected estimate's world frame every physical coordinate
    // is stretched by 1/0.94, so the dots are *searched for* there too.)
    let uncorrected = rectify_frame(&rgba, W, H, 1.0).expect("detected");
    let stretch = |d: [f64; 2]| [d[0] / 0.94, d[1] / 0.94];
    let naive = measure_dots_mm(&uncorrected, stretch(dot_a), stretch(dot_b));
    assert!(
        (naive / measured - 1.0 / 0.94).abs() < 0.01,
        "uncorrected/corrected ratio should be ~1/0.94, got {}",
        naive / measured
    );
}

#[test]
fn sub_pixel_corners_beat_half_a_pixel() {
    let scene = Scene { markers: vec![marker_at_origin(0, 150.0)], dots: vec![] };
    let rgba = render_scene(&scene, &H_TRUE, W, H, 2.0, 3);
    let mut gray = vec![0u8; W * H];
    grayscale(&rgba, &mut gray);
    let found = detect_markers(&gray, W, H);
    assert_eq!(found.len(), 1);
    let m = &found[0];
    assert_eq!(m.id, 0);
    let world = [[0.0, 0.0], [150.0, 0.0], [150.0, 150.0], [0.0, 150.0]];
    let mut total = 0.0;
    for (c, w) in m.corners.iter().zip(world) {
        let t = project(&H_TRUE, w[0], w[1]);
        total += ((c[0] - t[0]).powi(2) + (c[1] - t[1]).powi(2)).sqrt();
    }
    let mean = total / 4.0;
    assert!(mean < 0.5, "mean corner error {mean:.3} px (want sub-pixel)");
}

#[test]
fn rotated_markers_decode_with_correct_id_and_canonical_corners() {
    for rot in 0..4u8 {
        for id in [0u16, 1] {
            let mut m = marker_at_origin(rot, 150.0);
            m.id = id;
            let tl_mm = printed_top_left(&m);
            let scene = Scene { markers: vec![m], dots: vec![] };
            let rgba = render_scene(&scene, &H_TRUE, W, H, 2.0, 11 + rot as u64);
            let mut gray = vec![0u8; W * H];
            grayscale(&rgba, &mut gray);
            let found = detect_markers(&gray, W, H);
            assert_eq!(found.len(), 1, "rot {rot} id {id}: exactly one marker");
            assert_eq!(found[0].id, id, "rot {rot}: wrong ID");
            // corners[0] must be the printed top-left, whatever the rotation
            // (an upside-down marker is rot == 2).
            let expect = project(&H_TRUE, tl_mm[0], tl_mm[1]);
            let got = found[0].corners[0];
            let err = ((got[0] - expect[0]).powi(2) + (got[1] - expect[1]).powi(2)).sqrt();
            assert!(
                err < 1.5,
                "rot {rot} id {id}: canonical TL off by {err:.2} px ({got:?} vs {expect:?})"
            );
        }
    }
}

#[test]
fn both_markers_in_frame_use_the_eight_point_path() {
    let dot_a = [230.0, 40.0];
    let dot_b = [530.0, 40.0];
    let scene = Scene {
        markers: vec![
            marker_at_origin(0, 150.0),
            SyntheticMarker { id: 1, x_mm: 620.0, y_mm: 15.0, side_mm: 150.0, rot_quarter: 0 },
        ],
        dots: vec![(dot_a, 8.0), (dot_b, 8.0)],
    };
    // Wider view so both markers fit: scale the projection down.
    let h_wide = [
        0.62, 0.03, 100.0, //
        -0.02, 0.61, 60.0, //
        1.0e-4, 0.7e-4, 1.0,
    ];
    let rgba = render_scene(&scene, &h_wide, W, H, 2.0, 99);
    let r = rectify_frame(&rgba, W, H, 1.0).expect("markers detected");
    assert_eq!(r.marker_ids, vec![0, 1], "both Reference Markers must be used");
    assert_eq!(r.estimate.residuals.len(), 8, "8 correspondences");
    assert_eq!(r.estimate.inliers, 8);
    // Markers are ~95 px across in this wide two-marker view, so corner
    // noise is higher than the single-marker close-up.
    assert!(r.estimate.rms < 1.0, "rms {}", r.estimate.rms);

    let measured = measure_dots_mm(&r, dot_a, dot_b);
    let err_pct = (measured - 300.0).abs() / 300.0 * 100.0;
    assert!(err_pct < 1.0, "two-marker measurement off by {err_pct:.2}%");
}

#[test]
fn frame_without_marker_detects_nothing() {
    let scene = Scene { markers: vec![], dots: vec![([200.0, 200.0], 30.0)] };
    let rgba = render_scene(&scene, &H_TRUE, W, H, 4.0, 5);
    assert!(rectify_frame(&rgba, W, H, 1.0).is_none());
}

#[test]
fn marker_cut_by_the_frame_edge_is_rejected_not_misread() {
    // Shift the projection so a third of the marker hangs off the left edge.
    let mut h = H_TRUE;
    h[2] = -70.0;
    let scene = Scene { markers: vec![marker_at_origin(0, 150.0)], dots: vec![] };
    let rgba = render_scene(&scene, &h, W, H, 2.0, 8);
    let mut gray = vec![0u8; W * H];
    grayscale(&rgba, &mut gray);
    let found = detect_markers(&gray, W, H);
    assert!(found.is_empty(), "truncated marker must not decode: {found:?}");
}

#[test]
fn invalid_correction_factor_is_rejected() {
    let scene = Scene { markers: vec![marker_at_origin(0, 150.0)], dots: vec![] };
    let rgba = render_scene(&scene, &H_TRUE, W, H, 0.0, 1);
    assert!(rectify_frame(&rgba, W, H, 0.0).is_none());
    assert!(rectify_frame(&rgba, W, H, -1.0).is_none());
    assert!(rectify_frame(&rgba, W, H, f64::NAN).is_none());
}

#[test]
fn rectified_marker_itself_measures_its_physical_side() {
    // On the rectified image the marker's black square must span its
    // physical side within 1% — the most direct metric self-check.
    let scene = Scene { markers: vec![marker_at_origin(0, 150.0)], dots: vec![] };
    let rgba = render_scene(&scene, &H_TRUE, W, H, 2.0, 21);
    let r = rectify_frame(&rgba, W, H, 1.0).expect("detected");
    // Marker corners in rectified px: wall (0,0) and (150,0).
    let px_per_mm = 1.0 / r.mm_per_px;
    let ax = (0.0 - r.origin_mm[0]) * px_per_mm;
    let bx = (150.0 - r.origin_mm[0]) * px_per_mm;
    let y = (75.0 - r.origin_mm[1]) * px_per_mm; // marker mid-height
    // Walk the rectified row and find the black square's left/right edges.
    // Scan only a window around the predicted columns: far outside it the
    // image legitimately holds the dark out-of-source-frame fill.
    let row = y.round() as usize;
    assert!(row < r.height);
    let x0 = (ax - 12.0).max(0.0) as usize;
    let x1 = ((bx + 12.0) as usize).min(r.width - 1);
    let mut left = None;
    let mut right = None;
    for x in x0..=x1 {
        let dark = r.rgba[(row * r.width + x) * 4] < 90;
        if dark && left.is_none() {
            left = Some(x as f64);
        }
        if dark {
            right = Some(x as f64);
        }
    }
    let (left, right) = (left.expect("left edge"), right.expect("right edge"));
    // Sanity: found edges sit near the predicted marker corner columns.
    assert!((left - ax).abs() < 5.0 && (right - bx).abs() < 5.0);
    let side_mm = (right - left + 1.0) * r.mm_per_px;
    let err_pct = (side_mm - 150.0).abs() / 150.0 * 100.0;
    assert!(err_pct < 1.0, "marker side measures {side_mm:.2} mm ({err_pct:.2}% off)");
}
