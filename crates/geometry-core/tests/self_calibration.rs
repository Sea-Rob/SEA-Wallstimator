//! Synthetic ground-truth tests for self-calibrated intrinsics (issue #6):
//! render pans through a camera with KNOWN focal length and division-model
//! k1 radial distortion (`render_scene_distorted`), run the full recorded-pan
//! pipeline, and verify:
//!
//! * the joint LM recovers focal and k1 within documented tolerance
//!   (focal within 5%, k1 within 0.02 absolute),
//! * a straight row of wall dots reaching the frame edges stays straight in
//!   the Rectified Wall Image (straightness metric: max deviation from the
//!   least-squares line through the dot centroids) — and straighter than
//!   the uncalibrated baseline,
//! * the Error Bound on the distorted-camera capture SHRINKS versus the
//!   forced-pinhole baseline AND still contains the true error (the
//!   distance contract, `bound_between_mm`),
//! * degenerate chains (too short, or translation-only with no rotation
//!   wobble) honestly refuse to claim calibrated intrinsics and fall back
//!   to the pinhole default.

use std::sync::OnceLock;

use geometry_core::calib::Distortion;
use geometry_core::pan::{PanCore, PanOutput};
use geometry_core::synthetic::{render_scene_distorted, PanCamera, Scene, SyntheticMarker};

const W: usize = 640;
const H: usize = 360;
const B_X_MM: f64 = 3850.0;

/// Ground-truth camera: same focal as the clean-pan fixture, plus real
/// barrel distortion (division model, radius normalized by the half frame
/// diagonal — the exact model the pipeline estimates).
const FOCAL_TRUE: f64 = 700.0;
const K1_TRUE: f64 = -0.08;

/// Documented recovery tolerances (also quoted in the README).
const FOCAL_TOL_REL: f64 = 0.05;
const K1_TOL: f64 = 0.02;

/// Wall y of the straightness dot row: near the TOP edge of the camera
/// frame throughout the pan (frame centre tracks wall y = 280 mm, the frame
/// reaches to y ~ -80 mm), which is where radial distortion bends geometry
/// hardest.
const LINE_Y_MM: f64 = -40.0;

fn line_xs() -> Vec<f64> {
    (0..14).map(|i| 300.0 + 250.0 * i as f64).collect()
}

fn marker(id: u16, x_mm: f64, y_mm: f64) -> SyntheticMarker {
    SyntheticMarker { id, x_mm, y_mm, side_mm: 150.0, rot_quarter: 0 }
}

fn camera() -> PanCamera {
    PanCamera {
        focal_px: FOCAL_TRUE,
        width: W,
        height: H,
        distance_mm: 1400.0,
        start_center_mm: [500.0, 280.0],
        end_center_mm: [3450.0, 280.0],
        // More rotation wobble than the clean-pan fixture — still hand-held
        // realistic (~3.4 deg peak yaw), and rotation is exactly what makes
        // the focal observable in plane-based self-calibration.
        yaw_amp: 0.06,
        pitch_amp: 0.02,
    }
}

/// Measurement pairs + tracking texture, as in tests/pan_sequence.rs, plus
/// the straightness dot row along the frame's top edge.
fn dots() -> Vec<([f64; 2], f64)> {
    let mut d = vec![
        ([250.0, 250.0], 8.0),
        ([250.0, 550.0], 8.0),
        ([1900.0, 250.0], 8.0),
        ([2200.0, 250.0], 8.0),
        ([3700.0, 150.0], 8.0),
        ([3700.0, 450.0], 8.0),
    ];
    let mut x = 450.0;
    let mut k = 0u32;
    while x < 3600.0 {
        let y0 = [120.0, 480.0, 300.0][k as usize % 3];
        d.push(([x, y0], 6.0));
        d.push(([x + 90.0, 620.0 - y0], 5.0));
        d.push(([x + 45.0, y0 + 160.0], 4.0));
        k += 1;
        x += 210.0 + 20.0 * ((k % 4) as f64);
    }
    for x in line_xs() {
        d.push(([x, LINE_Y_MM], 7.0));
    }
    d
}

fn scene() -> Scene {
    Scene {
        markers: vec![marker(0, 0.0, 0.0), marker(1, B_X_MM, 0.0)],
        dots: dots(),
    }
}

/// Render the distorted pan ONCE and process it twice: with the production
/// self-calibration path and with the forced-pinhole baseline. Shared by
/// the recovery / straightness / bound tests (rendering dominates runtime).
fn fixture() -> &'static (PanOutput, PanOutput) {
    static FIX: OnceLock<(PanOutput, PanOutput)> = OnceLock::new();
    FIX.get_or_init(|| {
        let scene = scene();
        let cam = camera();
        let dist = Distortion::new(W, H, K1_TRUE);
        let mut cal = PanCore::new(W, H);
        let mut base = PanCore::new(W, H);
        for (i, h) in cam.sequence(60).iter().enumerate() {
            let rgba = render_scene_distorted(&scene, h, W, H, 2.5, 1000 + i as u64, &dist);
            cal.push_frame(&rgba);
            base.push_frame(&rgba);
        }
        let calibrated = cal.finish(1.0, true).expect("calibrated pan must process");
        let baseline = base
            .finish_with(1.0, true, false)
            .expect("pinhole baseline must process");
        (calibrated, baseline)
    })
}

/// Centroid of dark pixels near an expected output position (output px) —
/// the Homeowner's measure-tool tap, as in tests/pan_sequence.rs. `None`
/// when no dark dot is present in the window (a grossly mis-positioned
/// stitch can move content arbitrarily far from its true wall position).
fn dark_centroid_opt(out: &PanOutput, near_mm: [f64; 2], radius_px: f64) -> Option<[f64; 2]> {
    let cx = (near_mm[0] - out.origin_mm[0]) / out.mm_per_px;
    let cy = (near_mm[1] - out.origin_mm[1]) / out.mm_per_px;
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0u32);
    for y in 0..out.height {
        for x in 0..out.width {
            let dx = x as f64 - cx;
            let dy = y as f64 - cy;
            if dx * dx + dy * dy > radius_px * radius_px {
                continue;
            }
            let px = &out.rgba[(y * out.width + x) * 4..(y * out.width + x) * 4 + 4];
            if px[0] < 90 && !(px[0] == 24 && px[1] == 24 && px[2] == 28) {
                sx += x as f64;
                sy += y as f64;
                n += 1;
            }
        }
    }
    (n > 3).then(|| [sx / n as f64, sy / n as f64])
}

fn dark_centroid(out: &PanOutput, near_mm: [f64; 2], radius_px: f64) -> [f64; 2] {
    dark_centroid_opt(out, near_mm, radius_px)
        .unwrap_or_else(|| panic!("no dark dot found near wall {near_mm:?}"))
}

fn measure_mm(out: &PanOutput, a_mm: [f64; 2], b_mm: [f64; 2], radius_px: f64) -> f64 {
    let ca = dark_centroid(out, a_mm, radius_px);
    let cb = dark_centroid(out, b_mm, radius_px);
    ((ca[0] - cb[0]).powi(2) + (ca[1] - cb[1]).powi(2)).sqrt() * out.mm_per_px
}

/// Straightness metric (issue criterion): max deviation (mm) of the dot-row
/// centroids from their own least-squares line across the full wall.
/// Returns the deviation and how many of the row's dots could be located
/// within `radius_px` of their true wall positions; `None` when fewer than
/// 10 of 14 were found (the stitch's geometry is too broken to even measure
/// the row — which is itself a result).
fn line_straightness_mm(out: &PanOutput, radius_px: f64) -> Option<(f64, usize)> {
    let pts: Vec<[f64; 2]> = line_xs()
        .iter()
        .filter_map(|&x| {
            let c = dark_centroid_opt(out, [x, LINE_Y_MM], radius_px)?;
            Some([
                out.origin_mm[0] + c[0] * out.mm_per_px,
                out.origin_mm[1] + c[1] * out.mm_per_px,
            ])
        })
        .collect();
    if pts.len() < 10 {
        return None;
    }
    // Least-squares y = a + b·x; the line is near-horizontal, so the
    // vertical residual is the deviation (to first order).
    let n = pts.len() as f64;
    let mx = pts.iter().map(|p| p[0]).sum::<f64>() / n;
    let my = pts.iter().map(|p| p[1]).sum::<f64>() / n;
    let sxx: f64 = pts.iter().map(|p| (p[0] - mx).powi(2)).sum();
    let sxy: f64 = pts.iter().map(|p| (p[0] - mx) * (p[1] - my)).sum();
    let b = sxy / sxx.max(1e-12);
    let dev = pts
        .iter()
        .map(|p| (p[1] - (my + b * (p[0] - mx))).abs())
        .fold(0.0, f64::max);
    Some((dev, pts.len()))
}

#[test]
fn probe_dispositions_tmp() {
    let scene = scene();
    let cam = camera();
    let dist = Distortion::new(W, H, K1_TRUE);
    let mut core = PanCore::new(W, H);
    let mut line = String::new();
    for (i, h) in cam.sequence(60).iter().enumerate() {
        let rgba = render_scene_distorted(&scene, h, W, H, 2.5, 1000 + i as u64, &dist);
        let d = core.push_frame(&rgba);
        line.push(match d {
            geometry_core::pan::FrameDisposition::Kept => 'K',
            geometry_core::pan::FrameDisposition::Skipped => '.',
            geometry_core::pan::FrameDisposition::Candidate => 'c',
            geometry_core::pan::FrameDisposition::CapReached => 'X',
        });
    }
    println!("dispositions: {line}");
    println!("keyframes {}, broken {}", core.keyframe_count(), core.tracking_broken());
}

#[test]
fn probe_debug_tmp() {
    let (cal, base) = fixture();
    for (name, o) in [("cal", cal), ("base", base)] {
        println!(
            "{name}: kf {} size {}x{} mmpp {:.3} origin ({:.0},{:.0}) far_x {:.0} closure {:?} \
             rejected {} bound near {:.1} far {:.1} calib {:?}",
            o.keyframes_used,
            o.width,
            o.height,
            o.mm_per_px,
            o.origin_mm[0],
            o.origin_mm[1],
            o.far_x_mm,
            o.closure,
            o.closure_rejected,
            o.bound_near_anchor_mm(),
            o.bound_far_end_mm(),
            o.calibration,
        );
        // Where did the dot near wall (1300, -40) actually land?
        let cx = ((1300.0 - o.origin_mm[0]) / o.mm_per_px) as i64;
        let cy = ((-40.0 - o.origin_mm[1]) / o.mm_per_px) as i64;
        let mut best = (255u8, 0i64, 0i64);
        for dy in -60..=60i64 {
            for dx in -60..=60i64 {
                let x = cx + dx;
                let y = cy + dy;
                if x < 0 || y < 0 || x >= o.width as i64 || y >= o.height as i64 {
                    continue;
                }
                let px = &o.rgba[((y as usize) * o.width + x as usize) * 4..][..3];
                if px[0] < best.0 && !(px[0] == 24 && px[1] == 24 && px[2] == 28) {
                    best = (px[0], dx, dy);
                }
            }
        }
        println!("  darkest px near (1300,-40): luma {} at offset ({}, {})", best.0, best.1, best.2);
        for (i, l) in o.links.iter().enumerate() {
            println!("  link {i}: {} matches, {} inliers, rms {:.2} px", l.matches, l.inliers, l.rms_px);
        }
    }
}

#[test]
fn calibration_recovers_known_focal_and_k1() {
    let (out, _) = fixture();
    let calib = out
        .calibration
        .as_ref()
        .expect("a 60-frame wobbly pan across two markers must self-calibrate");
    println!(
        "recovered focal {:.1} px (true {FOCAL_TRUE}, sigma {:.1}), k1 {:.4} (true {K1_TRUE}, \
         sigma {:.4}); bundle rms {:.2} -> {:.2} px",
        calib.focal_px,
        calib.focal_sigma_px,
        calib.k1,
        calib.k1_sigma,
        calib.rms_before_px,
        calib.rms_after_px
    );
    let f_err = (calib.focal_px - FOCAL_TRUE).abs() / FOCAL_TRUE;
    assert!(
        f_err <= FOCAL_TOL_REL,
        "focal {:.1} px is {:.1}% off the true {FOCAL_TRUE} px (tolerance {}%)",
        calib.focal_px,
        f_err * 100.0,
        FOCAL_TOL_REL * 100.0
    );
    let k1_err = (calib.k1 - K1_TRUE).abs();
    assert!(
        k1_err <= K1_TOL,
        "k1 {:.4} is {:.4} off the true {K1_TRUE} (tolerance {K1_TOL})",
        calib.k1,
        k1_err
    );
    assert!(
        calib.rms_after_px < calib.rms_before_px,
        "joint refinement must reduce the bundle rms"
    );
}

#[test]
fn straight_dot_row_stays_straight_to_the_frame_edges() {
    let (calibrated, baseline) = fixture();
    // Calibrated: every dot must be findable AT its true wall position
    // (tight window) and the row must be straight.
    let (dev_cal, found_cal) =
        line_straightness_mm(calibrated, 15.0).expect("calibrated row must be measurable");
    assert_eq!(found_cal, line_xs().len(), "calibrated stitch must place every row dot");
    // Uncalibrated: a generous window, and the row may not even be
    // measurable — the uncorrected chain misplaces content by far more
    // than the search radius, which is the failure mode calibration fixes.
    match line_straightness_mm(baseline, 30.0) {
        Some((dev_base, found_base)) => {
            println!(
                "straightness (max deviation from LS line, wall y = {LINE_Y_MM} mm): \
                 calibrated {dev_cal:.2} mm ({found_cal} dots) vs uncalibrated \
                 {dev_base:.2} mm ({found_base} dots)"
            );
            assert!(
                dev_cal < dev_base,
                "calibration must straighten the frame-edge dot row: \
                 {dev_cal:.2} vs {dev_base:.2} mm"
            );
        }
        None => println!(
            "straightness: calibrated {dev_cal:.2} mm ({found_cal} dots); uncalibrated row \
             UNMEASURABLE — the pinhole chain misplaces the dots by more than 30 px \
             (an even stronger form of bent geometry)"
        ),
    }
    // Absolute criterion: straight to within the stitch's own resolution
    // (~2 mm/px) — visible bowing is gone, not merely reduced.
    assert!(
        dev_cal <= 3.0,
        "calibrated dot row still bows {dev_cal:.2} mm from straight"
    );
}

#[test]
fn error_bound_shrinks_and_still_contains_the_true_error() {
    let (calibrated, baseline) = fixture();

    // The bound must have tightened because its measured inputs (anchor
    // residual, link RMS, closure state) genuinely improved. Compare the
    // DISTANCE bound over the same wall span for both results — the two
    // runs render different extents, so far-end scalars would compare
    // different positions.
    let span = (250.0, 3700.0);
    let cal_bound = calibrated.bound.bound_between_mm(span.0, span.1);
    let base_bound = baseline.bound.bound_between_mm(span.0, span.1);
    println!(
        "distance bound over [{:.0}, {:.0}] mm: calibrated {cal_bound:.1} mm vs pinhole \
         baseline {base_bound:.1} mm (baseline closure applied: {}, rejected: {})",
        span.0,
        span.1,
        baseline.closure.is_some(),
        baseline.closure_rejected,
    );
    assert!(calibrated.calibration.is_some());
    assert!(baseline.calibration.is_none(), "forced-pinhole baseline must not calibrate");
    assert!(
        cal_bound < base_bound,
        "calibration must tighten the distance bound: {cal_bound:.1} vs {base_bound:.1} mm"
    );

    // ...and the tightened bound must still contain the true error: the
    // DISTANCE contract (bound_between_mm), checked at both ends, the
    // middle, and across the full wall.
    let cases = [
        ("near A", [250.0, 250.0], [250.0, 550.0], 300.0),
        ("middle", [1900.0, 250.0], [2200.0, 250.0], 300.0),
        ("near B", [3700.0, 150.0], [3700.0, 450.0], 300.0),
        (
            "full span",
            [250.0, 250.0],
            [3700.0, 150.0],
            ((3700.0f64 - 250.0).powi(2) + (150.0f64 - 250.0).powi(2)).sqrt(),
        ),
    ];
    for (name, a, b, true_mm) in cases {
        let measured = measure_mm(calibrated, a, b, 18.0);
        let allowed = calibrated.bound.bound_between_mm(a[0], b[0]);
        let err = (measured - true_mm).abs();
        println!(
            "{name}: measured {measured:.1} mm (true {true_mm:.1}), err {err:.1} <= bound {allowed:.1}"
        );
        assert!(
            err <= allowed,
            "{name}: err {err:.1} mm exceeds the calibrated bound {allowed:.1} mm — \
             the tightened bound broke the distance contract"
        );
    }
}

/// A 2-keyframe barely-moving pan cannot separate focal from pose: it must
/// NOT claim calibrated intrinsics, even though the lens genuinely is
/// distorted. The honest result is the pinhole fallback with the wider
/// uncalibrated bound.
#[test]
fn short_barely_moving_pan_refuses_to_claim_calibration() {
    let scene = scene();
    let cam = PanCamera {
        focal_px: FOCAL_TRUE,
        width: W,
        height: H,
        distance_mm: 1400.0,
        start_center_mm: [500.0, 280.0],
        end_center_mm: [1020.0, 280.0],
        yaw_amp: 0.0,
        pitch_amp: 0.0,
    };
    let dist = Distortion::new(W, H, K1_TRUE);
    let mut core = PanCore::new(W, H);
    for (i, h) in cam.sequence(30).iter().enumerate() {
        let rgba = render_scene_distorted(&scene, h, W, H, 2.5, 4000 + i as u64, &dist);
        core.push_frame(&rgba);
    }
    let out = core.finish(1.0, true).expect("short pan must still process open-loop");
    println!(
        "short pan: {} keyframes, calibrated: {}",
        out.keyframes_used,
        out.calibration.is_some()
    );
    assert!(
        out.keyframes_used <= 3,
        "fixture drift: this test needs a SHORT chain, got {} keyframes",
        out.keyframes_used
    );
    assert!(
        out.calibration.is_none(),
        "a {}-keyframe barely-moving pan must not claim calibrated intrinsics",
        out.keyframes_used
    );
}

/// Translation-only pan (zero rotation wobble, perfectly fronto-parallel):
/// every view constrains the focal identically, i.e. not at all — the LM
/// curvature gate must refuse, even with plenty of keyframes.
#[test]
fn fronto_parallel_translation_only_pan_refuses_calibration() {
    // Denser tracking texture than the shared fixture: without yaw wobble
    // the keyframe overlap windows land where the sparse fixture leaves few
    // matchable dots, and the chain would fail in pass 1 for texture
    // reasons — this test is about CALIBRATION refusal, not weak links.
    let mut d = dots();
    let mut x = 350.0;
    while x < 2600.0 {
        d.push(([x, 200.0], 5.0));
        d.push(([x + 60.0, 420.0], 4.0));
        x += 130.0;
    }
    let scene = Scene {
        markers: vec![marker(0, 0.0, 0.0), marker(1, B_X_MM, 0.0)],
        dots: d,
    };
    let cam = PanCamera {
        focal_px: FOCAL_TRUE,
        width: W,
        height: H,
        distance_mm: 1400.0,
        start_center_mm: [400.0, 280.0],
        end_center_mm: [2200.0, 280.0],
        yaw_amp: 0.0,
        pitch_amp: 0.0,
    };
    let dist = Distortion::new(W, H, K1_TRUE);
    let mut core = PanCore::new(W, H);
    for (i, h) in cam.sequence(60).iter().enumerate() {
        let rgba = render_scene_distorted(&scene, h, W, H, 2.5, 5000 + i as u64, &dist);
        core.push_frame(&rgba);
    }
    let out = core.finish(1.0, true).expect("translation pan must process");
    println!(
        "translation-only pan: {} keyframes, calibrated: {}",
        out.keyframes_used,
        out.calibration.is_some()
    );
    assert!(out.keyframes_used >= 4, "fixture drift: expected a longer chain");
    assert!(
        out.calibration.is_none(),
        "a rotation-free fronto-parallel chain must not claim a focal"
    );
}
