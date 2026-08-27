//! Recorded-pan processing (issue #4): keyframe selection during capture,
//! inter-keyframe feature tracking, homography chaining anchored at Reference
//! Marker A, loop closure against Marker B, drift redistribution, full-wall
//! stitching, and the session's first Error Bound.
//!
//! # Pipeline
//!
//! **During capture** ([`PanCore::push_frame`]): each incoming RGBA frame is
//! reduced to a luma plane, a 1/8-scale thumbnail and a sharpness score.
//! Frame-to-frame translation is tracked incrementally by small-window NCC
//! on the thumbnails (a small window cannot alias on periodic wall texture
//! and consecutive frames move little); the accumulated shift estimates
//! overlap with the last kept keyframe. Frames are buffered as the
//! *sharpest candidate* while overlap sits in the candidate band, and the
//! candidate is committed as a keyframe when overlap drops below the keep
//! threshold. Only kept keyframes (capped at [`MAX_KEYFRAMES`]) retain full
//! pixels — everything else is discarded immediately, which is what keeps a
//! multi-second pan within tens of MB instead of gigabytes. A long
//! untrackable streak marks the pan's continuity as broken; if keyframes
//! later commit beyond the gap, processing refuses with
//! [`PanError::TrackingLost`] instead of silently chaining across it.
//!
//! Alongside selection, cheap live-coaching checks (issue #5) update a
//! [`CoachStatus`] per frame — marker visibility (full-resolution detection
//! every [`COACH_DETECT_EVERY`]th frame), motion speed and exposure — so the
//! capture page can coach the Homeowner in real time and demand a retake
//! when a recording ends without both markers ever seen, or when the core
//! already knows the chain broke ([`PanCore::tracking_broken`]).
//!
//! **After capture** ([`PanCore::finish`]):
//! 1. Reference Markers are detected in every keyframe.
//! 2. The wall frame is anchored at the best (largest) Marker A detection,
//!    same convention as [`crate::rectify`]: A's printed top-left corner is
//!    wall (0,0), axes in mm.
//! 3. Harris-style corners are extracted per keyframe and matched to the
//!    next keyframe by NCC patch search (seeded by the capture-time shift);
//!    matches — plus shared marker corners — go through
//!    [`crate::homography::estimate`] (RANSAC + LM) to give per-link
//!    homographies, which are chained into wall->keyframe maps. Links with
//!    too few, or too tightly clustered, inliers break the chain loudly
//!    ([`PanError::WeakSegment`]).
//! 3b. Self-calibration (issue #6, [`crate::calib`]): focal length and one
//!    division-model radial-distortion coefficient (k1) are refined jointly
//!    with the keyframe poses by LM over the same correspondences (marker
//!    corners + tracked features). When the result passes the conditioning
//!    gates, every measured point is undistorted and the anchor estimate,
//!    links and chain are re-run on the corrected points — frame-edge
//!    geometry stops bending, residuals drop, and with them the measured
//!    terms of the Error Bound below (the bound tightens because its
//!    inputs genuinely improved, not because a constant changed). When the
//!    chain cannot support calibration (short, barely-rotating,
//!    near-fronto-parallel — see the gates in [`crate::calib`]), the
//!    pipeline honestly stays pinhole (k1 = 0) and keeps the wider
//!    uncalibrated bound.
//! 4. Loop closure: Marker B's corners in its best keyframe are
//!    back-projected through the chain onto the wall plane. Because B's wall
//!    *pose* is unknown but its printed *size* is known (ADR-0002), the
//!    discrepancy between the back-projected quad and the best-fitting rigid
//!    square of the true side is the measured chain drift; its similarity
//!    fit isolates the dominant observable, the local scale error at B.
//! 5. Drift redistribution: composed-homography drift accumulates link by
//!    link, so the correction is a *progressive local-scale field* — local
//!    scale `s^t` at chain fraction t (log-space interpolation), with
//!    keyframe positions moved by the integral of that field along the
//!    chain. (Applying the full similarity to absolute wall coordinates
//!    would overshoot far-end positions by ~2x, because the drift was not
//!    present from the anchor onward.) The residual mismatch at B after
//!    correction is re-measured and feeds the Error Bound.
//! 6. The full-wall Rectified Wall Image is stitched by inverse warping,
//!    pick-best-source per pixel: the covering keyframe whose view centre
//!    is nearest on the wall (Voronoi partition). Nearest-centre minimises
//!    extrapolation of that keyframe\'s estimated homography — the metric
//!    error lever — and is also the least oblique view; capture-time
//!    selection already kept the sharp frames. Seams are unblended (v1).
//!
//! # Error Bound model (see CONTEXT.md: per-session 95% bound)
//!
//! At wall position x (mm from Marker A\'s origin), the 1-sigma error is
//! modelled as independent terms added in quadrature:
//!
//! * **anchor** — marker A corner residual (floored at a sub-pixel
//!   constant, in mm at A\'s scale) times a vertical-extrapolation factor:
//!   a 4-corner fit of a small marker has perspective noise growing
//!   ~quadratically with distance in marker-side units, and closure
//!   constrains nothing vertically. Constant in x (along the pan the
//!   closure terms below take over).
//! * **track(x)** — a random walk of per-link tracking inlier RMS
//!   (converted to mm at each keyframe\'s scale), accumulated along the
//!   chain up to x; full per-link RMS, not RMS/sqrt(n): conservative.
//! * **x · rel** — relative scale uncertainty of the closure itself: B\'s
//!   corner noise over the marker\'s 150 mm baseline
//!   (`Var(scale) = sigma^2 / (2 side^2)` for a square\'s corners). Without
//!   closure there is no measurement to bound scale drift, so a
//!   conservative documented prior [`OPEN_LOOP_REL`] is used and the
//!   result is flagged open-loop.
//! * **ramp(x)** — the *measured* residual drift left at B after
//!   redistribution, ramped linearly over the anchor->B span.
//!
//! The 95% bound is `K95` times the quadrature sum. **The public contract
//! is the DISTANCE bound** [`BoundModel::bound_between_mm`] — a 95% bound
//! on distances measured between two wall positions, which is what the
//! measure tool and Clear Zone dimensions consume. Per-position values
//! ([`BoundModel::bound_at_mm`]) are components with a documented
//! systematic-bias caveat; the far end of a wall is honestly less certain
//! than the metre around Marker A.

use crate::calib::{self, Distortion, SelfCalibration};
use crate::detect::{detect_markers, DetectedMarker};
use crate::homography::{estimate, Homography};
use crate::linalg::{mat3_inv, mat3_mul};
use crate::marker::{LEFT_MARKER_ID, MARKER_SIDE_MM, RIGHT_MARKER_ID};
use crate::rectify::{anchor_corners_mm, fit_rigid_square};

/// Hard cap on kept keyframes: bounds peak memory (see module docs).
pub const MAX_KEYFRAMES: usize = 30;

/// Thumbnail downscale factor for the capture-time overlap estimate.
const THUMB_DIV: usize = 8;

/// Commit the pending candidate when overlap with the last keyframe falls
/// below this fraction.
const KEEP_OVERLAP: f64 = 0.60;

/// Start collecting sharpest-candidate frames below this overlap.
const CANDIDATE_OVERLAP: f64 = 0.85;

/// Minimum NCC peak for the thumbnail shift to be trusted.
const MIN_SHIFT_SCORE: f64 = 0.35;

/// Consecutive untrackable frames after which the pan's continuity is
/// considered broken (if keyframes are later committed beyond the gap).
const LOST_TRACK_FRAMES: usize = 10;

/// RANSAC inlier threshold for link estimation (px).
const LINK_INLIER_THRESHOLD_PX: f64 = 2.5;

/// A link needs at least this many RANSAC inliers to be trusted.
const MIN_LINK_INLIERS: usize = 10;

/// The inliers of a link must span at least this fraction of the frame in x
/// or y. A homography fit to one tight cluster (a single fixture on an
/// otherwise blank wall) is numerically "consistent" yet extrapolates
/// garbage across the rest of the frame — that is a weak segment, and it
/// must fail loudly rather than silently corrupt the chain.
const MIN_LINK_SPAN_FRAC: f64 = 0.25;

/// Minimum NCC peak for a full-resolution patch match.
const MIN_MATCH_NCC: f64 = 0.55;

/// Sub-pixel corner noise floor (px) used wherever a residual of an exact
/// fit would otherwise pretend to be zero. The detector's synthetic tests
/// measure ~0.15-0.3 px mean corner error on marker-sized quads (see
/// tests/still_rectification.rs sub-pixel assertions); 0.30 px stays at the
/// conservative edge of that.
const CORNER_SIGMA_FLOOR_PX: f64 = 0.30;

/// 95% factor applied to the 1-sigma quadrature sum.
const K95: f64 = 2.0;

/// Relative scale-drift prior when no loop closure is available (2%):
/// ADR-0002's motivating error magnitude for unanchored chaining. A prior,
/// not a measurement — the result is flagged so the UI can say so.
const OPEN_LOOP_REL: f64 = 0.02;

/// Closure plausibility: a measured drift beyond this is not credible chain
/// drift — it means a bad Marker B detection, a rotate-in-place "pan", or a
/// marker off the wall plane. Refuse the closure (fall back to the honest
/// open-loop bound + a flag) rather than redistribute garbage. Shared with
/// the CI tests so the production guard and the test's notion of
/// "implausible" can never drift apart.
pub const MAX_CLOSURE_DISCREPANCY_MM: f64 = 50.0;

/// Closure plausibility: print scale is already corrected upstream
/// (ADR-0002 self-verification), so a closure demanding more than a 3%
/// scale rewrite is implausible for the same reasons as above.
pub const MAX_CLOSURE_SCALE_DEV: f64 = 0.03;

// ---------------------------------------------------------------------------
// Live capture coaching (issue #5): cheap per-frame checks that run alongside
// keyframe selection so the UI can coach the Homeowner WHILE recording. The
// speed and exposure checks reuse what push_frame already computes (thumbnail
// shift -> motion speed, thumbnail luma -> exposure); marker visibility is
// the one check that pays for its own work — FULL-RESOLUTION detection every
// [`COACH_DETECT_EVERY`] frames — because its verdict feeds the retake gate
// and must therefore match what [`PanCore::finish`]'s own detection pass can
// see (a downscaled plane raises the detection floor and made the coach
// demand retakes of perfectly processable captures). Coaching is purely
// observational: it never influences keyframe selection or processing.
//
// A rotation-dominant-motion cue (issue #19's stand-and-swivel habit) was
// removed here: its flow-curvature heuristic could not separate camera
// rotation from translation along a wall viewed at an angle — the t_z·sin(yaw)
// component of angled-plane flow is non-affine with the same edges-faster-
// than-centre quadratic signature — so it false-fired persistently on correct
// technique. The post-capture closure plausibility guard
// ([`MAX_CLOSURE_DISCREPANCY_MM`] / [`MAX_CLOSURE_SCALE_DEV`]) remains the
// backstop; issue #19 owns any live replacement.

/// Run FULL-RESOLUTION marker detection every Nth pushed frame. Full-res
/// because the coach's detection floor must equal the one in
/// [`PanCore::finish`]: on a real phone FOV a 2×2-downscaled plane lost a
/// plainly visible marker beyond ~2.3–2.5 m and the retake gate then refused
/// captures processing could handle. Per-frame full-res detection is not
/// frame-budget cheap, so the cadence is slow — the seen-flags are sticky,
/// and frame 0 is always probed, so the start of a recording is covered
/// immediately; at 30 fps this bounds "marker (re)acquired" latency to
/// ~330 ms.
const COACH_DETECT_EVERY: u64 = 10;

/// Frames since the last marker sighting before "marker lost" trips. Mid-pan
/// on a long wall legitimately sees neither marker for a while — a slow,
/// correct pan of a 3-4 m wall spends 5-7 s between losing A and finding B —
/// so this is generous (~8 s at 30 fps), not a reaction-time constant.
pub const COACH_MARKER_STALE_FRAMES: u64 = 240;

/// Motion-speed hysteresis (fractions of frame width per frame) on an EMA of
/// the tracked thumbnail shift. 0.028 * 640 px ~= 18 px/frame: at a typical
/// 1/60 s exposure that smears ~9 px of motion blur across the frame — past
/// what keyframe tracking tolerates. A full-width pan at the HI threshold
/// takes ~1.2 s; the coached pace is several times slower.
const COACH_SPEED_HI_FRAC: f64 = 0.028;
const COACH_SPEED_LO_FRAC: f64 = 0.018;

/// EMA smoothing for the speed check: reacts in ~3 frames (~100 ms).
const COACH_SPEED_ALPHA: f64 = 0.4;

/// Exposure thresholds on the thumbnail luma histogram: "too dark" when most
/// of the frame sits near black, "blown out" when most sits at saturation.
const COACH_DARK_LUMA: f32 = 40.0;
const COACH_BRIGHT_LUMA: f32 = 240.0;
const COACH_DARK_FRAC: f64 = 0.70;
const COACH_BRIGHT_FRAC: f64 = 0.50;

/// Consecutive frames an exposure condition must hold to trip / to clear
/// (~130 ms at 30 fps): a single auto-exposure hiccup must not flash a cue.
const COACH_EXPOSURE_DEBOUNCE: u32 = 4;

/// Real-motion floor (thumb px per frame) for the blur bookkeeping: tracked
/// frames moving less than this are the Homeowner standing still — hand
/// jitter, not pan motion — and belong on NEITHER side of the "share of
/// motion frames blurred" ratio ([`CoachStatus::blur_fraction`]). Counting
/// them in the denominator diluted the statistic: a fast pan followed by a
/// long standstill read as mostly fine. 0.2 thumb px ≈ 1.6 full-res px per
/// frame — well under any deliberate pan speed.
const COACH_BLUR_MIN_MOTION_THUMB_PX: f64 = 0.2;

/// Coaching flags (bitmask in [`CoachStatus::flags`]). A flag is set while
/// its (debounced) check is tripping.
pub const COACH_NO_MARKER: u8 = 1 << 0;
pub const COACH_MARKER_LOST: u8 = 1 << 1;
pub const COACH_TOO_FAST: u8 = 1 << 2;
pub const COACH_TOO_DARK: u8 = 1 << 3;
pub const COACH_BLOWN_OUT: u8 = 1 << 4;

/// The single highest-priority coaching cue for the UI's one-message line.
/// Priority: marker problems > too fast > exposure — a lost marker
/// invalidates the session outright, speed ruins the frames the markers
/// would be found in, and exposure ruins everything equally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoachCue {
    AllGood = 0,
    /// No Reference Marker has been seen yet: start with Marker A in view.
    FindMarkerA = 1,
    /// A marker was seen earlier but none for [`COACH_MARKER_STALE_FRAMES`].
    MarkerLost = 2,
    TooFast = 3,
    TooDark = 4,
    BlownOut = 5,
}

/// Compact per-frame coaching status. Read it after each
/// [`PanCore::push_frame`]; all fields are cheap copies of already-computed
/// state.
#[derive(Clone, Copy, Debug, Default)]
pub struct CoachStatus {
    /// Bitmask of the `COACH_*` flags currently tripping (debounced).
    pub flags: u8,
    /// Marker A / B sighted at least once this recording (sticky). Both must
    /// be true by the time recording stops, or the capture needs a retake.
    pub marker_a_seen: bool,
    pub marker_b_seen: bool,
    /// Smoothed tracked motion (full-resolution px per frame).
    pub speed_px_per_frame: f64,
    /// Mean thumbnail luma of the last frame (0-255).
    pub mean_luma: f64,
    /// Fraction of MOTION frames that were untrackable or over the speed
    /// limit — the "mostly blurred" retake signal (a proxy: untrackable
    /// frames also include featureless wall, which reads as the same
    /// Homeowner problem). Tracked frames below the real-motion floor
    /// ([`COACH_BLUR_MIN_MOTION_THUMB_PX`]: standing still) count on
    /// neither side of the ratio.
    pub blur_fraction: f64,
}

impl CoachStatus {
    /// Highest-priority active cue (see [`CoachCue`] for the ordering).
    pub fn cue(&self) -> CoachCue {
        if self.flags & COACH_NO_MARKER != 0 {
            CoachCue::FindMarkerA
        } else if self.flags & COACH_MARKER_LOST != 0 {
            CoachCue::MarkerLost
        } else if self.flags & COACH_TOO_FAST != 0 {
            CoachCue::TooFast
        } else if self.flags & COACH_TOO_DARK != 0 {
            CoachCue::TooDark
        } else if self.flags & COACH_BLOWN_OUT != 0 {
            CoachCue::BlownOut
        } else {
            CoachCue::AllGood
        }
    }
}

/// What the thumbnail tracker saw for one frame, for coaching purposes.
enum CoachMotion {
    /// The very first frame: nothing to move relative to.
    FirstFrame,
    /// Frame-to-frame thumbnail shift (thumb px).
    Tracked([f64; 2]),
    /// The tracker lost this frame (motion blur / featureless wall).
    Untracked,
    /// A byte-identical repeat of the previous frame (the camera pipeline
    /// delivered no new frame before the processing loop came round again).
    /// Its zero shift says nothing about the pan's real speed — observing
    /// it would yank the speed EMA down and make the TOO_FAST cue flicker
    /// at exactly the rates where processing outpaces the camera.
    Duplicate,
}

/// Internal live-coaching state, updated once per pushed frame.
struct CoachState {
    /// Frames observed so far (== push_frame calls).
    frame_idx: u64,
    seen_a: bool,
    seen_b: bool,
    last_marker_frame: Option<u64>,
    speed_ema_px: f64,
    too_fast: bool,
    dark_streak: u32,
    dark_clear_streak: u32,
    too_dark: bool,
    bright_streak: u32,
    bright_clear_streak: u32,
    blown_out: bool,
    mean_luma: f64,
    /// Motion frames that were untrackable or over the speed limit.
    fast_frames: u64,
    /// Frames with a REAL motion observation: tracked above the real-motion
    /// floor, or untrackable (blur / featureless — unmeasurable, but not a
    /// standstill: a still camera on a wall tracks near-zero shift fine).
    motion_frames: u64,
}

impl CoachState {
    fn new() -> CoachState {
        CoachState {
            frame_idx: 0,
            seen_a: false,
            seen_b: false,
            last_marker_frame: None,
            speed_ema_px: 0.0,
            too_fast: false,
            dark_streak: 0,
            dark_clear_streak: 0,
            too_dark: false,
            bright_streak: 0,
            bright_clear_streak: 0,
            blown_out: false,
            mean_luma: 0.0,
            fast_frames: 0,
            motion_frames: 0,
        }
    }

    /// One coaching observation per pushed frame: exposure from the
    /// (already computed) thumbnail, motion speed from the (already
    /// computed) tracked thumbnail shift, and full-resolution marker
    /// detection every [`COACH_DETECT_EVERY`]th frame.
    #[allow(clippy::too_many_arguments)]
    fn observe(
        &mut self,
        gray: &[u8],
        thumb: &[f32],
        width: usize,
        height: usize,
        thumb_w: usize,
        thumb_h: usize,
        motion: CoachMotion,
    ) {
        let idx = self.frame_idx;
        self.frame_idx += 1;

        // Exposure: luma histogram of the (already computed) thumbnail.
        let n = (thumb_w * thumb_h).max(1) as f64;
        let (mut sum, mut dark, mut bright) = (0.0f64, 0u32, 0u32);
        for &v in thumb {
            sum += v as f64;
            if v < COACH_DARK_LUMA {
                dark += 1;
            } else if v > COACH_BRIGHT_LUMA {
                bright += 1;
            }
        }
        self.mean_luma = sum / n;
        let streak = |cond: bool, on: &mut u32, off: &mut u32, state: &mut bool| {
            if cond {
                *on += 1;
                *off = 0;
                if *on >= COACH_EXPOSURE_DEBOUNCE {
                    *state = true;
                }
            } else {
                *off += 1;
                *on = 0;
                if *off >= COACH_EXPOSURE_DEBOUNCE {
                    *state = false;
                }
            }
        };
        streak(
            dark as f64 / n > COACH_DARK_FRAC,
            &mut self.dark_streak,
            &mut self.dark_clear_streak,
            &mut self.too_dark,
        );
        streak(
            bright as f64 / n > COACH_BRIGHT_FRAC,
            &mut self.bright_streak,
            &mut self.bright_clear_streak,
            &mut self.blown_out,
        );

        // Marker visibility: FULL-RESOLUTION detection every Nth frame —
        // full-res so the coach's detection floor equals finish()'s own at
        // the same scale, and the retake gate never refuses a capture whose
        // markers processing would find (see COACH_DETECT_EVERY). The
        // detector needs >= 32 px planes and a marker >= 14 px.
        if idx % COACH_DETECT_EVERY == 0 && width >= 32 && height >= 32 {
            for m in detect_markers(gray, width, height) {
                match m.id {
                    LEFT_MARKER_ID => self.seen_a = true,
                    RIGHT_MARKER_ID => self.seen_b = true,
                    _ => {}
                }
                self.last_marker_frame = Some(idx);
            }
        }

        // Motion speed + blur bookkeeping (every frame after the first).
        let hi = COACH_SPEED_HI_FRAC * width as f64;
        let lo = COACH_SPEED_LO_FRAC * width as f64;
        match motion {
            CoachMotion::FirstFrame | CoachMotion::Duplicate => {}
            CoachMotion::Tracked(shift) => {
                let thumb_px = shift[0].hypot(shift[1]);
                let px = thumb_px * THUMB_DIV as f64;
                self.speed_ema_px =
                    COACH_SPEED_ALPHA * px + (1.0 - COACH_SPEED_ALPHA) * self.speed_ema_px;
                if self.speed_ema_px > hi {
                    self.too_fast = true;
                } else if self.speed_ema_px < lo {
                    self.too_fast = false;
                }
                // blur_fraction is the share of MOTION frames blurred: a
                // standing-still frame (tracked jitter under the floor) is
                // not a motion frame and must not dilute the denominator.
                if thumb_px >= COACH_BLUR_MIN_MOTION_THUMB_PX {
                    self.motion_frames += 1;
                    if px > hi {
                        self.fast_frames += 1;
                    }
                }
            }
            // Untrackable: motion blur or a featureless stretch — either way
            // a frame the pipeline cannot use; count it for blur_fraction
            // but leave the (unmeasurable) speed state alone.
            CoachMotion::Untracked => {
                self.motion_frames += 1;
                self.fast_frames += 1;
            }
        }
    }

    fn status(&self) -> CoachStatus {
        let mut flags = 0u8;
        match self.last_marker_frame {
            None => {
                if self.frame_idx > 0 {
                    flags |= COACH_NO_MARKER;
                }
            }
            Some(f) => {
                if self.frame_idx.saturating_sub(f) > COACH_MARKER_STALE_FRAMES {
                    flags |= COACH_MARKER_LOST;
                }
            }
        }
        if self.too_fast {
            flags |= COACH_TOO_FAST;
        }
        if self.too_dark {
            flags |= COACH_TOO_DARK;
        }
        if self.blown_out {
            flags |= COACH_BLOWN_OUT;
        }
        CoachStatus {
            flags,
            marker_a_seen: self.seen_a,
            marker_b_seen: self.seen_b,
            speed_px_per_frame: self.speed_ema_px,
            mean_luma: self.mean_luma,
            blur_fraction: self.fast_frames as f64 / self.motion_frames.max(1) as f64,
        }
    }
}

/// Furthest the stitched output extends from the anchor (mm).
const MAX_PAN_EXTENT_MM: f64 = 8000.0;

/// Output size caps for the stitched image.
const MAX_PAN_OUT_W: usize = 4096;
const MAX_PAN_OUT_H: usize = 1440;

// ---------------------------------------------------------------------------
// Capture-time keyframe selection.

/// Frame disposition returned by [`PanCore::push_frame`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameDisposition {
    /// Committed as a keyframe (full pixels retained).
    Kept = 0,
    /// Discarded (overlap still high, or untrackable content).
    Skipped = 1,
    /// Buffered as the current sharpest candidate for the next keyframe.
    Candidate = 2,
    /// Keyframe cap reached; frame discarded and the pan flagged truncated.
    CapReached = 3,
}

struct Keyframe {
    rgba: Vec<u8>,
    gray: Vec<u8>,
    sharpness: f64,
    /// Accumulated full-resolution px shift from the previous keyframe,
    /// measured incrementally at capture time (seeds the feature matcher).
    shift_from_prev: [f64; 2],
}

/// Incremental pan recorder: feed every captured frame, it decides what to
/// keep. All heavy processing happens once in [`PanCore::finish`].
///
/// Overlap is estimated by accumulating small frame-to-frame thumbnail
/// shifts rather than one large-window correlation against the last
/// keyframe: walls carry periodic texture (bricks, tiles, evenly spaced
/// fixtures) that aliases a wide correlation search, while the true
/// inter-frame motion is always small.
pub struct PanCore {
    width: usize,
    height: usize,
    thumb_w: usize,
    thumb_h: usize,
    keyframes: Vec<Keyframe>,
    /// Pending sharpest candidate plus its accumulated thumb shift from the
    /// last kept keyframe.
    candidate: Option<(Keyframe, [f64; 2])>,
    /// Thumbnail of the previous successfully tracked frame.
    prev_thumb: Option<Vec<f32>>,
    /// Accumulated thumb-px shift: last kept keyframe -> previous frame.
    acc_shift: [f64; 2],
    /// Consecutive frames whose shift could not be tracked.
    lost_streak: usize,
    /// A lost streak exceeded [`LOST_TRACK_FRAMES`]; if a keyframe commits
    /// after this, the chain is broken at that gap.
    pending_gap: bool,
    /// Keyframe index after which continuity broke (sticky, first gap).
    broken_after: Option<usize>,
    truncated: bool,
    /// Live-coaching state (issue #5): observational only — never influences
    /// keyframe selection or processing.
    coach: CoachState,
}

impl PanCore {
    pub fn new(width: usize, height: usize) -> PanCore {
        PanCore {
            width,
            height,
            thumb_w: (width / THUMB_DIV).max(1),
            thumb_h: (height / THUMB_DIV).max(1),
            keyframes: Vec::new(),
            candidate: None,
            prev_thumb: None,
            acc_shift: [0.0, 0.0],
            lost_streak: 0,
            pending_gap: false,
            broken_after: None,
            truncated: false,
            coach: CoachState::new(),
        }
    }

    pub fn keyframe_count(&self) -> usize {
        self.keyframes.len()
    }

    /// Live-coaching status as of the last pushed frame (issue #5).
    pub fn coach_status(&self) -> CoachStatus {
        self.coach.status()
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// True when the core ALREADY KNOWS post-capture processing must fail
    /// with [`PanError::TrackingLost`]: continuity broke mid-recording and
    /// keyframes were committed beyond the gap. The capture page's retake
    /// gate consults this at stop time so the Homeowner is told immediately,
    /// instead of waiting through a processing run for [`PanCore::finish`]
    /// to fail with the same fact. (A trailing untrackable gap with nothing
    /// committed beyond it does not break processing — finish() drops the
    /// stale candidate and proceeds — so it is deliberately not reported
    /// here.) This is CERTAIN knowledge only: a [`PanError::WeakSegment`]
    /// — too few or too tightly clustered matches on one link — can still
    /// surface only at processing time, because link quality is not known
    /// until full-resolution matching runs.
    pub fn tracking_broken(&self) -> bool {
        self.broken_after.is_some()
    }

    /// Luma plane + thumbnail + sharpness only — the RGBA copy is deferred
    /// until a frame is actually retained, so skipped frames never allocate
    /// the big buffer.
    fn analyze(&self, rgba: &[u8]) -> (Vec<u8>, Vec<f32>, f64) {
        let mut gray = vec![0u8; self.width * self.height];
        crate::grayscale(rgba, &mut gray);
        let thumb = downscale(&gray, self.width, self.height, self.thumb_w, self.thumb_h);
        let sharpness = tenengrad(&gray, self.width, self.height);
        (gray, thumb, sharpness)
    }

    /// Consider one captured RGBA frame (`width * height * 4` bytes).
    pub fn push_frame(&mut self, rgba: &[u8]) -> FrameDisposition {
        debug_assert_eq!(rgba.len(), self.width * self.height * 4);
        let (gray, thumb, sharpness) = self.analyze(rgba);
        let retain = |gray: Vec<u8>, sharpness: f64, shift_from_prev: [f64; 2]| Keyframe {
            rgba: rgba.to_vec(),
            gray,
            sharpness,
            shift_from_prev,
        };
        if self.keyframes.is_empty() {
            self.coach.observe(
                &gray,
                &thumb,
                self.width,
                self.height,
                self.thumb_w,
                self.thumb_h,
                CoachMotion::FirstFrame,
            );
            self.keyframes.push(retain(gray, sharpness, [0.0, 0.0]));
            self.prev_thumb = Some(thumb);
            self.acc_shift = [0.0, 0.0];
            return FrameDisposition::Kept;
        }

        // Small-window incremental shift against the previous tracked frame.
        let prev = self.prev_thumb.as_ref().unwrap();
        let shift = ncc_shift_local(prev, &thumb, self.thumb_w, self.thumb_h);
        let tracked = shift.filter(|&(_, _, s)| s >= MIN_SHIFT_SCORE);
        self.coach.observe(
            &gray,
            &thumb,
            self.width,
            self.height,
            self.thumb_w,
            self.thumb_h,
            match tracked {
                // A perfect-correlation zero shift is a repeated camera
                // frame, not a measurement of a stationary pan.
                Some((dx, dy, s)) if dx == 0.0 && dy == 0.0 && s > 0.999 => {
                    CoachMotion::Duplicate
                }
                Some((dx, dy, _)) => CoachMotion::Tracked([dx, dy]),
                None => CoachMotion::Untracked,
            },
        );
        let Some((ddx, ddy, _score)) = tracked else {
            // Untrackable content (blank wall / motion blur): keeping it
            // would spend the cap on frames the tracker cannot use anyway.
            // A long streak marks the pan's continuity as broken — if
            // keyframes commit beyond the gap, finish() must fail loudly
            // instead of chaining across it with a stale shift estimate.
            self.lost_streak += 1;
            if self.lost_streak >= LOST_TRACK_FRAMES {
                self.pending_gap = true;
            }
            // Keep the reference thumbnail moving so tracking can
            // re-acquire once trackable wall reappears (the accumulated
            // shift stays frozen — it is stale, which pending_gap records).
            self.prev_thumb = Some(thumb);
            return FrameDisposition::Skipped;
        };
        self.lost_streak = 0;
        self.acc_shift[0] += ddx;
        self.acc_shift[1] += ddy;
        self.prev_thumb = Some(thumb);
        let acc = self.acc_shift;
        let overlap = 1.0
            - f64::max(
                (acc[0].abs() * THUMB_DIV as f64) / self.width as f64,
                (acc[1].abs() * THUMB_DIV as f64) / self.height as f64,
            );

        if overlap < KEEP_OVERLAP {
            if self.keyframes.len() >= MAX_KEYFRAMES {
                self.truncated = true;
                self.candidate = None;
                return FrameDisposition::CapReached;
            }
            if self.pending_gap && self.broken_after.is_none() {
                // First keyframe committed beyond an untrackable gap.
                self.broken_after = Some(self.keyframes.len() - 1);
            }
            match self.candidate.take() {
                // Commit the sharpest candidate seen in the band; the
                // current frame starts the next candidate window.
                Some((cand, cand_acc)) => {
                    self.keyframes.push(cand);
                    // Shift bookkeeping: acc was last-kept -> current; the
                    // candidate becomes the new last-kept.
                    self.acc_shift = [acc[0] - cand_acc[0], acc[1] - cand_acc[1]];
                    let new_acc = self.acc_shift;
                    self.candidate = Some((
                        retain(gray, sharpness, full_px(new_acc)),
                        new_acc,
                    ));
                    FrameDisposition::Kept
                }
                // Moved fast enough that no candidate exists: keep directly.
                None => {
                    self.keyframes.push(retain(gray, sharpness, full_px(acc)));
                    self.acc_shift = [0.0, 0.0];
                    FrameDisposition::Kept
                }
            }
        } else if overlap < CANDIDATE_OVERLAP {
            let sharper = self
                .candidate
                .as_ref()
                .is_none_or(|(c, _)| sharpness > c.sharpness);
            if sharper {
                self.candidate = Some((retain(gray, sharpness, full_px(acc)), acc));
            }
            FrameDisposition::Candidate
        } else {
            FrameDisposition::Skipped
        }
    }

    /// Run the full post-capture pipeline. `correction_factor` is the
    /// session print-scale factor (ADR-0002: MULTIPLY, never divide).
    /// `close_loop` exists so tests can prove the closure mechanism; the
    /// production path always passes `true`. Self-calibration (issue #6)
    /// is attempted; see [`PanCore::finish_with`] to force the pinhole
    /// baseline.
    pub fn finish(&mut self, correction_factor: f64, close_loop: bool) -> Result<PanOutput, PanError> {
        self.finish_with(correction_factor, close_loop, true)
    }

    /// [`PanCore::finish`] with the self-calibration stage switchable:
    /// `calibrate = false` forces the pinhole (k1 = 0) path. Exists so tests
    /// can compare the calibrated result against the uncalibrated baseline;
    /// production always calibrates (the conditioning gates inside decide
    /// whether the calibration is trustworthy).
    pub fn finish_with(
        &mut self,
        correction_factor: f64,
        close_loop: bool,
        calibrate: bool,
    ) -> Result<PanOutput, PanError> {
        if !(correction_factor.is_finite() && correction_factor > 0.0) {
            return Err(PanError::InvalidCorrectionFactor);
        }
        // A trailing candidate extends coverage (it overlaps the last
        // keyframe by less than CANDIDATE_OVERLAP): commit it. Two guards
        // (issue #4 review): a candidate whose accumulated shift spans an
        // untrackable gap has a stale seed and must be dropped, not allowed
        // to silently bridge the gap; and dropping for the keyframe cap
        // must set the truncation flag like any other cap event.
        if let Some((cand, _)) = self.candidate.take() {
            if self.pending_gap {
                // Stale across a gap: drop. Mid-recording commits already
                // convert gaps to broken_after in push_frame.
            } else if self.keyframes.len() < MAX_KEYFRAMES {
                self.keyframes.push(cand);
            } else {
                self.truncated = true;
            }
        }
        if self.keyframes.is_empty() {
            return Err(PanError::NoKeyframes);
        }
        if let Some(after) = self.broken_after {
            return Err(PanError::TrackingLost { after_keyframe: after });
        }
        process(
            &self.keyframes,
            self.width,
            self.height,
            correction_factor,
            close_loop,
            self.truncated,
            calibrate,
        )
    }
}

/// Box downscale of a luma plane to `tw` x `th` (f32 for NCC math).
fn downscale(gray: &[u8], w: usize, h: usize, tw: usize, th: usize) -> Vec<f32> {
    let mut out = vec![0f32; tw * th];
    let bx = w / tw;
    let by = h / th;
    for ty in 0..th {
        for tx in 0..tw {
            let mut acc = 0u32;
            for y in ty * by..ty * by + by {
                for x in tx * bx..tx * bx + bx {
                    acc += gray[y * w + x] as u32;
                }
            }
            out[ty * tw + tx] = acc as f32 / (bx * by) as f32;
        }
    }
    out
}

/// Tenengrad sharpness: mean squared central-difference gradient, sampled
/// on a stride-2 grid (cheap enough per frame at capture time).
fn tenengrad(gray: &[u8], w: usize, h: usize) -> f64 {
    if w < 4 || h < 4 {
        return 0.0;
    }
    let mut acc = 0.0f64;
    let mut n = 0u32;
    let mut y = 1;
    while y < h - 1 {
        let mut x = 1;
        while x < w - 1 {
            let gx = gray[y * w + x + 1] as f64 - gray[y * w + x - 1] as f64;
            let gy = gray[(y + 1) * w + x] as f64 - gray[(y - 1) * w + x] as f64;
            acc += gx * gx + gy * gy;
            n += 1;
            x += 2;
        }
        y += 2;
    }
    acc / n.max(1) as f64
}

/// Thumb-px shift converted to full-resolution px.
fn full_px(acc: [f64; 2]) -> [f64; 2] {
    [acc[0] * THUMB_DIV as f64, acc[1] * THUMB_DIV as f64]
}

/// Frame-to-frame shift search radius in thumb px (consecutive video frames
/// move little; a small window also cannot alias on periodic wall texture).
const SHIFT_RADIUS: isize = 8;

/// Incremental translation between two consecutive-frame thumbnails by
/// exhaustive zero-mean NCC over a small window, with parabolic sub-pixel
/// refinement: returns (dx, dy, peak score) in thumb px, where (dx, dy)
/// shifts image `a` onto image `b`.
fn ncc_shift_local(a: &[f32], b: &[f32], tw: usize, th: usize) -> Option<(f64, f64, f64)> {
    let score = |dx: isize, dy: isize| -> f64 {
        // Overlap window in `a` coordinates.
        let x0 = 0.max(-dx) as usize;
        let x1 = (tw as isize).min(tw as isize - dx) as usize;
        let y0 = 0.max(-dy) as usize;
        let y1 = (th as isize).min(th as isize - dy) as usize;
        if x1 <= x0 + 4 || y1 <= y0 + 4 {
            return -2.0;
        }
        let n = ((x1 - x0) * (y1 - y0)) as f64;
        let (mut sa, mut sb) = (0.0f64, 0.0f64);
        for y in y0..y1 {
            for x in x0..x1 {
                sa += a[y * tw + x] as f64;
                sb += b[(y as isize + dy) as usize * tw + (x as isize + dx) as usize] as f64;
            }
        }
        let (ma, mb) = (sa / n, sb / n);
        let (mut num, mut da, mut db) = (0.0f64, 0.0f64, 0.0f64);
        for y in y0..y1 {
            for x in x0..x1 {
                let va = a[y * tw + x] as f64 - ma;
                let vb = b[(y as isize + dy) as usize * tw + (x as isize + dx) as usize] as f64 - mb;
                num += va * vb;
                da += va * va;
                db += vb * vb;
            }
        }
        if da < 1e-6 || db < 1e-6 {
            return -2.0; // flat region: correlation undefined
        }
        num / (da * db).sqrt()
    };

    let mut best = (0isize, 0isize, -2.0f64);
    for dy in -SHIFT_RADIUS..=SHIFT_RADIUS {
        for dx in -SHIFT_RADIUS..=SHIFT_RADIUS {
            let s = score(dx, dy);
            if s > best.2 {
                best = (dx, dy, s);
            }
        }
    }
    if best.2 <= -2.0 {
        return None;
    }
    // Parabolic sub-pixel refinement: keeps the accumulated shift honest
    // over the ~10 frames between keyframes.
    let refine = |m: f64, l: f64, r: f64| -> f64 {
        let denom = l - 2.0 * m + r;
        if denom.abs() < 1e-12 || l <= -2.0 || r <= -2.0 {
            return 0.0;
        }
        (0.5 * (l - r) / denom).clamp(-0.5, 0.5)
    };
    let ox = refine(best.2, score(best.0 - 1, best.1), score(best.0 + 1, best.1));
    let oy = refine(best.2, score(best.0, best.1 - 1), score(best.0, best.1 + 1));
    Some((best.0 as f64 + ox, best.1 as f64 + oy, best.2))
}

// ---------------------------------------------------------------------------
// Feature extraction + matching (post-capture, between kept keyframes).

/// Harris-style corner extraction, grid-distributed so features spread along
/// the wall rather than clustering on one fixture. Returns up to ~2 corners
/// per grid cell, strongest first, capped at `max_pts`.
fn harris_corners(gray: &[u8], w: usize, h: usize, max_pts: usize) -> Vec<[f64; 2]> {
    const GRID_X: usize = 12;
    const GRID_Y: usize = 7;
    const MARGIN: usize = 10; // patch radius + search head-room

    if w <= 2 * MARGIN || h <= 2 * MARGIN {
        return Vec::new();
    }
    // Structure tensor with a 5x5 box window, computed via running sums.
    let mut ixx = vec![0f32; w * h];
    let mut iyy = vec![0f32; w * h];
    let mut ixy = vec![0f32; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let gx = gray[y * w + x + 1] as f32 - gray[y * w + x - 1] as f32;
            let gy = gray[(y + 1) * w + x] as f32 - gray[(y - 1) * w + x] as f32;
            ixx[y * w + x] = gx * gx;
            iyy[y * w + x] = gy * gy;
            ixy[y * w + x] = gx * gy;
        }
    }
    let box5 = |src: &[f32], x: usize, y: usize| -> f32 {
        let mut s = 0f32;
        for yy in y - 2..=y + 2 {
            for xx in x - 2..=x + 2 {
                s += src[yy * w + xx];
            }
        }
        s
    };

    let cell_w = (w - 2 * MARGIN) / GRID_X;
    let cell_h = (h - 2 * MARGIN) / GRID_Y;
    let mut all: Vec<(f32, [f64; 2])> = Vec::new();
    let mut global_max = 0f32;
    let mut cells: Vec<Vec<(f32, [f64; 2])>> = vec![Vec::new(); GRID_X * GRID_Y];

    for cy in 0..GRID_Y {
        for cx in 0..GRID_X {
            let x0 = MARGIN + cx * cell_w;
            let y0 = MARGIN + cy * cell_h;
            let x1 = (x0 + cell_w).min(w - MARGIN);
            let y1 = (y0 + cell_h).min(h - MARGIN);
            // Two best local maxima of the Harris response in this cell.
            let mut best: [(f32, [f64; 2]); 2] = [(0.0, [0.0, 0.0]); 2];
            let mut y = y0;
            while y < y1 {
                let mut x = x0;
                while x < x1 {
                    let sxx = box5(&ixx, x, y);
                    let syy = box5(&iyy, x, y);
                    let sxy = box5(&ixy, x, y);
                    let det = sxx * syy - sxy * sxy;
                    let tr = sxx + syy;
                    let r = det - 0.05 * tr * tr;
                    if r > best[0].0 {
                        // Keep the two strongest, at least 6 px apart.
                        let p = [x as f64, y as f64];
                        if (p[0] - best[0].1[0]).abs() + (p[1] - best[0].1[1]).abs() > 6.0 {
                            best[1] = best[0];
                        }
                        best[0] = (r, p);
                    } else if r > best[1].0
                        && (x as f64 - best[0].1[0]).abs() + (y as f64 - best[0].1[1]).abs() > 6.0
                    {
                        best[1] = (r, [x as f64, y as f64]);
                    }
                    x += 2; // stride 2: sub-pixel precision comes from NCC
                    global_max = global_max.max(r);
                }
                y += 2;
            }
            cells[cy * GRID_X + cx] = best.iter().filter(|(r, _)| *r > 0.0).cloned().collect();
        }
    }
    // Keep only responses that are meaningful relative to the frame's best.
    let threshold = global_max * 1e-4;
    for cell in cells {
        for (r, p) in cell {
            if r > threshold {
                all.push((r, p));
            }
        }
    }
    all.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    all.truncate(max_pts);
    all.into_iter().map(|(_, p)| p).collect()
}

/// Match a feature from `a` into `b` by zero-mean NCC over a search window
/// around `predicted`. Returns the sub-pixel match position and the peak.
fn ncc_match(
    a: &[u8],
    b: &[u8],
    w: usize,
    h: usize,
    p: [f64; 2],
    predicted: [f64; 2],
) -> Option<([f64; 2], f64)> {
    const R: isize = 6; // patch radius: 13x13
    const S: isize = 18; // search radius: covers accumulated shift error
                         // plus hand-wobble rotation at the frame edges

    let px = p[0].round() as isize;
    let py = p[1].round() as isize;
    if px < R || py < R || px >= w as isize - R || py >= h as isize - R {
        return None;
    }
    // Template stats.
    let n = ((2 * R + 1) * (2 * R + 1)) as f64;
    let mut sum = 0.0f64;
    for dy in -R..=R {
        for dx in -R..=R {
            sum += a[((py + dy) as usize) * w + (px + dx) as usize] as f64;
        }
    }
    let mean_a = sum / n;
    let mut var_a = 0.0f64;
    let mut tmpl = [0f64; 13 * 13];
    for dy in -R..=R {
        for dx in -R..=R {
            let v = a[((py + dy) as usize) * w + (px + dx) as usize] as f64 - mean_a;
            tmpl[((dy + R) * 13 + dx + R) as usize] = v;
            var_a += v * v;
        }
    }
    if var_a < 1e-3 {
        return None; // flat template
    }

    let cx = predicted[0].round() as isize;
    let cy = predicted[1].round() as isize;
    let mut best = (0isize, 0isize, -2.0f64);
    let mut scores = vec![-2.0f64; ((2 * S + 1) * (2 * S + 1)) as usize];
    for sy in -S..=S {
        for sx in -S..=S {
            let qx = cx + sx;
            let qy = cy + sy;
            if qx < R || qy < R || qx >= w as isize - R || qy >= h as isize - R {
                continue;
            }
            let mut sum_b = 0.0f64;
            for dy in -R..=R {
                for dx in -R..=R {
                    sum_b += b[((qy + dy) as usize) * w + (qx + dx) as usize] as f64;
                }
            }
            let mean_b = sum_b / n;
            let (mut num, mut var_b) = (0.0f64, 0.0f64);
            for dy in -R..=R {
                for dx in -R..=R {
                    let vb = b[((qy + dy) as usize) * w + (qx + dx) as usize] as f64 - mean_b;
                    num += tmpl[((dy + R) * 13 + dx + R) as usize] * vb;
                    var_b += vb * vb;
                }
            }
            if var_b < 1e-3 {
                continue;
            }
            let s = num / (var_a * var_b).sqrt();
            scores[((sy + S) * (2 * S + 1) + sx + S) as usize] = s;
            if s > best.2 {
                best = (sx, sy, s);
            }
        }
    }
    if best.2 < MIN_MATCH_NCC {
        return None;
    }
    // Parabolic sub-pixel refinement on the score surface.
    let at = |sx: isize, sy: isize| -> f64 {
        if sx < -S || sx > S || sy < -S || sy > S {
            return -2.0;
        }
        scores[((sy + S) * (2 * S + 1) + sx + S) as usize]
    };
    let refine = |m: f64, l: f64, r: f64| -> f64 {
        let denom = l - 2.0 * m + r;
        if denom.abs() < 1e-9 || l <= -2.0 || r <= -2.0 {
            return 0.0;
        }
        (0.5 * (l - r) / denom).clamp(-0.5, 0.5)
    };
    let ox = refine(best.2, at(best.0 - 1, best.1), at(best.0 + 1, best.1));
    let oy = refine(best.2, at(best.0, best.1 - 1), at(best.0, best.1 + 1));
    Some((
        [(cx + best.0) as f64 + ox, (cy + best.1) as f64 + oy],
        best.2,
    ))
}

// ---------------------------------------------------------------------------
// Post-capture processing.

/// Failure modes of the pan pipeline. Every variant carries enough context
/// for the capture page to tell the Homeowner what to do differently.
#[derive(Clone, Debug, PartialEq)]
pub enum PanError {
    InvalidCorrectionFactor,
    NoKeyframes,
    /// Marker A was not detected in any keyframe: the chain has no anchor.
    AnchorMarkerNotFound,
    /// Tracking between two consecutive keyframes produced too few reliable
    /// correspondences: the chain is broken at this segment.
    WeakSegment {
        from_keyframe: usize,
        to_keyframe: usize,
        matches: usize,
        inliers: usize,
    },
    /// Tracking was lost mid-pan (camera moved too fast, or a featureless
    /// stretch of wall) and later keyframes sit beyond the gap: the chain
    /// cannot connect them.
    TrackingLost { after_keyframe: usize },
    /// The anchor marker's homography could not be estimated.
    AnchorEstimateFailed,
    /// The stitched output extent degenerated (chain mapped everything to
    /// implausible wall coordinates).
    DegenerateExtent,
}

impl std::fmt::Display for PanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PanError::InvalidCorrectionFactor => {
                write!(f, "invalid print-scale correction factor")
            }
            PanError::NoKeyframes => write!(f, "no keyframes were captured during the pan"),
            PanError::AnchorMarkerNotFound => write!(
                f,
                "Reference Marker A was never seen: start the pan with the whole left marker in view"
            ),
            PanError::WeakSegment { from_keyframe, to_keyframe, matches, inliers } => write!(
                f,
                "tracking failed between keyframe {from_keyframe} and {to_keyframe} \
                 ({matches} matches, {inliers} agreeing): pan more slowly and steadily \
                 across that part of the wall"
            ),
            PanError::TrackingLost { after_keyframe } => write!(
                f,
                "tracking was lost after keyframe {after_keyframe} (the camera moved too \
                 fast or that stretch of wall had nothing to track): re-record the pan, \
                 moving slowly and steadily"
            ),
            PanError::AnchorEstimateFailed => {
                write!(f, "Reference Marker A was seen but its geometry could not be estimated")
            }
            PanError::DegenerateExtent => {
                write!(f, "the tracked pan did not produce a usable wall extent")
            }
        }
    }
}

impl std::error::Error for PanError {}

/// Quality record for one chain link (keyframe i -> i+1).
#[derive(Clone, Debug)]
pub struct LinkQuality {
    pub matches: usize,
    pub inliers: usize,
    pub rms_px: f64,
}

/// Loop-closure record.
#[derive(Clone, Debug)]
pub struct Closure {
    /// Keyframe index that provided Marker B.
    pub keyframe: usize,
    /// RMS distance (mm) between B's chained back-projection and the ideal
    /// rigid square BEFORE correction: the measured chain drift.
    pub discrepancy_mm: f64,
    /// RMS distance (mm) remaining AFTER the similarity correction — the
    /// non-rigid drift component redistribution cannot remove; feeds the
    /// Error Bound.
    pub residual_mm: f64,
    /// Scale factor the redistribution applied at the far end (1.0 = none).
    pub scale_correction: f64,
}

/// Error Bound evaluator: 95% bound in mm as a function of wall x (mm from
/// Marker A's printed top-left corner). See the module docs for the model.
#[derive(Clone, Debug)]
pub struct BoundModel {
    /// Anchor-corner noise (mm) times the worst vertical extrapolation lever
    /// of the rendered extent: a 4-corner fit of a 150 mm marker amplifies
    /// corner noise roughly linearly with distance in units of the marker
    /// side, and the loop closure says nothing about the vertical axis.
    sigma_a_mm: f64,
    /// Relative scale uncertainty (per mm of distance from the anchor):
    /// with closure, the closure's own precision; without, [`OPEN_LOOP_REL`].
    rel: f64,
    /// Measured post-closure residual at Marker B: (anchor->B chain span mm,
    /// residual mm). The residual is what redistribution could not remove —
    /// a real measurement of remaining error at B — ramped linearly from the
    /// anchor. `None` without closure.
    closure_ramp: Option<(f64, f64)>,
    /// (|wall x|, accumulated tracking variance mm^2), sorted by |x| with a
    /// running max — conservative monotone envelope of the random walk.
    track: Vec<(f64, f64)>,
}

impl BoundModel {
    /// Per-position component (mm) of the Error Bound at wall position x mm
    /// from the anchor origin.
    ///
    /// NOT a standalone 95% bound on absolute positions: positions carry a
    /// known systematic bias from drift redistribution (the progressive
    /// scale field can only correct up to the drift's unknown onset
    /// profile; issue #4's review Monte Carlo measured far-end position
    /// coverage of ~61%, driven by a same-signed displacement). That bias
    /// is common-mode — it largely cancels between two points measured in
    /// the same session — which is exactly why the distance contract of
    /// [`BoundModel::bound_between_mm`] holds. Consume this only as a
    /// building block for distance bounds.
    pub fn bound_at_mm(&self, x_mm: f64) -> f64 {
        let ax = x_mm.abs();
        let mut track_var = 0.0;
        for &(d, v) in &self.track {
            if d <= ax {
                track_var = v;
            } else {
                break;
            }
        }
        let ramp_var = match self.closure_ramp {
            Some((span, res)) if span > 1e-9 => (res * ax / span).powi(2),
            _ => 0.0,
        };
        K95 * (self.sigma_a_mm.powi(2) + track_var + (ax * self.rel).powi(2) + ramp_var).sqrt()
    }

    /// THE Error Bound contract (CONTEXT.md: per-session 95% bound on
    /// measurement error): 95% bound (mm) on a DISTANCE measured between
    /// wall positions `xa_mm` and `xb_mm`. Product measurements are
    /// distances (measure tool, Clear Zone dimensions), and this reading
    /// was empirically validated by issue #4's review Monte Carlo: 100%
    /// coverage over 381 pairs (worst error/bound ratio 0.89) across 80
    /// randomized pans. The endpoint SUM is deliberate — do not "tighten"
    /// it to an RSS combination: measured worst ratios (0.89) exceed the
    /// 1/sqrt(2) that an RSS bound assumes.
    pub fn bound_between_mm(&self, xa_mm: f64, xb_mm: f64) -> f64 {
        self.bound_at_mm(xa_mm) + self.bound_at_mm(xb_mm)
    }
}

/// Everything the recorded-pan pipeline produces.
pub struct PanOutput {
    /// Stitched full-wall Rectified Wall Image, tightly packed RGBA.
    pub rgba: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub mm_per_px: f64,
    /// Wall-plane mm of the output's top-left pixel corner.
    pub origin_mm: [f64; 2],
    pub keyframes_used: usize,
    /// True when the keyframe cap cut the recording short.
    pub truncated: bool,
    pub links: Vec<LinkQuality>,
    /// `None` when Marker B was never usable (open-loop result).
    pub closure: Option<Closure>,
    /// True when Marker B WAS detected but its closure had to be refused —
    /// implausible back-projection, an implausible measured drift/scale
    /// (see [`MAX_CLOSURE_DISCREPANCY_MM`] / [`MAX_CLOSURE_SCALE_DEV`]), or
    /// a degenerate correction. The result fell back to open-loop and the
    /// UI must tell the Homeowner to retake rather than trust silence.
    pub closure_rejected: bool,
    /// Self-calibrated intrinsics actually applied to this result (issue
    /// #6): the shared focal + division-model k1 the joint LM certified,
    /// with the curvature sigmas that certified them. `None` = pinhole
    /// fallback — the chain could not support calibration (too short, too
    /// fronto-parallel, too little rotation wobble) or the refinement did
    /// not prove itself; the honest response is the wider uncalibrated
    /// bound, never a garbage focal (see [`crate::calib`]).
    pub calibration: Option<SelfCalibration>,
    pub bound: BoundModel,
    /// Wall x (mm) of the anchor-nearest and far output edges — where the
    /// near/far bound scalars are evaluated.
    pub far_x_mm: f64,
}

impl PanOutput {
    pub fn bound_near_anchor_mm(&self) -> f64 {
        self.bound.bound_at_mm(0.0)
    }
    pub fn bound_far_end_mm(&self) -> f64 {
        self.bound.bound_at_mm(self.far_x_mm)
    }
    /// Worst-case per-position component over the MARKER-BRACKETED SPAN
    /// [0, `far_x_mm`] — the instrumented candidate area the two markers
    /// bracket (ADR-0002); the model is monotone in |x|, so the span
    /// maximum sits at `far_x_mm`. Rendered pixels beyond the markers are
    /// uninstrumented extrapolation and can exceed this value —
    /// [`BoundModel::bound_at_mm`] / [`BoundModel::bound_between_mm`]
    /// remain valid to evaluate out there.
    pub fn bound_worst_mm(&self) -> f64 {
        self.bound_far_end_mm()
    }
}

/// Local wall-mm-per-source-px scale of a wall->px homography around a wall
/// point, estimated from finite differences.
fn local_mm_per_px(w_h: &Homography, wall: [f64; 2]) -> Option<f64> {
    let p0 = w_h.apply(wall[0], wall[1])?;
    let px = w_h.apply(wall[0] + 1.0, wall[1])?;
    let py = w_h.apply(wall[0], wall[1] + 1.0)?;
    let du = ((px.0 - p0.0).powi(2) + (px.1 - p0.1).powi(2)).sqrt();
    let dv = ((py.0 - p0.0).powi(2) + (py.1 - p0.1).powi(2)).sqrt();
    let px_per_mm = (du + dv) / 2.0;
    if px_per_mm < 1e-9 {
        return None;
    }
    Some(1.0 / px_per_mm)
}

/// Bilinear luma sample (callers guarantee 1 px of margin).
fn sample_luma(gray: &[u8], w: usize, x: f64, y: f64) -> f64 {
    let xf = x.floor() as usize;
    let yf = y.floor() as usize;
    let fx = x - xf as f64;
    let fy = y - yf as f64;
    let g = |xx: usize, yy: usize| gray[yy * w + xx] as f64;
    g(xf, yf) * (1.0 - fx) * (1.0 - fy)
        + g(xf + 1, yf) * fx * (1.0 - fy)
        + g(xf, yf + 1) * (1.0 - fx) * fy
        + g(xf + 1, yf + 1) * fx * fy
}

/// Edge blur width (px) of a detected marker's border: the luma profile
/// along the outward normal at points along each edge is a black->white
/// step whose contrast/max-gradient ratio measures how many pixels the
/// transition is smeared over (~2 px sharp; grows with motion/defocus
/// blur). Content-independent, unlike whole-frame sharpness — a marker
/// keyframe always out-scores a bare-wall keyframe on Tenengrad no matter
/// how blurred the marker is.
fn marker_edge_blur_px(gray: &[u8], w: usize, h: usize, corners: &[[f64; 2]; 4]) -> f64 {
    let cx = corners.iter().map(|c| c[0]).sum::<f64>() / 4.0;
    let cy = corners.iter().map(|c| c[1]).sum::<f64>() / 4.0;
    let mut widths: Vec<f64> = Vec::new();
    for k in 0..4 {
        let a = corners[k];
        let b = corners[(k + 1) % 4];
        for frac in [0.3, 0.5, 0.7] {
            let mx = a[0] + (b[0] - a[0]) * frac;
            let my = a[1] + (b[1] - a[1]) * frac;
            let (mut nx, mut ny) = (-(b[1] - a[1]), b[0] - a[0]);
            let len = (nx * nx + ny * ny).sqrt();
            if len < 1e-9 {
                continue;
            }
            nx /= len;
            ny /= len;
            if (mx - cx) * nx + (my - cy) * ny < 0.0 {
                nx = -nx;
                ny = -ny;
            }
            let mut prof = [0.0f64; 7];
            let mut ok = true;
            for (j, t) in (-3..=3).enumerate() {
                let x = mx + nx * t as f64;
                let y = my + ny * t as f64;
                if x < 1.0 || y < 1.0 || x > (w - 2) as f64 || y > (h - 2) as f64 {
                    ok = false;
                    break;
                }
                prof[j] = sample_luma(gray, w, x, y);
            }
            if !ok {
                continue;
            }
            let contrast =
                prof.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
                    - prof.iter().cloned().fold(f64::INFINITY, f64::min);
            let mut grad = 0.0f64;
            for j in 0..6 {
                grad = grad.max((prof[j + 1] - prof[j]).abs());
            }
            if contrast > 20.0 && grad > 1e-9 {
                widths.push((contrast / grad).clamp(1.0, 6.0));
            }
        }
    }
    if widths.is_empty() {
        return 2.0; // no measurable edge: assume nominal sharpness
    }
    widths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    widths[widths.len() / 2]
}

/// 2D similarity fit (Umeyama, no reflection): scale, rotation, translation
/// mapping `src` onto `dst` in the least-squares sense.
fn fit_similarity(src: &[[f64; 2]; 4], dst: &[[f64; 2]; 4]) -> (f64, f64, [f64; 2]) {
    let cen = |pts: &[[f64; 2]; 4]| -> [f64; 2] {
        let mut c = [0.0, 0.0];
        for p in pts {
            c[0] += p[0] / 4.0;
            c[1] += p[1] / 4.0;
        }
        c
    };
    let cs = cen(src);
    let cd = cen(dst);
    let (mut a, mut b, mut n2) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..4 {
        let sx = src[i][0] - cs[0];
        let sy = src[i][1] - cs[1];
        let dx = dst[i][0] - cd[0];
        let dy = dst[i][1] - cd[1];
        a += sx * dx + sy * dy;
        b += sx * dy - sy * dx;
        n2 += sx * sx + sy * sy;
    }
    let scale = (a * a + b * b).sqrt() / n2.max(1e-12);
    let theta = b.atan2(a);
    let (s, c) = theta.sin_cos();
    let t = [
        cd[0] - scale * (c * cs[0] - s * cs[1]),
        cd[1] - scale * (s * cs[0] + c * cs[1]),
    ];
    (scale, theta, t)
}

fn quad_area(c: &[[f64; 2]; 4]) -> f64 {
    let mut a = 0.0;
    for i in 0..4 {
        let j = (i + 1) % 4;
        a += c[i][0] * c[j][1] - c[j][0] * c[i][1];
    }
    a.abs() * 0.5
}

/// Raw per-link correspondences: NCC feature matches plus shared marker
/// corners, in measured (possibly lens-distorted) px. Collected once —
/// both the pinhole pass and the calibrated re-estimation consume them.
fn collect_link_matches(
    keyframes: &[Keyframe],
    detections: &[Vec<DetectedMarker>],
    width: usize,
    height: usize,
) -> Vec<(Vec<[f64; 2]>, Vec<[f64; 2]>)> {
    let n = keyframes.len();
    let find = |i: usize, id: u16| detections[i].iter().find(|m| m.id == id);
    let mut out = Vec::with_capacity(n.saturating_sub(1));
    for i in 0..n.saturating_sub(1) {
        let (a, b) = (&keyframes[i], &keyframes[i + 1]);
        // Capture-time accumulated shift: seeds the full-res patch search.
        let shift = b.shift_from_prev;

        let corners = harris_corners(&a.gray, width, height, 150);
        let mut src: Vec<[f64; 2]> = Vec::new();
        let mut dst: Vec<[f64; 2]> = Vec::new();
        for p in corners {
            let predicted = [p[0] + shift[0], p[1] + shift[1]];
            if let Some((q, _)) = ncc_match(&a.gray, &b.gray, width, height, p, predicted) {
                src.push(p);
                dst.push(q);
            }
        }
        // Shared marker corners are the highest-quality correspondences.
        for id in [LEFT_MARKER_ID, RIGHT_MARKER_ID] {
            if let (Some(ma), Some(mb)) = (find(i, id), find(i + 1, id)) {
                src.extend_from_slice(&ma.corners);
                dst.extend_from_slice(&mb.corners);
            }
        }
        out.push((src, dst));
    }
    out
}

struct LinkEstimates {
    links: Vec<Homography>,
    quality: Vec<LinkQuality>,
    /// RANSAC-inlier correspondences in RAW measured px, evenly subsampled
    /// to [`calib::CALIB_MATCHES_PER_LINK`] — the self-calibration bundle's
    /// observations (it applies its own distortion model to raw points).
    inliers_raw: Vec<(Vec<[f64; 2]>, Vec<[f64; 2]>)>,
}

/// Estimate all chain links from the raw matches, undistorting every point
/// through `dist` first (exact identity when uncalibrated).
/// links[i]: (ideal) px_i -> (ideal) px_{i+1}.
fn estimate_links(
    raw: &[(Vec<[f64; 2]>, Vec<[f64; 2]>)],
    dist: &Distortion,
    width: usize,
    height: usize,
) -> Result<LinkEstimates, PanError> {
    let mut links: Vec<Homography> = Vec::with_capacity(raw.len());
    let mut quality: Vec<LinkQuality> = Vec::with_capacity(raw.len());
    let mut inliers_raw: Vec<(Vec<[f64; 2]>, Vec<[f64; 2]>)> = Vec::with_capacity(raw.len());
    for (i, (src_raw, dst_raw)) in raw.iter().enumerate() {
        let src: Vec<[f64; 2]> = src_raw.iter().map(|&p| dist.undistort(p)).collect();
        let dst: Vec<[f64; 2]> = dst_raw.iter().map(|&p| dist.undistort(p)).collect();
        let matches = src.len();
        // Fewer matches than the inlier gate accepts can never pass it —
        // skip the estimation work (this threshold deliberately equals
        // MIN_LINK_INLIERS; a lower one would be dead code).
        let est = (matches >= MIN_LINK_INLIERS)
            .then(|| estimate(&src, &dst, LINK_INLIER_THRESHOLD_PX))
            .flatten();
        // Spread guard: inliers clustered in one corner of the frame cannot
        // support extrapolation across it.
        let spread_ok = |e: &crate::homography::Estimate| -> bool {
            let mut min = [f64::INFINITY; 2];
            let mut max = [f64::NEG_INFINITY; 2];
            for (k, r) in e.residuals.iter().enumerate() {
                if *r <= LINK_INLIER_THRESHOLD_PX {
                    for a in 0..2 {
                        min[a] = min[a].min(src[k][a]);
                        max[a] = max[a].max(src[k][a]);
                    }
                }
            }
            (max[0] - min[0]) / width as f64 >= MIN_LINK_SPAN_FRAC
                || (max[1] - min[1]) / height as f64 >= MIN_LINK_SPAN_FRAC
        };
        match est {
            Some(e) if e.inliers >= MIN_LINK_INLIERS && spread_ok(&e) => {
                // Fit quality = inlier RMS: `e.rms` runs over all matches
                // including the outliers RANSAC excluded from the fit, which
                // would inflate the Error Bound with errors that never
                // entered the model.
                let (mut acc, mut cnt) = (0.0f64, 0usize);
                let mut idxs: Vec<usize> = Vec::new();
                for (k, r) in e.residuals.iter().enumerate() {
                    if *r <= LINK_INLIER_THRESHOLD_PX {
                        acc += r * r;
                        cnt += 1;
                        idxs.push(k);
                    }
                }
                let step = idxs.len().div_ceil(calib::CALIB_MATCHES_PER_LINK).max(1);
                let (mut in_s, mut in_d) = (Vec::new(), Vec::new());
                for (j, &k) in idxs.iter().enumerate() {
                    if j % step == 0 {
                        in_s.push(src_raw[k]);
                        in_d.push(dst_raw[k]);
                    }
                }
                inliers_raw.push((in_s, in_d));
                quality.push(LinkQuality {
                    matches,
                    inliers: e.inliers,
                    rms_px: (acc / cnt.max(1) as f64).sqrt(),
                });
                links.push(e.h);
            }
            other => {
                return Err(PanError::WeakSegment {
                    from_keyframe: i,
                    to_keyframe: i + 1,
                    matches,
                    inliers: other.map_or(0, |e| e.inliers),
                });
            }
        }
    }
    Ok(LinkEstimates { links, quality, inliers_raw })
}

/// Chain the links outward from the anchor: W[i] maps wall mm -> keyframe i
/// (ideal) px.
fn build_chain(
    links: &[Homography],
    anchor_h: &Homography,
    ka: usize,
) -> Result<Vec<[f64; 9]>, PanError> {
    let n = links.len() + 1;
    let mut w_chain: Vec<[f64; 9]> = vec![[0.0; 9]; n];
    w_chain[ka] = anchor_h.0;
    for i in ka + 1..n {
        w_chain[i] = mat3_mul(&links[i - 1].0, &w_chain[i - 1]);
    }
    for i in (0..ka).rev() {
        let inv = mat3_inv(&links[i].0).ok_or(PanError::DegenerateExtent)?;
        w_chain[i] = mat3_mul(&inv, &w_chain[i + 1]);
    }
    Ok(w_chain)
}

#[allow(clippy::too_many_arguments)]
fn process(
    keyframes: &[Keyframe],
    width: usize,
    height: usize,
    correction_factor: f64,
    close_loop: bool,
    truncated: bool,
    calibrate: bool,
) -> Result<PanOutput, PanError> {
    let n = keyframes.len();
    let side_mm = MARKER_SIDE_MM * correction_factor;

    // 1. Marker detection per keyframe (RAW measured corners).
    let detections: Vec<Vec<DetectedMarker>> = keyframes
        .iter()
        .map(|kf| detect_markers(&kf.gray, width, height))
        .collect();
    let find_raw = |i: usize, id: u16| detections[i].iter().find(|m| m.id == id);

    // 2. Anchor at the best Marker A detection.
    let ka = (0..n)
        .filter_map(|i| find_raw(i, LEFT_MARKER_ID).map(|m| (i, quad_area(&m.corners))))
        .max_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .ok_or(PanError::AnchorMarkerNotFound)?;
    let world = anchor_corners_mm(side_mm);

    // 3. Per-link tracking (collected once) + pinhole estimation pass:
    //    anchor homography, links[i]: px_i -> px_{i+1}, and the chain.
    let raw_matches = collect_link_matches(keyframes, &detections, width, height);
    let est_a_raw = estimate(
        &world,
        &find_raw(ka, LEFT_MARKER_ID).unwrap().corners,
        LINK_INLIER_THRESHOLD_PX,
    )
    .ok_or(PanError::AnchorEstimateFailed)?;
    let le = estimate_links(&raw_matches, &Distortion::none(width, height), width, height)?;
    let chain0 = build_chain(&le.links, &est_a_raw.h, ka)?;

    // 3b. Self-calibration (issue #6, see crate::calib and the module docs):
    //     jointly refine {focal, k1, keyframe poses} over the marker corners
    //     and tracked inliers, then — only when the conditioning gates pass —
    //     undistort every measured point and re-run anchor + links + chain
    //     on the corrected points. Any failure on the calibrated re-run
    //     falls back to the pinhole pass rather than half-applying a lens.
    let mut dist = Distortion::none(width, height);
    let mut calibration: Option<SelfCalibration> = None;
    let mut dets_u: Option<Vec<Vec<DetectedMarker>>> = None;
    let (mut est_a, mut link_quality, mut w_chain) = (est_a_raw, le.quality, chain0);
    if calibrate && n >= calib::MIN_CALIB_KEYFRAMES {
        let marker_a_obs: Vec<(usize, [[f64; 2]; 4])> = (0..n)
            .filter_map(|i| find_raw(i, LEFT_MARKER_ID).map(|m| (i, m.corners)))
            .collect();
        // Bootstrap k1 by a coarse deterministic scan minimizing the links'
        // inlier RMS: a homography fits UNDISTORTED matches much better
        // than distorted ones, and the signal is independent of the focal.
        // This matters because the raw pinhole chain of a genuinely
        // distorted lens absorbs the distortion field into per-link
        // perspective terms — chained up, those compound into a badly
        // deformed chain that would poison the bundle's pose
        // initialization.
        let link_rms = |le: &LinkEstimates| -> f64 {
            le.quality.iter().map(|q| q.rms_px).sum::<f64>() / le.quality.len().max(1) as f64
        };
        let mut samples: Vec<(f64, f64, LinkEstimates)> = Vec::new();
        let mut k1v = -0.30;
        while k1v <= 0.061 {
            let d = Distortion::new(width, height, k1v);
            if let Ok(le) = estimate_links(&raw_matches, &d, width, height) {
                let obj = link_rms(&le);
                samples.push((k1v, obj, le));
            }
            k1v += 0.02;
        }
        let best_i = samples
            .iter()
            .enumerate()
            .min_by(|a, b| a.1 .1.partial_cmp(&b.1 .1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i);
        let boot = best_i.and_then(|bi| {
            let mut k1_boot = samples[bi].0;
            let obj_mid = samples[bi].1;
            // Parabolic refine when both equally spaced neighbours exist.
            if bi > 0 && bi + 1 < samples.len() {
                let (kl, ol, _) = &samples[bi - 1];
                let (kr, or_, _) = &samples[bi + 1];
                if (kl - (k1_boot - 0.02)).abs() < 1e-9 && (kr - (k1_boot + 0.02)).abs() < 1e-9 {
                    let denom = ol - 2.0 * obj_mid + or_;
                    if denom > 1e-12 {
                        k1_boot += (0.02 * 0.5 * (ol - or_) / denom).clamp(-0.02, 0.02);
                    }
                }
            }
            let d_boot = Distortion::new(width, height, k1_boot);
            let a_corners = find_raw(ka, LEFT_MARKER_ID).unwrap().corners;
            let a_u = [
                d_boot.undistort(a_corners[0]),
                d_boot.undistort(a_corners[1]),
                d_boot.undistort(a_corners[2]),
                d_boot.undistort(a_corners[3]),
            ];
            let ea = estimate(&world, &a_u, LINK_INLIER_THRESHOLD_PX)?;
            let le_boot = estimate_links(&raw_matches, &d_boot, width, height).ok()?;
            let chain = build_chain(&le_boot.links, &ea.h, ka).ok()?;
            Some((k1_boot, chain, le_boot.inliers_raw))
        });
        let (k1_init, boot_chain, boot_inliers) = match &boot {
            Some((k1b, ch, inl)) => (*k1b, ch, inl),
            None => (0.0, &w_chain, &le.inliers_raw),
        };
        let sc = calib::self_calibrate(&calib::CalibInput {
            width,
            height,
            side_mm,
            k1_init,
            w_chain: boot_chain,
            marker_a: &marker_a_obs,
            links: boot_inliers,
        });
        if let Some((sc, bundle_chain)) = sc {
            let d2 = Distortion::new(width, height, sc.k1);
            let undist_marker = |m: &DetectedMarker| DetectedMarker {
                id: m.id,
                corners: [
                    d2.undistort(m.corners[0]),
                    d2.undistort(m.corners[1]),
                    d2.undistort(m.corners[2]),
                    d2.undistort(m.corners[3]),
                ],
            };
            let du: Vec<Vec<DetectedMarker>> = detections
                .iter()
                .map(|v| v.iter().map(undist_marker).collect())
                .collect();
            // The chain comes from the bundle's poses (jointly optimal over
            // every observation at once; pairwise re-chaining would compound
            // per-link errors again). The per-link estimates are still re-run
            // on undistorted points for the Error Bound's link-quality terms,
            // and the anchor is re-fit for its corner residual.
            let redo = estimate(
                &world,
                &du[ka].iter().find(|m| m.id == LEFT_MARKER_ID).unwrap().corners,
                LINK_INLIER_THRESHOLD_PX,
            )
            .and_then(|ea| {
                let l = estimate_links(&raw_matches, &d2, width, height).ok()?;
                Some((ea, l.quality))
            });
            if let Some((ea, lq)) = redo {
                est_a = ea;
                link_quality = lq;
                w_chain = bundle_chain;
                dist = d2;
                dets_u = Some(du);
                calibration = Some(sc);
            }
        }
    }
    let dets = dets_u.as_ref().unwrap_or(&detections);
    let find = |i: usize, id: u16| dets[i].iter().find(|m| m.id == id);
    let marker_a = find(ka, LEFT_MARKER_ID).unwrap();

    // Wall-frame chain positions (frame centers) for closure weights and the
    // tracking random walk.
    let center = [width as f64 / 2.0, height as f64 / 2.0];
    let mut wall_center = vec![[0.0f64; 2]; n];
    for i in 0..n {
        let inv = mat3_inv(&w_chain[i]).ok_or(PanError::DegenerateExtent)?;
        let (x, y) = Homography(inv)
            .apply(center[0], center[1])
            .ok_or(PanError::DegenerateExtent)?;
        wall_center[i] = [x, y];
    }
    // Signed along-chain distance from the anchor keyframe.
    let mut s_dist = vec![0.0f64; n];
    for i in ka + 1..n {
        let d = ((wall_center[i][0] - wall_center[i - 1][0]).powi(2)
            + (wall_center[i][1] - wall_center[i - 1][1]).powi(2))
        .sqrt();
        s_dist[i] = s_dist[i - 1] + d;
    }
    for i in (0..ka).rev() {
        let d = ((wall_center[i][0] - wall_center[i + 1][0]).powi(2)
            + (wall_center[i][1] - wall_center[i + 1][1]).powi(2))
        .sqrt();
        s_dist[i] = s_dist[i + 1] - d;
    }

    // 5. Loop closure against Marker B. The closure keyframe is chosen by
    //    detection quality — area WEIGHTED BY SHARPNESS — not raw area: a
    //    close-but-motion-blurred B otherwise beats a smaller sharp one and
    //    poisons the closure (issue #4 review).
    let kb = (0..n)
        .filter_map(|i| {
            find(i, RIGHT_MARKER_ID)
                .map(|m| (i, quad_area(&m.corners) * keyframes[i].sharpness.max(1e-9)))
        })
        .max_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i);

    let mut closure: Option<Closure> = None;
    let mut closure_rejected = false;
    let mut rel_from_closure: Option<f64> = None;
    let mut closure_ramp: Option<(f64, f64)> = None;
    let mut b_far_x: Option<f64> = None;
    if let (Some(kb), true) = (kb, close_loop) {
        let marker_b = find(kb, RIGHT_MARKER_ID).unwrap();
        // Back-project B through the A-anchored chain.
        let q = mat3_inv(&w_chain[kb]).and_then(|inv| {
            let hinv = Homography(inv);
            let mut q = [[0.0f64; 2]; 4];
            for (k, c) in marker_b.corners.iter().enumerate() {
                match hinv.apply(c[0], c[1]) {
                    Some((x, y))
                        if x.abs() < MAX_PAN_EXTENT_MM * 2.0
                            && y.abs() < MAX_PAN_EXTENT_MM * 2.0 =>
                    {
                        q[k] = [x, y]
                    }
                    _ => return None, // implausible: bad detection, no closure
                }
            }
            Some(q)
        });
        if q.is_none() {
            // B was seen but back-projects implausibly: refuse, and say so.
            closure_rejected = true;
        }
        if let Some(q) = q {
            // Ideal rigid square of the known physical side, posed to fit
            // the back-projection: the pose is unknown (the Homeowner taped
            // B wherever), the SIZE is the metric constraint (ADR-0002).
            let local = anchor_corners_mm(side_mm);
            let s_fit = fit_rigid_square(&local, &q);
            let rms4 = |pts: &[[f64; 2]; 4], refs: &[[f64; 2]; 4]| -> f64 {
                let mut acc = 0.0;
                for k in 0..4 {
                    acc += (pts[k][0] - refs[k][0]).powi(2) + (pts[k][1] - refs[k][1]).powi(2);
                }
                (acc / 4.0).sqrt()
            };
            // Measured chain drift at B: how far the chained back-projection
            // is from being a true-size square. Its similarity fit isolates
            // the dominant observable, the LOCAL scale error at B (the
            // rigid fit already absorbed translation/rotation, which are
            // unobservable with B's pose unknown).
            let discrepancy_mm = rms4(&q, &s_fit);
            let (scale, _theta, _trans) = fit_similarity(&q, &s_fit);

            // Plausibility guard (issue #4 review): drift beyond these
            // limits is not credible chain drift — rotate-in-place "pans"
            // measured 46% scale rewrites here and redistributing them
            // produced confident garbage. Refuse; fall to open-loop + flag.
            if discrepancy_mm > MAX_CLOSURE_DISCREPANCY_MM
                || (scale - 1.0).abs() > MAX_CLOSURE_SCALE_DEV
            {
                closure_rejected = true;
            } else {
            // Redistribute the drift as a progressive local-scale field:
            // the chain's scale error accumulated link by link, so the
            // correction at chain fraction t is a LOCAL scale of s^t
            // (log-space interpolation of the similarity), and keyframe
            // positions move by the integral of that field along the chain
            // — correcting positions with the full global scale would
            // overshoot by ~2x, since drift builds progressively rather
            // than being present from the anchor onward.
            let span = s_dist[kb];
            let t_of = |i: usize| -> f64 {
                if span.abs() < 1e-9 {
                    1.0
                } else {
                    (s_dist[i] / span).clamp(0.0, 1.0)
                }
            };
            let mut c_corr = wall_center.clone();
            for i in ka + 1..n {
                let sm = scale.powf((t_of(i - 1) + t_of(i)) / 2.0);
                for a in 0..2 {
                    c_corr[i][a] =
                        c_corr[i - 1][a] + sm * (wall_center[i][a] - wall_center[i - 1][a]);
                }
            }
            for i in (0..ka).rev() {
                let sm = scale.powf((t_of(i + 1) + t_of(i)) / 2.0);
                for a in 0..2 {
                    c_corr[i][a] =
                        c_corr[i + 1][a] + sm * (wall_center[i][a] - wall_center[i + 1][a]);
                }
            }
            let mut applied = true;
            let mut w_corr = w_chain.clone();
            for i in 0..n {
                let st = scale.powf(t_of(i));
                // Wall-side correction F_i: local scale st about the
                // keyframe's chain position, re-centred on the integrated
                // position. W_i' = W_i . F_i^-1.
                let f = [
                    st,
                    0.0,
                    c_corr[i][0] - st * wall_center[i][0],
                    0.0,
                    st,
                    c_corr[i][1] - st * wall_center[i][1],
                    0.0,
                    0.0,
                    1.0,
                ];
                match mat3_inv(&f) {
                    Some(f_inv) => w_corr[i] = mat3_mul(&w_chain[i], &f_inv),
                    None => {
                        applied = false;
                        break;
                    }
                }
            }

            if applied {
                w_chain = w_corr;
                // Residual drift at B after redistribution: re-measure the
                // back-projected quad against a (re-posed) true-size square.
                let residual_mm = mat3_inv(&w_chain[kb])
                    .and_then(|inv| {
                        let hinv = Homography(inv);
                        let mut q2 = [[0.0f64; 2]; 4];
                        for (k, c) in marker_b.corners.iter().enumerate() {
                            let (x, y) = hinv.apply(c[0], c[1])?;
                            q2[k] = [x, y];
                        }
                        let s2 = fit_rigid_square(&local, &q2);
                        b_far_x = Some(
                            s2.iter()
                                .map(|p| p[0])
                                .fold(0.0f64, |acc, v| if v.abs() > acc.abs() { v } else { acc }),
                        );
                        Some(rms4(&q2, &s2))
                    })
                    .unwrap_or(discrepancy_mm);

                // Closure precision: B's corner noise over the marker
                // baseline, as relative scale uncertainty of the metric it
                // anchors. Var(scale) = sigma^2 / sum|q~|^2 = sigma^2 /
                // (2 side^2) for a square's 4 corners.
                //
                // sigma is MEASURED, not assumed (issue #4 review: a
                // constant here silently tightened the bound exactly when
                // B was blurred). Two real measurements feed it:
                //  * B's own edge blur width in its closure keyframe —
                //    corner localization error grows roughly linearly with
                //    the edge transition width, so the floor is scaled by
                //    (blur_px / 2), the sharp-edge reference;
                //  * when >= 2 keyframes see B, the per-view closure-scale
                //    scatter — a direct empirical precision of the very
                //    quantity the closure estimates, absorbing blur and
                //    chain noise at B.
                // (The rigid-fit residual is NOT used here: it is dominated
                // by systematic projective distortion of the chain at B and
                // already feeds the bound via the closure ramp.)
                let mmpp_b = local_mm_per_px(&Homography(w_chain[kb]), s_fit[0]).unwrap_or(1.0);
                // Blur is profiled on the RAW image, so it needs the RAW
                // (distorted) corner positions, not the undistorted ones.
                let blur_px = marker_edge_blur_px(
                    &keyframes[kb].gray,
                    width,
                    height,
                    &find_raw(kb, RIGHT_MARKER_ID).unwrap().corners,
                );
                let blur_penalty = (blur_px / 2.0).max(1.0);
                let sigma_b_mm = CORNER_SIGMA_FLOOR_PX * mmpp_b * blur_penalty;
                let mut sigma_scale = sigma_b_mm / (side_mm * std::f64::consts::SQRT_2);
                let view_scales: Vec<f64> = (0..n)
                    .filter_map(|i| {
                        let m = find(i, RIGHT_MARKER_ID)?;
                        let hinv = Homography(mat3_inv(&w_chain[i])?);
                        let mut qv = [[0.0f64; 2]; 4];
                        for (k, c) in m.corners.iter().enumerate() {
                            let (x, y) = hinv.apply(c[0], c[1])?;
                            if x.abs() > MAX_PAN_EXTENT_MM * 2.0
                                || y.abs() > MAX_PAN_EXTENT_MM * 2.0
                            {
                                return None;
                            }
                            qv[k] = [x, y];
                        }
                        let fit = fit_rigid_square(&local, &qv);
                        Some(fit_similarity(&qv, &fit).0)
                    })
                    .collect();
                if view_scales.len() >= 2 {
                    let m = view_scales.len() as f64;
                    let mean = view_scales.iter().sum::<f64>() / m;
                    let var =
                        view_scales.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / (m - 1.0);
                    sigma_scale = sigma_scale.max(var.sqrt());
                }
                rel_from_closure = Some(sigma_scale);
                // And the measured residual at B, ramped over the anchor->B
                // span.
                closure_ramp = Some((span.abs().max(side_mm), residual_mm));
                closure = Some(Closure {
                    keyframe: kb,
                    discrepancy_mm,
                    residual_mm,
                    scale_correction: scale,
                });
            } else {
                // Degenerate correction with B present: refuse, and say so.
                closure_rejected = true;
            }
            } // end plausibility-guard else
        }
    }
    // Refresh chain positions after redistribution (stitching seams and the
    // bound's position axis use them).
    for i in 0..n {
        if let Some(inv) = mat3_inv(&w_chain[i]) {
            if let Some((x, y)) = Homography(inv).apply(center[0], center[1]) {
                wall_center[i] = [x, y];
            }
        }
    }

    // 6. Error Bound.
    let w_anchor = Homography(w_chain[ka]);
    let mmpp_a = local_mm_per_px(&w_anchor, [side_mm / 2.0, side_mm / 2.0])
        .ok_or(PanError::DegenerateExtent)?;
    let sigma_a_mm = est_a.rms.max(CORNER_SIGMA_FLOOR_PX) * mmpp_a;
    // Per-link tracking noise in mm at each keyframe's local scale,
    // accumulated as a random walk outward from the anchor.
    let mut track_var = vec![0.0f64; n];
    let link_var = |i: usize| -> f64 {
        // Link i connects keyframes i and i+1.
        let mmpp = local_mm_per_px(&Homography(w_chain[i + 1]), wall_center[i + 1])
            .unwrap_or(mmpp_a);
        (link_quality[i].rms_px.max(CORNER_SIGMA_FLOOR_PX) * mmpp).powi(2)
    };
    for i in ka + 1..n {
        track_var[i] = track_var[i - 1] + link_var(i - 1);
    }
    for i in (0..ka).rev() {
        track_var[i] = track_var[i + 1] + link_var(i);
    }
    // Monotone envelope over |wall x| (conservative).
    let mut track_pts: Vec<(f64, f64)> = (0..n)
        .map(|i| (wall_center[i][0].abs(), track_var[i]))
        .collect();
    track_pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut running = 0.0f64;
    for p in &mut track_pts {
        running = running.max(p.1);
        p.1 = running;
    }

    // 7. Stitch: output extent = union of keyframe footprints on the wall.
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    // The chain lives in IDEAL (undistorted) px; the actual frame's corner
    // positions in that space are the undistorted image corners (identity
    // when uncalibrated).
    let frame_corners = [
        [0.0, 0.0],
        [width as f64, 0.0],
        [width as f64, height as f64],
        [0.0, height as f64],
    ]
    .map(|fc| dist.undistort(fc));
    let mut inv_chain: Vec<[f64; 9]> = Vec::with_capacity(n);
    for w_i in &w_chain {
        let inv = mat3_inv(w_i).ok_or(PanError::DegenerateExtent)?;
        for fc in frame_corners {
            if let Some((x, y)) = Homography(inv).apply(fc[0], fc[1]) {
                min_x = min_x.min(x.clamp(-MAX_PAN_EXTENT_MM, MAX_PAN_EXTENT_MM));
                max_x = max_x.max(x.clamp(-MAX_PAN_EXTENT_MM, MAX_PAN_EXTENT_MM));
                min_y = min_y.min(y.clamp(-MAX_PAN_EXTENT_MM, MAX_PAN_EXTENT_MM));
                max_y = max_y.max(y.clamp(-MAX_PAN_EXTENT_MM, MAX_PAN_EXTENT_MM));
            }
        }
        inv_chain.push(inv);
    }
    if !(min_x.is_finite() && min_y.is_finite() && max_x > min_x && max_y > min_y) {
        return Err(PanError::DegenerateExtent);
    }

    // Resolution: the anchor marker's native detail, capped by output size.
    let mut side_px = 0.0;
    for k in 0..4 {
        let p = marker_a.corners[k];
        let q = marker_a.corners[(k + 1) % 4];
        side_px += ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt();
    }
    side_px /= 4.0;
    let native_mm_per_px = side_mm / side_px.max(1.0);
    let mm_per_px = native_mm_per_px
        .max((max_x - min_x) / MAX_PAN_OUT_W as f64)
        .max((max_y - min_y) / MAX_PAN_OUT_H as f64);
    let out_w = (((max_x - min_x) / mm_per_px).round() as usize).clamp(8, MAX_PAN_OUT_W);
    let out_h = (((max_y - min_y) / mm_per_px).round() as usize).clamp(8, MAX_PAN_OUT_H);

    // Pick-best-source stitch, Voronoi style: each output pixel is rendered
    // from the covering keyframe whose view center is nearest on the wall.
    // Nearest-center minimizes extrapolation from that keyframe's estimated
    // homography (the metric error lever) and is also the least oblique,
    // best-resolved view of that spot; capture-time candidate selection
    // already made sure the kept keyframes are the sharp ones. Seams are
    // straight Voronoi edges with no blending (v1).
    let mut rgba_out = vec![0u8; out_w * out_h * 4];
    let mut best_d2 = vec![f32::INFINITY; out_w * out_h];
    for i in 0..n {
        // Footprint bbox in output px (homographies map the frame quad to a
        // quad; its corner bbox contains it).
        let mut bx0 = f64::INFINITY;
        let mut by0 = f64::INFINITY;
        let mut bx1 = f64::NEG_INFINITY;
        let mut by1 = f64::NEG_INFINITY;
        for fc in frame_corners {
            if let Some((x, y)) = Homography(inv_chain[i]).apply(fc[0], fc[1]) {
                bx0 = bx0.min((x - min_x) / mm_per_px);
                bx1 = bx1.max((x - min_x) / mm_per_px);
                by0 = by0.min((y - min_y) / mm_per_px);
                by1 = by1.max((y - min_y) / mm_per_px);
            }
        }
        let x0 = bx0.floor().max(0.0) as usize;
        let y0 = by0.floor().max(0.0) as usize;
        let x1 = (bx1.ceil() as usize).min(out_w.saturating_sub(1));
        let y1 = (by1.ceil() as usize).min(out_h.saturating_sub(1));
        let w_i = Homography(w_chain[i]);
        let kf = &keyframes[i];
        let kc = wall_center[i];
        for oy in y0..=y1.min(out_h - 1) {
            let wy = min_y + (oy as f64 + 0.5) * mm_per_px;
            for ox in x0..=x1.min(out_w - 1) {
                let o = oy * out_w + ox;
                let wx = min_x + (ox as f64 + 0.5) * mm_per_px;
                let d2 = ((wx - kc[0]).powi(2) + (wy - kc[1]).powi(2)) as f32;
                if d2 >= best_d2[o] {
                    continue; // an already-rendered keyframe is closer
                }
                // The chain maps wall -> IDEAL px; the real pixels live in
                // the distorted image, so the sample position is bent back
                // through the calibrated lens (identity when uncalibrated).
                if let Some((ix, iy)) = w_i.apply(wx, wy) {
                    let [sx, sy] = dist.distort([ix, iy]);
                    if sx >= 0.0
                        && sy >= 0.0
                        && sx <= (width - 1) as f64
                        && sy <= (height - 1) as f64
                    {
                        let xf = sx.floor() as usize;
                        let yf = sy.floor() as usize;
                        let xc = (xf + 1).min(width - 1);
                        let yc = (yf + 1).min(height - 1);
                        let fx = sx - xf as f64;
                        let fy = sy - yf as f64;
                        for ch in 0..3 {
                            let g = |xx: usize, yy: usize| kf.rgba[(yy * width + xx) * 4 + ch] as f64;
                            let v = g(xf, yf) * (1.0 - fx) * (1.0 - fy)
                                + g(xc, yf) * fx * (1.0 - fy)
                                + g(xf, yc) * (1.0 - fx) * fy
                                + g(xc, yc) * fx * fy;
                            rgba_out[o * 4 + ch] = v.round().clamp(0.0, 255.0) as u8;
                        }
                        rgba_out[o * 4 + 3] = 255;
                        best_d2[o] = d2;
                    }
                }
            }
        }
    }
    drop(best_d2);
    // Unfilled pixels: same dark fill as the still path, so the Homeowner
    // sees the captured extent.
    for px in rgba_out.chunks_exact_mut(4) {
        if px[3] == 0 {
            px[0] = 24;
            px[1] = 24;
            px[2] = 28;
            px[3] = 255;
        }
    }

    // Far end for the bound scalars: Marker B's far edge when closure ran
    // (the far end of the candidate wall area per ADR-0002 — the markers
    // bracket it); otherwise the rendered x-edge farther from the anchor.
    let far_edge = if max_x.abs() > min_x.abs() { max_x } else { min_x };
    let far_x_mm = b_far_x.unwrap_or(far_edge);

    // Anchor extrapolation term: a 4-corner fit of a small marker has
    // perspective noise that grows ~quadratically with distance in units of
    // the marker side, and the loop closure constrains nothing in the
    // vertical direction. Use half the worst-corner quadratic factor over
    // the rendered vertical extent (the LS fit averages four corners);
    // measured on synthetic pans this matches the observed vertical error
    // at 3-4 marker-sides. Along the pan axis the closure's measured terms
    // take over, so this stays a constant floor rather than growing with x.
    let v_max = (min_y - side_mm / 2.0)
        .abs()
        .max((max_y - side_mm / 2.0).abs());
    let lever = ((v_max / side_mm).powi(2) / 2.0).max(1.0);
    let bound = BoundModel {
        sigma_a_mm: sigma_a_mm * lever,
        rel: rel_from_closure.unwrap_or(OPEN_LOOP_REL),
        closure_ramp,
        track: track_pts,
    };

    Ok(PanOutput {
        rgba: rgba_out,
        width: out_w,
        height: out_h,
        mm_per_px,
        origin_mm: [min_x, min_y],
        keyframes_used: n,
        truncated,
        links: link_quality,
        closure,
        closure_rejected,
        calibration,
        bound,
        far_x_mm,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_fit_recovers_a_known_transform() {
        let src = [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]];
        let (s_true, th_true, t_true) = (1.03, 0.02, [5.0, -3.0]);
        let (sn, cs) = (th_true as f64).sin_cos();
        let mut dst = [[0.0f64; 2]; 4];
        for i in 0..4 {
            dst[i] = [
                s_true * (cs * src[i][0] - sn * src[i][1]) + t_true[0],
                s_true * (sn * src[i][0] + cs * src[i][1]) + t_true[1],
            ];
        }
        let (s, th, t) = fit_similarity(&src, &dst);
        assert!((s - s_true).abs() < 1e-9);
        assert!((th - th_true).abs() < 1e-9);
        assert!((t[0] - t_true[0]).abs() < 1e-9 && (t[1] - t_true[1]).abs() < 1e-9);
    }

    #[test]
    fn bound_model_is_monotone_in_distance() {
        let b = BoundModel {
            sigma_a_mm: 0.8,
            rel: 0.003,
            closure_ramp: Some((3900.0, 1.5)),
            track: vec![(500.0, 1.0), (1500.0, 3.0), (3000.0, 6.0)],
        };
        let near = b.bound_at_mm(0.0);
        let mid = b.bound_at_mm(1800.0);
        let far = b.bound_at_mm(4000.0);
        assert!(near < mid && mid < far, "{near} {mid} {far}");
        assert!((near - K95 * 0.8f64).abs() < 1e-9);
    }

    /// Draw a few dark discs on a light background at a given x offset.
    fn textured_frame(w: usize, h: usize, offset: f64) -> Vec<u8> {
        let mut rgba = vec![0u8; w * h * 4];
        let dots = [
            [30.0, 30.0],
            [90.0, 80.0],
            [150.0, 40.0],
            [210.0, 90.0],
            [60.0, 100.0],
            [180.0, 20.0],
            [240.0, 60.0],
            [120.0, 70.0],
        ];
        for y in 0..h {
            for x in 0..w {
                let mut v = 200u8;
                for d in &dots {
                    // The scene scrolls left as the camera pans right.
                    let dx = x as f64 - (d[0] - offset).rem_euclid(300.0);
                    let dy = y as f64 - d[1];
                    if dx * dx + dy * dy < 36.0 {
                        v = 30;
                        break;
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

    #[test]
    fn losing_track_mid_recording_reports_tracking_lost() {
        let (w, h) = (160usize, 120usize);
        let mut core = PanCore::new(w, h);
        let mut offset = 0.0;
        for _ in 0..14 {
            core.push_frame(&textured_frame(w, h, offset));
            offset += 7.0;
        }
        assert!(core.keyframe_count() >= 1);
        // Lens covered / blank wall: flat frames are untrackable.
        let flat = vec![180u8; w * h * 4];
        for _ in 0..(LOST_TRACK_FRAMES + 2) {
            assert_eq!(core.push_frame(&flat), FrameDisposition::Skipped);
        }
        // Texture reappears; keyframes commit beyond the gap.
        for _ in 0..30 {
            core.push_frame(&textured_frame(w, h, offset));
            offset += 7.0;
        }
        match core.finish(1.0, true) {
            Err(PanError::TrackingLost { .. }) => {}
            Err(other) => panic!("expected TrackingLost, got {other:?}"),
            Ok(out) => panic!(
                "expected TrackingLost, got a {}x{} result",
                out.width, out.height
            ),
        }
    }

    #[test]
    fn tenengrad_ranks_sharp_above_blurred() {
        let (w, h) = (64usize, 64usize);
        let mut sharp = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                sharp[y * w + x] = if (x / 8 + y / 8) % 2 == 0 { 230 } else { 20 };
            }
        }
        // 3x3 box blur of the sharp pattern.
        let mut blurred = sharp.clone();
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let mut acc = 0u32;
                for dy in 0..3 {
                    for dx in 0..3 {
                        acc += sharp[(y + dy - 1) * w + (x + dx - 1)] as u32;
                    }
                }
                blurred[y * w + x] = (acc / 9) as u8;
            }
        }
        assert!(tenengrad(&sharp, w, h) > 1.5 * tenengrad(&blurred, w, h));
    }
}
