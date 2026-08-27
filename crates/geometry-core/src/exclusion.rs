//! Exclusion Zone computation (issue #8): Obstruction outlines -> buffered,
//! wall-clipped Exclusion Zones, all in metric wall coordinates.
//!
//! The Homeowner traces axis-aligned Obstruction rectangles on the confirmed
//! Rectified Wall Image; each Obstruction's TYPE maps to a compliance buffer
//! (600 mm around openable windows/doors per AS/NZS 5139, zero for purely
//! physical Obstructions — the mapping lives in the page's reviewable config,
//! `web/obstruction-types.js`, NOT here). This module owns the geometry: an
//! Exclusion Zone is the outline inflated by its buffer on all four sides,
//! then clipped to the confirmed Wall bounds — wall material stops at the
//! edges and at the Floor Line, so no zone can claim space outside the Wall.
//!
//! Coordinate frame: the wall plane in millimetres, y growing DOWNWARD
//! (matching the Rectified Wall Image), so the Floor Line is the wall
//! rectangle's LARGEST y (its `bottom`).
//!
//! # Overlapping zones stay separate — deliberately
//!
//! Two Obstructions whose buffered zones overlap are returned as two
//! separate rectangles, NOT unioned into a polygon. Every downstream
//! consumer (overlay rendering, the coming Clear Zone search) treats the
//! zone set as "a point is excluded iff it is inside ANY zone", for which
//! separate rectangles and their union are equivalent — and axis-aligned
//! rectangles keep both this module and its consumers trivially auditable.

/// An axis-aligned rectangle on the wall plane, millimetres, y down.
/// `right > left` and `bottom > top` for a rectangle with area; the helpers
/// below treat anything else as empty.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectMm {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl RectMm {
    pub fn new(left: f64, top: f64, right: f64, bottom: f64) -> Self {
        RectMm { left, top, right, bottom }
    }

    /// True when the rectangle has no area (degenerate or inverted).
    pub fn is_empty(&self) -> bool {
        !(self.right > self.left && self.bottom > self.top)
    }

    /// All four coordinates are finite numbers.
    pub fn is_finite(&self) -> bool {
        self.left.is_finite()
            && self.top.is_finite()
            && self.right.is_finite()
            && self.bottom.is_finite()
    }

    /// Grow (positive `by`) every side outward by `by` millimetres.
    fn inflate(&self, by: f64) -> RectMm {
        RectMm {
            left: self.left - by,
            top: self.top - by,
            right: self.right + by,
            bottom: self.bottom + by,
        }
    }

    /// Intersection with `other`; may be empty (checked by the caller).
    fn intersect(&self, other: &RectMm) -> RectMm {
        RectMm {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        }
    }
}

/// One Obstruction's Exclusion Zone: the outline inflated by `buffer_mm` on
/// all four sides, clipped to the Wall bounds (`wall.bottom` is the Floor
/// Line, so a buffer reaching past the floor is cut off there — exclusion
/// below the Floor Line is meaningless, nothing mounts under the floor).
///
/// Returns `None` when the inflated outline does not overlap the Wall at
/// all (an outline traced entirely outside the confirmed bounds — the UI
/// clamps outlines inside the Wall, so `None` means a caller bug, and the
/// WASM wrapper in lib.rs refuses the whole batch rather than silently
/// dropping a zone).
pub fn exclusion_zone(outline: &RectMm, buffer_mm: f64, wall: &RectMm) -> Option<RectMm> {
    let zone = outline.inflate(buffer_mm).intersect(wall);
    if zone.is_empty() {
        None
    } else {
        Some(zone)
    }
}

/// Batch form consumed by the WASM wrapper: one zone per Obstruction, same
/// order, so the page can keep outline / zone / type associated by index.
/// `outlines` and `buffers_mm` must be the same length. Overlapping zones
/// come back as-is (see the module docs on why there is no union step).
pub fn exclusion_zones(
    outlines: &[RectMm],
    buffers_mm: &[f64],
    wall: &RectMm,
) -> Option<Vec<RectMm>> {
    debug_assert_eq!(outlines.len(), buffers_mm.len());
    outlines
        .iter()
        .zip(buffers_mm.iter())
        .map(|(outline, &buffer)| exclusion_zone(outline, buffer, wall))
        .collect()
}

/// Flat-array form behind the WASM API (`exclusion_zones_mm` in lib.rs):
/// validates everything the JS side could get wrong and errors instead of
/// guessing. Lives here (returning a plain message, not a `JsError`) so the
/// validation is unit-testable on native targets — wasm-bindgen error types
/// cannot even be constructed off-wasm.
///
/// * `outlines_mm` — 4 values per Obstruction: `[left, top, right, bottom]`.
/// * `buffers_mm` — one buffer per Obstruction, finite and non-negative.
/// * `wall_mm` — `[left, top, right, floor]`, finite, with area.
///
/// Returns 4 values per zone in the same order as the input.
pub fn exclusion_zones_flat(
    outlines_mm: &[f64],
    buffers_mm: &[f64],
    wall_mm: &[f64],
) -> Result<Vec<f64>, &'static str> {
    if outlines_mm.len() % 4 != 0 || outlines_mm.len() != buffers_mm.len() * 4 {
        return Err("outlines_mm must hold 4 values per obstruction and match buffers_mm");
    }
    if wall_mm.len() != 4 {
        return Err("wall_mm must be [left, top, right, floor]");
    }
    let wall = RectMm::new(wall_mm[0], wall_mm[1], wall_mm[2], wall_mm[3]);
    if !wall.is_finite() || wall.is_empty() {
        return Err("wall bounds are degenerate or non-finite");
    }
    let outlines: Vec<RectMm> = outlines_mm
        .chunks_exact(4)
        .map(|r| RectMm::new(r[0], r[1], r[2], r[3]))
        .collect();
    if outlines.iter().any(|o| !o.is_finite() || o.is_empty()) {
        return Err("an obstruction outline is degenerate or non-finite");
    }
    if buffers_mm.iter().any(|b| !b.is_finite() || *b < 0.0) {
        return Err("buffers must be finite and non-negative");
    }
    let zones = exclusion_zones(&outlines, buffers_mm, &wall)
        .ok_or("an obstruction outline lies entirely outside the wall bounds")?;
    Ok(zones
        .iter()
        .flat_map(|z| [z.left, z.top, z.right, z.bottom])
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A generous wall so unclipped tests exercise pure inflation:
    /// 6000 mm wide, top at -500 mm, Floor Line at 2700 mm.
    fn wall() -> RectMm {
        RectMm::new(0.0, -500.0, 6000.0, 2700.0)
    }

    #[test]
    fn window_buffer_inflates_every_side_by_600mm() {
        let outline = RectMm::new(1000.0, 800.0, 1600.0, 2000.0);
        let zone = exclusion_zone(&outline, 600.0, &wall()).expect("inside the wall");
        assert_eq!(zone, RectMm::new(400.0, 200.0, 2200.0, 2600.0));
    }

    #[test]
    fn zero_buffer_zone_is_exactly_the_outline() {
        // Pipes and other purely physical Obstructions: the product must not
        // overlap them, but there is no standoff — zone == outline.
        let outline = RectMm::new(2500.0, 100.0, 2560.0, 2400.0);
        let zone = exclusion_zone(&outline, 0.0, &wall()).expect("inside the wall");
        assert_eq!(zone, outline);
    }

    #[test]
    fn overlapping_zones_stay_separate_rects() {
        // Two windows 400 mm apart: their 600 mm zones overlap by 800 mm.
        // The contract is one zone per Obstruction, same order, no union.
        let outlines = [
            RectMm::new(1000.0, 500.0, 1500.0, 1500.0),
            RectMm::new(1900.0, 500.0, 2400.0, 1500.0),
        ];
        let zones = exclusion_zones(&outlines, &[600.0, 600.0], &wall()).expect("both inside");
        assert_eq!(zones.len(), 2);
        assert_eq!(zones[0], RectMm::new(400.0, -100.0, 2100.0, 2100.0));
        assert_eq!(zones[1], RectMm::new(1300.0, -100.0, 3000.0, 2100.0));
        // They do overlap — and both are still reported whole.
        assert!(zones[0].right > zones[1].left);
    }

    #[test]
    fn zone_is_clipped_by_the_wall_edges() {
        // A door in the wall's top-left corner: the 600 mm buffer would
        // reach past the left edge and above the top edge — clipped to both.
        let outline = RectMm::new(100.0, -300.0, 1000.0, 1800.0);
        let zone = exclusion_zone(&outline, 600.0, &wall()).expect("inside the wall");
        assert_eq!(zone, RectMm::new(0.0, -500.0, 1600.0, 2400.0));
    }

    #[test]
    fn buffer_past_the_floor_line_is_cut_at_the_floor() {
        // A low window whose 600 mm buffer extends beyond the Floor Line
        // (wall bottom = 2700 mm): exclusion below the floor is meaningless,
        // so the zone stops exactly at it.
        let outline = RectMm::new(3000.0, 1900.0, 3900.0, 2500.0);
        let zone = exclusion_zone(&outline, 600.0, &wall()).expect("inside the wall");
        assert_eq!(zone.bottom, 2700.0, "zone must stop at the Floor Line");
        assert_eq!(zone, RectMm::new(2400.0, 1300.0, 4500.0, 2700.0));
    }

    #[test]
    fn outline_entirely_outside_the_wall_yields_none() {
        // The UI clamps outlines inside the Wall, so this is a caller bug —
        // it must surface as None, never as a silent empty rectangle.
        let outline = RectMm::new(7000.0, 100.0, 7500.0, 600.0);
        assert_eq!(exclusion_zone(&outline, 0.0, &wall()), None);
        // Even a large buffer that would reach back over the wall still
        // produces a real overlapping zone — only true non-overlap is None.
        let zone = exclusion_zone(&outline, 1200.0, &wall()).expect("buffer reaches the wall");
        assert_eq!(zone, RectMm::new(5800.0, -500.0, 6000.0, 1800.0));
    }

    #[test]
    fn batch_preserves_order_and_refuses_when_any_zone_is_empty() {
        let outlines = [
            RectMm::new(100.0, 100.0, 200.0, 200.0),
            RectMm::new(9000.0, 100.0, 9100.0, 200.0), // outside the wall
        ];
        assert_eq!(exclusion_zones(&outlines, &[0.0, 0.0], &wall()), None);
        let inside = [outlines[0]];
        let zones = exclusion_zones(&inside, &[50.0], &wall()).expect("inside");
        assert_eq!(zones, vec![RectMm::new(50.0, 50.0, 250.0, 250.0)]);
    }

    #[test]
    fn flat_form_computes_and_flattens_in_order() {
        // A window (600 mm buffer) and a pipe (0 mm) on a 6000×3200 mm wall:
        // the window's zone is clipped at the Floor Line (y = 2700).
        let outlines = [
            1000.0, 1900.0, 1600.0, 2500.0, // window, buffer runs past the floor
            2500.0, 100.0, 2560.0, 2400.0, // pipe
        ];
        let zones = exclusion_zones_flat(&outlines, &[600.0, 0.0], &[0.0, -500.0, 6000.0, 2700.0])
            .expect("valid input");
        assert_eq!(
            zones,
            vec![
                400.0, 1300.0, 2200.0, 2700.0, // clipped at the Floor Line
                2500.0, 100.0, 2560.0, 2400.0, // zero buffer: zone == outline
            ],
        );
        // No obstructions is a valid state (a blank wall), not an error.
        assert_eq!(
            exclusion_zones_flat(&[], &[], &[0.0, 0.0, 100.0, 100.0]).expect("empty ok"),
            Vec::<f64>::new(),
        );
    }

    #[test]
    fn flat_form_refuses_malformed_input() {
        let wall = [0.0, 0.0, 1000.0, 1000.0];
        let outline = [100.0, 100.0, 200.0, 200.0];
        // Length mismatch between outlines and buffers.
        assert!(exclusion_zones_flat(&outline, &[0.0, 0.0], &wall).is_err());
        assert!(exclusion_zones_flat(&outline[..3], &[0.0], &wall).is_err());
        // Wall must be [left, top, right, floor], finite, with area.
        assert!(exclusion_zones_flat(&outline, &[0.0], &[0.0, 0.0, 1000.0]).is_err());
        assert!(exclusion_zones_flat(&outline, &[0.0], &[0.0, 0.0, 0.0, 1000.0]).is_err());
        assert!(exclusion_zones_flat(&outline, &[0.0], &[f64::NAN, 0.0, 1000.0, 1000.0]).is_err());
        // Degenerate / non-finite outlines and negative buffers.
        assert!(exclusion_zones_flat(&[100.0, 100.0, 100.0, 200.0], &[0.0], &wall).is_err());
        assert!(exclusion_zones_flat(&[100.0, f64::NAN, 200.0, 200.0], &[0.0], &wall).is_err());
        assert!(exclusion_zones_flat(&outline, &[-1.0], &wall).is_err());
        // An outline entirely outside the wall is a caller bug, not a zone.
        assert!(exclusion_zones_flat(&[2000.0, 100.0, 2100.0, 200.0], &[0.0], &wall).is_err());
    }

    #[test]
    fn empty_and_finite_checks() {
        assert!(RectMm::new(0.0, 0.0, 0.0, 10.0).is_empty());
        assert!(RectMm::new(0.0, 0.0, 10.0, 0.0).is_empty());
        assert!(RectMm::new(10.0, 0.0, 0.0, 10.0).is_empty());
        assert!(!RectMm::new(0.0, 0.0, 1.0, 1.0).is_empty());
        assert!(!RectMm::new(0.0, f64::NAN, 1.0, 1.0).is_finite());
        assert!(!RectMm::new(f64::INFINITY, 0.0, 1.0, 1.0).is_finite());
    }
}
