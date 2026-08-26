//! Synthetic ground-truth tests for the recorded-pan pipeline (issue #4):
//! render a camera pan across a ~4 m wall carrying both Reference Markers
//! plus reference dots at both ends and the middle (blur via supersampling,
//! additive noise), feed every frame through the capture-time keyframe
//! selector, run the full tracking -> chaining -> loop-closure -> stitching
//! pipeline, and verify:
//!
//! * known distances at both ends and the middle are recovered within the
//!   REPORTED Error Bound,
//! * the Error Bound itself is <= 30 mm on a clean capture,
//! * loop closure genuinely reduces the far-end error versus the same
//!   keyframes processed open-loop,
//! * an untrackable segment breaks the chain loudly, naming the segment.

use geometry_core::pan::{PanCore, PanError, PanOutput, MAX_CLOSURE_DISCREPANCY_MM};
use geometry_core::synthetic::{render_scene, PanCamera, Scene, SyntheticMarker};

const W: usize = 640;
const H: usize = 360;

/// Wall layout: Marker A's printed top-left at wall (0, 0) — the wall frame
/// and the scene frame coincide, so ground truth needs no conversion.
const B_X_MM: f64 = 3850.0;

fn marker(id: u16, x_mm: f64, y_mm: f64) -> SyntheticMarker {
    SyntheticMarker { id, x_mm, y_mm, side_mm: 150.0, rot_quarter: 0 }
}

fn camera() -> PanCamera {
    PanCamera {
        focal_px: 700.0,
        width: W,
        height: H,
        distance_mm: 1400.0,
        start_center_mm: [500.0, 280.0],
        end_center_mm: [3450.0, 280.0],
        yaw_amp: 0.035,
        pitch_amp: 0.02,
    }
}

/// Reference dots: pairs 300 mm apart near Marker A, in the middle, and near
/// Marker B, plus spread "fixture" texture through the middle so inter-frame
/// tracking has something to hold onto (walls are low-texture; power points,
/// skirting shadows and the markers themselves play this role in reality).
fn dots() -> Vec<([f64; 2], f64)> {
    let mut d = vec![
        // Near-A measurement pair (vertical, 300 mm).
        ([250.0, 250.0], 8.0),
        ([250.0, 550.0], 8.0),
        // Middle measurement pair (horizontal, 300 mm).
        ([1900.0, 250.0], 8.0),
        ([2200.0, 250.0], 8.0),
        // Near-B measurement pair (vertical, 300 mm).
        ([3700.0, 150.0], 8.0),
        ([3700.0, 450.0], 8.0),
    ];
    // Tracking texture spread along the pan: irregular clusters standing in
    // for power points, skirting shadows, paint blemishes.
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
    d
}

fn scene() -> Scene {
    Scene {
        markers: vec![marker(0, 0.0, 0.0), marker(1, B_X_MM, 0.0)],
        dots: dots(),
    }
}

/// Push a rendered pan into a fresh PanCore.
fn record_pan(scene: &Scene, frames: usize, noise: f64, seed: u64) -> PanCore {
    let cam = camera();
    let mut core = PanCore::new(W, H);
    for (i, h) in cam.sequence(frames).iter().enumerate() {
        let rgba = render_scene(scene, h, W, H, noise, seed + i as u64);
        core.push_frame(&rgba);
    }
    core
}

/// Centroid of dark pixels near an expected output position — the test's
/// stand-in for the Homeowner tapping the measure tool.
fn dark_centroid(out: &PanOutput, near_mm: [f64; 2], radius_px: f64) -> [f64; 2] {
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
            // Skip the out-of-coverage fill (24, 24, 28).
            if px[0] < 90 && !(px[0] == 24 && px[1] == 24 && px[2] == 28) {
                sx += x as f64;
                sy += y as f64;
                n += 1;
            }
        }
    }
    assert!(n > 3, "no dark dot found near wall {near_mm:?} (output px {cx:.0},{cy:.0})");
    [sx / n as f64, sy / n as f64]
}

/// Measured mm distance between two dots on the stitched image.
/// `radius_px` is the tap search window: open-loop results drift far enough
/// that the dot must be hunted over a wider area.
fn measure_mm(out: &PanOutput, a_mm: [f64; 2], b_mm: [f64; 2], radius_px: f64) -> f64 {
    let ca = dark_centroid(out, a_mm, radius_px);
    let cb = dark_centroid(out, b_mm, radius_px);
    ((ca[0] - cb[0]).powi(2) + (ca[1] - cb[1]).powi(2)).sqrt() * out.mm_per_px
}

#[test]
fn pan_recovers_known_distances_within_the_reported_error_bound() {
    let scene = scene();
    let mut core = record_pan(&scene, 60, 2.5, 1000);
    let out = core.finish(1.0, true).expect("pan must process");

    assert!(
        (4..=30).contains(&out.keyframes_used),
        "keyframe selection out of range: {}",
        out.keyframes_used
    );
    assert!(!out.truncated);
    let closure = out.closure.as_ref().expect("Marker B must close the loop");
    assert!(!out.closure_rejected, "clean closure must not be rejected");
    // Same constant the production plausibility guard enforces: the test's
    // notion of "implausible" can never drift from the shipping one.
    assert!(
        closure.discrepancy_mm < MAX_CLOSURE_DISCREPANCY_MM,
        "implausible measured drift: {} mm",
        closure.discrepancy_mm
    );

    // The stitched image must cover the whole wall — far wider than any
    // single frame's ~1280 mm footprint.
    let span_mm = out.width as f64 * out.mm_per_px;
    assert!(span_mm > 3800.0, "stitched span only {span_mm:.0} mm");

    // Error Bound <= 30 mm everywhere on a clean capture (issue criterion).
    let far = out.bound_far_end_mm();
    assert!(far <= 30.0, "far-end Error Bound {far:.1} mm exceeds 30 mm");
    assert!(out.bound_near_anchor_mm() < far, "bound must grow with distance");

    // Known 300 mm distances at both ends and the middle, each within the
    // bound at its own wall position (sum of the two endpoint bounds).
    let cases = [
        ("near A", [250.0, 250.0], [250.0, 550.0], 300.0),
        ("middle", [1900.0, 250.0], [2200.0, 250.0], 300.0),
        ("near B", [3700.0, 150.0], [3700.0, 450.0], 300.0),
    ];
    println!(
        "keyframes {} | bound near {:.1} mm, far {:.1} mm (at x={:.0}) | closure: \
         discrepancy {:.1} mm, residual {:.1} mm, scale {:.4}",
        out.keyframes_used,
        out.bound_near_anchor_mm(),
        out.bound_far_end_mm(),
        out.far_x_mm,
        closure.discrepancy_mm,
        closure.residual_mm,
        closure.scale_correction
    );
    for (name, a, b, true_mm) in cases {
        let measured = measure_mm(&out, a, b, 18.0);
        // THE Error Bound contract (CONTEXT.md via BoundModel docs): a 95%
        // bound on DISTANCES between two wall positions — what the measure
        // tool and Clear Zone dimensions consume.
        let allowed = out.bound.bound_between_mm(a[0], b[0]);
        let err = (measured - true_mm).abs();
        println!("{name}: measured {measured:.1} mm (true {true_mm}), err {err:.1} <= bound {allowed:.1}");
        assert!(
            err <= allowed,
            "{name}: measured {measured:.1} mm vs {true_mm} mm (err {err:.1}) \
             exceeds reported bound {allowed:.1} mm"
        );
    }

    // The full-wall span: near-A dot to near-B dot, 3450 mm apart in x.
    let a = [250.0f64, 250.0];
    let b = [3700.0f64, 150.0];
    let true_mm = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    let measured = measure_mm(&out, a, b, 18.0);
    let allowed = out.bound.bound_between_mm(a[0], b[0]);
    let err = (measured - true_mm).abs();
    println!("full span: measured {measured:.1} mm (true {true_mm:.1}), err {err:.1} <= bound {allowed:.1}");
    assert!(
        err <= allowed,
        "full span: measured {measured:.1} vs {true_mm:.1} (err {err:.1}) > bound {allowed:.1}"
    );
}

#[test]
fn loop_closure_reduces_far_end_error() {
    // Noisier capture: per-link tracking errors accumulate into real scale
    // drift over the chain, which is exactly what closure corrects. Fixed
    // seed keeps CI deterministic.
    let scene = scene();
    let a = [250.0f64, 250.0];
    let b = [3700.0f64, 150.0];
    let true_mm = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();

    let mut open_core = record_pan(&scene, 60, 6.0, 7777);
    let open = open_core.finish(1.0, false).expect("open-loop pan must process");
    assert!(open.closure.is_none(), "close_loop=false must skip closure");
    let open_err = (measure_mm(&open, a, b, 60.0) - true_mm).abs();

    let mut closed_core = record_pan(&scene, 60, 6.0, 7777);
    let closed = closed_core.finish(1.0, true).expect("closed-loop pan must process");
    let closure = closed.closure.as_ref().expect("closure must engage");
    let closed_err = (measure_mm(&closed, a, b, 60.0) - true_mm).abs();

    println!(
        "closure comparison: open-loop far error {open_err:.1} mm vs closed {closed_err:.1} mm \
         (scale correction {:.4}, discrepancy {:.1} mm); bounds far: open {:.1} / closed {:.1} mm",
        closure.scale_correction,
        closure.discrepancy_mm,
        open.bound_far_end_mm(),
        closed.bound_far_end_mm()
    );
    // The mechanism must have measured real drift and corrected it.
    assert!(
        (closure.scale_correction - 1.0).abs() > 1e-4,
        "closure measured no drift (scale {}), test is vacuous",
        closure.scale_correction
    );
    assert!(
        closed_err < open_err,
        "closure must reduce far-end error: open {open_err:.2} mm, closed {closed_err:.2} mm"
    );
    // And the open-loop bound must be honest about not having a closure:
    // wider than the closed-loop bound at the far end.
    assert!(
        open.bound_far_end_mm() > closed.bound_far_end_mm(),
        "open-loop bound {:.1} must exceed closed-loop bound {:.1}",
        open.bound_far_end_mm(),
        closed.bound_far_end_mm()
    );
}

/// Texture only near the ends, with a lone fixture cluster inside a gap
/// wider than one camera view: the fixture keeps the coarse tracker alive
/// (keyframes are kept) but consecutive keyframes in the gap share nothing
/// except that one tight cluster.
fn gappy_dots() -> Vec<([f64; 2], f64)> {
    let mut d = vec![
        ([250.0, 250.0], 8.0),
        ([250.0, 550.0], 8.0),
        // The lone middle fixture cluster (spans < 25% of a frame).
        ([2050.0, 350.0], 40.0),
        ([2130.0, 300.0], 10.0),
        ([2000.0, 450.0], 10.0),
        ([3700.0, 150.0], 8.0),
        ([3700.0, 450.0], 8.0),
    ];
    for x in [450.0, 650.0, 850.0, 1050.0, 1200.0] {
        d.push(([x, 480.0], 6.0));
        d.push(([x + 90.0, 150.0], 5.0));
    }
    for x in [2900.0, 3100.0, 3300.0, 3450.0] {
        d.push(([x, 480.0], 6.0));
        d.push(([x + 90.0, 150.0], 5.0));
    }
    d
}

#[test]
fn untrackable_segment_fails_loudly_naming_the_segment() {
    // The chain must break with a WeakSegment error — a homography "fit"
    // to a single tight feature cluster extrapolates garbage and must not
    // silently corrupt the chain.
    let scene = Scene {
        markers: vec![marker(0, 0.0, 0.0), marker(1, B_X_MM, 0.0)],
        dots: gappy_dots(),
    };
    let mut core = record_pan(&scene, 60, 2.5, 4242);
    match core.finish(1.0, true) {
        Err(PanError::WeakSegment { from_keyframe, to_keyframe, .. }) => {
            assert_eq!(to_keyframe, from_keyframe + 1);
            let msg = PanError::WeakSegment {
                from_keyframe,
                to_keyframe,
                matches: 3,
                inliers: 0,
            }
            .to_string();
            assert!(msg.contains("tracking failed between keyframe"));
        }
        Err(other) => panic!("expected WeakSegment, got: {other}"),
        Ok(out) => panic!(
            "chain across a featureless gap must fail loudly, but produced a \
             {}x{} image from {} keyframes",
            out.width, out.height, out.keyframes_used
        ),
    }
}

#[test]
fn losing_track_mid_pan_fails_loudly() {
    // A fully blank middle stretch wider than one view: the coarse tracker
    // loses the wall entirely, later keyframes sit beyond the gap, and the
    // recorder must report the loss instead of chaining across it.
    let mut dots: Vec<([f64; 2], f64)> = vec![
        ([250.0, 250.0], 8.0),
        ([250.0, 550.0], 8.0),
        ([3700.0, 150.0], 8.0),
        ([3700.0, 450.0], 8.0),
    ];
    for x in [450.0, 650.0, 850.0, 1050.0, 1200.0] {
        dots.push(([x, 480.0], 6.0));
        dots.push(([x + 90.0, 150.0], 5.0));
    }
    for x in [2900.0, 3100.0, 3300.0, 3450.0] {
        dots.push(([x, 480.0], 6.0));
        dots.push(([x + 90.0, 150.0], 5.0));
    }
    let scene = Scene {
        markers: vec![marker(0, 0.0, 0.0), marker(1, B_X_MM, 0.0)],
        dots,
    };
    let mut core = record_pan(&scene, 60, 2.5, 99);
    // Depending on exactly where trackable texture peters out, either the
    // capture-time continuity guard (TrackingLost) or the per-link guard
    // (WeakSegment) fires first — both are loud and both name the spot.
    match core.finish(1.0, true) {
        Err(PanError::TrackingLost { .. }) | Err(PanError::WeakSegment { .. }) => {}
        Err(other) => panic!("expected TrackingLost/WeakSegment, got: {other}"),
        Ok(_) => panic!("a pan with a lost middle must not silently produce a result"),
    }
}

#[test]
fn implausible_closure_is_refused_not_redistributed() {
    // Marker B physically 130 mm but claimed 150 mm — a stand-in for every
    // "closure demands a big scale rewrite" failure (blurred B, rotate-in-
    // place sweep, marker off the wall plane). The measured scale
    // correction (~1.15) is far beyond MAX_CLOSURE_SCALE_DEV: the guard
    // must REFUSE the closure, return the honest open-loop result, and
    // raise the closure_rejected flag — never redistribute the garbage.
    let bad_scene = Scene {
        markers: vec![
            marker(0, 0.0, 0.0),
            SyntheticMarker { id: 1, x_mm: B_X_MM, y_mm: 0.0, side_mm: 130.0, rot_quarter: 0 },
        ],
        dots: dots(),
    };
    let mut core = record_pan(&bad_scene, 60, 2.5, 1000);
    let out = core.finish(1.0, true).expect("pan must still process open-loop");
    assert!(
        out.closure.is_none(),
        "an implausible closure (scale {:?}) must not be applied",
        out.closure.map(|c| c.scale_correction)
    );
    assert!(
        out.closure_rejected,
        "refusing a visible Marker B must raise the flag, never stay silent"
    );
    // And the bound must be the honest open-loop one: far wider than a
    // clean closed-loop capture's.
    let mut clean_core = record_pan(&scene(), 60, 2.5, 1000);
    let clean = clean_core.finish(1.0, true).expect("clean pan");
    assert!(
        out.bound_far_end_mm() > 2.0 * clean.bound_far_end_mm(),
        "rejected closure must fall back to the wide open-loop bound: \
         rejected {:.1} mm vs clean {:.1} mm",
        out.bound_far_end_mm(),
        clean.bound_far_end_mm()
    );
}

/// 3x3 box blur on an RGBA frame (test stand-in for motion blur at B).
fn blur3(rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = rgba.to_vec();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            for ch in 0..3 {
                let mut acc = 0u32;
                for dy in 0..3 {
                    for dx in 0..3 {
                        acc += rgba[((y + dy - 1) * w + x + dx - 1) * 4 + ch] as u32;
                    }
                }
                out[(y * w + x) * 4 + ch] = (acc / 9) as u8;
            }
        }
    }
    out
}

#[test]
fn blurred_marker_b_widens_the_bound_not_tightens_it() {
    // Issue #4 review: with a CONSTANT closure-precision term, blurring
    // Marker B silently tightened the bound while poisoning the result.
    // The empirical per-view scatter must respond instead: same pan, same
    // seed, blur over the last 30% of frames (where B lives) => the far
    // bound must come out wider than the clean run's (or the closure must
    // be refused outright, which yields the even wider open-loop bound).
    let scene = scene();
    let cam = camera();
    let frames = 60usize;

    let mut clean_core = PanCore::new(W, H);
    let mut blurred_core = PanCore::new(W, H);
    for (i, h) in cam.sequence(frames).iter().enumerate() {
        let rgba = render_scene(&scene, h, W, H, 2.5, 1000 + i as u64);
        clean_core.push_frame(&rgba);
        if i >= frames * 7 / 10 {
            blurred_core.push_frame(&blur3(&rgba, W, H));
        } else {
            blurred_core.push_frame(&rgba);
        }
    }
    let clean = clean_core.finish(1.0, true).expect("clean pan");
    assert!(clean.closure.is_some() && !clean.closure_rejected);
    let blurred = blurred_core.finish(1.0, true).expect("blurred pan");

    println!(
        "far bound: clean {:.1} mm vs blurred-B {:.1} mm (closure applied: {}, rejected: {})",
        clean.bound_far_end_mm(),
        blurred.bound_far_end_mm(),
        blurred.closure.is_some(),
        blurred.closure_rejected,
    );
    assert!(
        blurred.bound_far_end_mm() > clean.bound_far_end_mm(),
        "blurring Marker B must WIDEN the far bound: clean {:.1} mm, blurred {:.1} mm",
        clean.bound_far_end_mm(),
        blurred.bound_far_end_mm()
    );
}

#[test]
fn pan_without_marker_a_reports_missing_anchor() {
    let scene = Scene {
        markers: vec![marker(1, B_X_MM, 0.0)],
        dots: dots(),
    };
    let mut core = record_pan(&scene, 30, 2.5, 5);
    match core.finish(1.0, true) {
        Err(PanError::AnchorMarkerNotFound) => {}
        Err(other) => panic!("expected AnchorMarkerNotFound, got {other}"),
        Ok(_) => panic!("pan with no Marker A must not produce a result"),
    }
}

#[test]
fn print_scale_correction_scales_the_pan_metric() {
    // Markers printed at 94%: physically 141 mm. The dots are a true 300 mm
    // apart regardless. With the correction factor the measurement is right.
    let scene = Scene {
        markers: vec![
            SyntheticMarker { id: 0, x_mm: 0.0, y_mm: 0.0, side_mm: 150.0 * 0.94, rot_quarter: 0 },
            SyntheticMarker { id: 1, x_mm: B_X_MM, y_mm: 40.0, side_mm: 150.0 * 0.94, rot_quarter: 0 },
        ],
        dots: dots(),
    };
    let mut core = record_pan(&scene, 60, 2.5, 31);
    let out = core.finish(0.94, true).expect("pan must process");
    let measured = measure_mm(&out, [250.0, 250.0], [250.0, 550.0], 12.0);
    let err = (measured - 300.0).abs();
    let allowed = out.bound.bound_at_mm(250.0) * 2.0;
    assert!(
        err <= allowed,
        "print-scale-corrected measurement {measured:.1} mm off by {err:.1} > {allowed:.1}"
    );
}
