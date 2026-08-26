//! Live capture coaching (issue #5): deterministic synthetic-fixture tests
//! for the per-frame checks that run alongside keyframe selection. Marker
//! and rotation scenarios render real scenes through the synthetic camera;
//! speed/exposure scenarios use cheap procedurally-dotted frames (no marker
//! needed for those checks, and rendering stays fast).

use geometry_core::pan::{
    CoachCue, CoachStatus, PanCore, COACH_BLOWN_OUT, COACH_MARKER_LOST,
    COACH_MARKER_STALE_FRAMES, COACH_NO_MARKER, COACH_ROTATION, COACH_TOO_DARK, COACH_TOO_FAST,
};
use geometry_core::synthetic::{
    pose_homography, render_scene, PanCamera, Scene, SyntheticMarker,
};

const W: usize = 480;
const H: usize = 270;
const FOCAL: f64 = 525.0;
const DIST: f64 = 1400.0;

fn marker(id: u16, x_mm: f64, y_mm: f64) -> SyntheticMarker {
    SyntheticMarker { id, x_mm, y_mm, side_mm: 150.0, rot_quarter: 0 }
}

/// Tracking texture spread along the wall (stands in for power points,
/// skirting shadows, paint blemishes — walls are low-texture otherwise).
fn spread_dots(from_mm: f64, to_mm: f64) -> Vec<([f64; 2], f64)> {
    let mut d = Vec::new();
    let mut x = from_mm;
    let mut k = 0u32;
    while x < to_mm {
        let y0 = [120.0, 460.0, 300.0][k as usize % 3];
        d.push(([x, y0], 7.0));
        d.push(([x + 80.0, 600.0 - y0], 6.0));
        d.push(([x + 40.0, y0 + 150.0], 5.0));
        k += 1;
        x += 170.0 + 15.0 * ((k % 4) as f64);
    }
    d
}

/// Cheap procedurally-dotted frame: world-anchored dark dots on a light wall
/// seen through a translation-only camera at `offset_px`. Microseconds per
/// frame — used by the checks that need no real marker or perspective.
fn dotted_frame(w: usize, h: usize, offset_px: f64, bg: u8, dot: u8) -> Vec<u8> {
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let wx = x as f64 + offset_px;
            // Dot lattice: centres every 60 px in x, three staggered rows.
            let col = (wx / 60.0).floor();
            let cx = col * 60.0 + 30.0 + 13.0 * ((col as i64 % 3) as f64);
            let row_phase = ((col as i64).rem_euclid(3)) as f64;
            let mut v = bg;
            for (ri, ry) in [40.0, 130.0, 220.0].iter().enumerate() {
                let cy = ry + 12.0 * ((ri as f64 + row_phase) % 3.0);
                let dx = wx - cx;
                let dy = y as f64 - cy;
                if dx * dx + dy * dy < 49.0 {
                    v = dot;
                }
            }
            let o = (y * w + x) * 4;
            rgba[o] = v;
            rgba[o + 1] = v;
            rgba[o + 2] = v;
            rgba[o + 3] = 255;
        }
    }
    rgba
}

fn flags_of(core: &PanCore) -> u8 {
    core.coach_status().flags
}

// ---------------------------------------------------------------------------
// Cue priority (unit-level: pure function of the status flags).

#[test]
fn cue_priority_is_marker_then_speed_then_exposure_then_rotation() {
    let s = |flags: u8| CoachStatus { flags, ..Default::default() };
    assert_eq!(s(0).cue(), CoachCue::AllGood);
    let all = COACH_NO_MARKER
        | COACH_MARKER_LOST
        | COACH_TOO_FAST
        | COACH_TOO_DARK
        | COACH_BLOWN_OUT
        | COACH_ROTATION;
    assert_eq!(s(all).cue(), CoachCue::FindMarkerA);
    assert_eq!(s(all & !COACH_NO_MARKER).cue(), CoachCue::MarkerLost);
    assert_eq!(
        s(COACH_TOO_FAST | COACH_TOO_DARK | COACH_BLOWN_OUT | COACH_ROTATION).cue(),
        CoachCue::TooFast
    );
    assert_eq!(s(COACH_TOO_DARK | COACH_BLOWN_OUT | COACH_ROTATION).cue(), CoachCue::TooDark);
    assert_eq!(s(COACH_BLOWN_OUT | COACH_ROTATION).cue(), CoachCue::BlownOut);
    assert_eq!(s(COACH_ROTATION).cue(), CoachCue::RotationDominant);
}

// ---------------------------------------------------------------------------
// Marker visibility.

#[test]
fn clean_slow_pan_reports_all_good_and_sees_both_markers() {
    // A at the left end, B at the right, slow wall-parallel pan covering
    // both: no check may trip, and both sightings must stick.
    let scene = Scene {
        markers: vec![marker(0, 0.0, 0.0), marker(1, 1750.0, 0.0)],
        dots: spread_dots(250.0, 1700.0),
    };
    let cam = PanCamera {
        focal_px: FOCAL,
        width: W,
        height: H,
        distance_mm: DIST,
        start_center_mm: [300.0, 280.0],
        end_center_mm: [1500.0, 280.0],
        yaw_amp: 0.02,
        pitch_amp: 0.012,
    };
    let mut core = PanCore::new(W, H);
    let frames = 80;
    let mut warned_frames = 0u32;
    for (i, h) in cam.sequence(frames).iter().enumerate() {
        let rgba = render_scene(&scene, h, W, H, 2.5, 100 + i as u64);
        core.push_frame(&rgba);
        if flags_of(&core) != 0 {
            warned_frames += 1;
        }
    }
    let status = core.coach_status();
    assert_eq!(
        status.flags, 0,
        "clean pan must end with no coaching flags (got {:#b}, cue {:?})",
        status.flags,
        status.cue()
    );
    assert_eq!(status.cue(), CoachCue::AllGood);
    assert!(status.marker_a_seen, "Marker A must be sighted at the start");
    assert!(status.marker_b_seen, "Marker B must be sighted at the end");
    assert!(
        status.blur_fraction < 0.25,
        "clean slow pan must not read as blurred: {}",
        status.blur_fraction
    );
    // No false positives during the pan either (first frame has no marker
    // observation yet, so allow the very first detection interval).
    assert!(
        warned_frames <= 1,
        "clean pan flagged {warned_frames} frames"
    );
}

#[test]
fn recording_with_no_marker_in_view_trips_find_marker_a() {
    let mut core = PanCore::new(W, H);
    for i in 0..30 {
        core.push_frame(&dotted_frame(W, H, i as f64 * 3.0, 205, 30));
    }
    let status = core.coach_status();
    assert!(
        status.flags & COACH_NO_MARKER != 0,
        "bare wall must trip COACH_NO_MARKER (flags {:#b})",
        status.flags
    );
    assert_eq!(status.cue(), CoachCue::FindMarkerA);
    assert!(!status.marker_a_seen && !status.marker_b_seen);
}

#[test]
fn marker_sighting_going_stale_mid_pan_trips_marker_lost() {
    // Marker A in view at the start (real rendered frames), then a long
    // stretch of bare wall: the sticky sighting stays, the staleness trips.
    let scene = Scene {
        markers: vec![marker(0, 0.0, 0.0)],
        dots: spread_dots(250.0, 900.0),
    };
    let h0 = pose_homography(FOCAL, W, H, [300.0, 280.0, -DIST], 0.0, 0.0);
    let mut core = PanCore::new(W, H);
    for i in 0..6 {
        let rgba = render_scene(&scene, &h0, W, H, 2.5, 500 + i as u64);
        core.push_frame(&rgba);
    }
    assert!(core.coach_status().marker_a_seen, "A must be seen at the start");
    assert_eq!(core.coach_status().flags & COACH_MARKER_LOST, 0);
    for i in 0..(COACH_MARKER_STALE_FRAMES + 10) {
        core.push_frame(&dotted_frame(W, H, i as f64 * 4.0, 205, 30));
    }
    let status = core.coach_status();
    assert!(
        status.flags & COACH_MARKER_LOST != 0,
        "stale sighting must trip COACH_MARKER_LOST (flags {:#b})",
        status.flags
    );
    assert_eq!(status.cue(), CoachCue::MarkerLost);
    assert!(status.marker_a_seen, "sightings are sticky");
    assert!(!status.marker_b_seen, "B was never seen — the page must demand a retake");
}

// ---------------------------------------------------------------------------
// Motion speed.

#[test]
fn fast_pan_trips_slow_down_and_clears_after_slowing() {
    let mut core = PanCore::new(W, H);
    let mut offset = 0.0;
    // ~22 px/frame at 480 wide is well over the HI threshold (0.028*480 ≈ 13.4).
    for _ in 0..20 {
        core.push_frame(&dotted_frame(W, H, offset, 205, 30));
        offset += 22.0;
    }
    let fast = core.coach_status();
    assert!(
        fast.flags & COACH_TOO_FAST != 0,
        "22 px/frame must trip COACH_TOO_FAST (flags {:#b}, speed {:.1})",
        fast.flags,
        fast.speed_px_per_frame
    );
    // (The cue itself is FindMarkerA here — these cheap frames carry no
    // marker, and marker problems outrank speed by design.)
    assert!(
        fast.blur_fraction > 0.5,
        "a pan that fast is mostly blurred: {}",
        fast.blur_fraction
    );
    // Slow back down: the cue must clear within a few frames (hysteresis).
    for _ in 0..12 {
        core.push_frame(&dotted_frame(W, H, offset, 205, 30));
        offset += 4.0;
    }
    let slow = core.coach_status();
    assert_eq!(
        slow.flags & COACH_TOO_FAST,
        0,
        "slowing down must clear COACH_TOO_FAST (speed {:.1})",
        slow.speed_px_per_frame
    );
}

// ---------------------------------------------------------------------------
// Exposure.

#[test]
fn dark_scene_trips_too_dark_and_recovers_when_lit() {
    let mut core = PanCore::new(W, H);
    let mut offset = 0.0;
    for _ in 0..10 {
        core.push_frame(&dotted_frame(W, H, offset, 25, 6));
        offset += 4.0;
    }
    let dark = core.coach_status();
    assert!(
        dark.flags & COACH_TOO_DARK != 0,
        "a ~25-luma frame must trip COACH_TOO_DARK (mean {:.0}, flags {:#b})",
        dark.mean_luma,
        dark.flags
    );
    // Light turned on: clears after the debounce window.
    for _ in 0..8 {
        core.push_frame(&dotted_frame(W, H, offset, 205, 30));
        offset += 4.0;
    }
    assert_eq!(core.coach_status().flags & COACH_TOO_DARK, 0);
}

#[test]
fn blown_out_scene_trips_blown_out() {
    let mut core = PanCore::new(W, H);
    let mut offset = 0.0;
    for _ in 0..10 {
        core.push_frame(&dotted_frame(W, H, offset, 250, 170));
        offset += 4.0;
    }
    let status = core.coach_status();
    assert!(
        status.flags & COACH_BLOWN_OUT != 0,
        "a ~250-luma frame must trip COACH_BLOWN_OUT (mean {:.0}, flags {:#b})",
        status.mean_luma,
        status.flags
    );
}

// ---------------------------------------------------------------------------
// Rotation-dominant motion (issue #19: coached away).

#[test]
fn rotate_in_place_sweep_trips_rotation_cue() {
    // Camera standing still in front of the wall, sweeping yaw 0 -> 35
    // degrees — the very phone habit the closure guard refuses post-capture.
    // Apparent motion stays under the speed threshold (~4-7 px/frame), so
    // the ROTATION check — not TOO_FAST — must be the one that fires.
    // Texture must cover everything the swivel sweeps across (the oblique
    // view at 35 degrees reaches well past wall x = -1000): the probes need
    // something to track, exactly as documented for the heuristic.
    let scene = Scene {
        markers: vec![marker(0, 0.0, 0.0)],
        dots: spread_dots(-1800.0, 2600.0),
    };
    let mut core = PanCore::new(W, H);
    let frames = 80;
    for i in 0..frames {
        let t = i as f64 / (frames - 1) as f64;
        let yaw = t * 35.0f64.to_radians();
        let h = pose_homography(FOCAL, W, H, [600.0, 280.0, -DIST], yaw, 0.0);
        let rgba = render_scene(&scene, &h, W, H, 2.5, 900 + i as u64);
        core.push_frame(&rgba);
    }
    let status = core.coach_status();
    assert!(
        status.flags & COACH_ROTATION != 0,
        "rotate-in-place must trip COACH_ROTATION (flags {:#b}, drift {:.3})",
        status.flags,
        status.rotation_drift
    );
    assert_eq!(
        status.flags & COACH_TOO_FAST,
        0,
        "the swivel is slow — TOO_FAST must not be the verdict (speed {:.1})",
        status.speed_px_per_frame
    );
    // A was seen at the start and the sweep is shorter than the staleness
    // window, so rotation is the highest-priority active cue.
    assert_eq!(status.cue(), CoachCue::RotationDominant);
}

