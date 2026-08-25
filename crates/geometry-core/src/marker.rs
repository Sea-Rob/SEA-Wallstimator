//! Reference Marker definitions: ArUco DICT_4X4_50 patterns and the nominal
//! print geometry of the two-page marker PDF (ADR-0002).
//!
//! Single source of truth for the marker dictionary: the PDF generator
//! (`crates/marker-pdf`) renders these patterns, and the detector (issue #3)
//! will match camera frames against the same words. The nominal ruler length
//! is also exported to the capture page, which divides the Homeowner's
//! measured length by it to obtain the session's print-scale correction
//! factor.
//!
//! Bit conventions match OpenCV's ArUco module, from whose predefined
//! `DICT_4X4_1000` table (of which `DICT_4X4_50` is the first 50 entries)
//! these words were transcribed: a word holds the 4x4 data cells row-major
//! from the top-left cell, most significant bit first, and a **1 bit is a
//! white cell**. The printed marker surrounds the data cells with a one-cell
//! black border, and the page keeps a white quiet zone of at least one cell
//! around that border (ArUco convention).

/// Data cells per side (the "4x4" in DICT_4X4_50).
pub const DATA_CELLS: usize = 4;

/// Cells per side of the printed black square: data cells plus the one-cell
/// black border on each side.
pub const CELLS_PER_SIDE: usize = DATA_CELLS + 2;

/// Minimum white quiet zone around the printed marker, in cells.
pub const QUIET_ZONE_CELLS: usize = 1;

/// Nominal printed side length of the marker's black square, in millimetres.
/// 150 mm on A4 leaves room for the quiet zone (one cell = 25 mm) on both
/// sides plus instructions and the ruler strip.
pub const MARKER_SIDE_MM: f64 = 150.0;

/// Nominal printed length of the print-scale verification ruler strip, in
/// millimetres. The Homeowner measures this strip; measured / nominal is the
/// session's print-scale correction factor.
pub const RULER_LENGTH_MM: f64 = 200.0;

/// Marker taped at the LEFT end of the wall area (page 1 of the PDF).
pub const LEFT_MARKER_ID: u16 = 0;

/// Marker taped at the RIGHT end of the wall area (page 2 of the PDF).
pub const RIGHT_MARKER_ID: u16 = 1;

/// DICT_4X4_50 words for the two Reference Markers, indexed by marker ID.
/// Encoding as per the module docs (row-major, MSB first, 1 = white).
const WORDS: [u16; 2] = [
    0xB532, // ID 0 — OpenCV DICT_4X4_1000 bytes {181, 50}
    0x0F9A, // ID 1 — OpenCV DICT_4X4_1000 bytes {15, 154}
];

/// Dictionary word for a marker ID, or `None` for IDs Wallstimator does not
/// use (only [`LEFT_MARKER_ID`] and [`RIGHT_MARKER_ID`] exist in a session).
pub fn marker_word(id: u16) -> Option<u16> {
    WORDS.get(id as usize).copied()
}

/// Rotate a 4x4 word 90 degrees counter-clockwise. The detector (issue #3)
/// must match markers in any of the four rotations.
pub fn rotate_word_ccw(word: u16) -> u16 {
    let bit = |r: usize, c: usize| (word >> (15 - (r * DATA_CELLS + c))) & 1;
    let mut out = 0u16;
    for r in 0..DATA_CELLS {
        for c in 0..DATA_CELLS {
            // new(r, c) = old(c, N-1-r)
            out = (out << 1) | bit(c, DATA_CELLS - 1 - r);
        }
    }
    out
}

/// Full printed cell grid ([`CELLS_PER_SIDE`]²) for a marker ID, including
/// the black border; `true` means a black (inked) cell. Row 0 is the top of
/// the printed marker. The quiet zone is *not* included — the renderer must
/// keep [`QUIET_ZONE_CELLS`] cells of white around this grid.
pub fn marker_cells(id: u16) -> Option<[[bool; CELLS_PER_SIDE]; CELLS_PER_SIDE]> {
    let word = marker_word(id)?;
    let mut cells = [[true; CELLS_PER_SIDE]; CELLS_PER_SIDE]; // border stays black
    for r in 0..DATA_CELLS {
        for c in 0..DATA_CELLS {
            let bit = (word >> (15 - (r * DATA_CELLS + c))) & 1;
            cells[r + 1][c + 1] = bit == 0; // 1 = white, so black when 0
        }
    }
    Some(cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OpenCV stores each marker as byte pairs, row-major, MSB first —
    /// identical bit order to our `u16` words.
    fn word_from_opencv_bytes(bytes: [u8; 2]) -> u16 {
        u16::from_be_bytes(bytes)
    }

    #[test]
    fn words_match_opencv_dict_4x4_50() {
        // First two rows of OpenCV's DICT_4X4_1000_BYTES table
        // (modules/objdetect/src/aruco/predefined_dictionaries.hpp),
        // canonical (first) rotation.
        assert_eq!(marker_word(LEFT_MARKER_ID), Some(word_from_opencv_bytes([181, 50])));
        assert_eq!(marker_word(RIGHT_MARKER_ID), Some(word_from_opencv_bytes([15, 154])));
        assert_eq!(marker_word(2), None, "only two Reference Markers exist");
    }

    #[test]
    fn rotations_match_opencv_rotation_entries() {
        // OpenCV lists each marker's four rotations; successive entries are
        // 90° counter-clockwise from the previous one.
        let expected: [(u16, [[u8; 2]; 3]); 2] = [
            (0, [[235, 72], [76, 173], [18, 215]]),
            (1, [[101, 71], [89, 240], [226, 166]]),
        ];
        for (id, rotations) in expected {
            let mut w = marker_word(id).unwrap();
            for bytes in rotations {
                w = rotate_word_ccw(w);
                assert_eq!(w, word_from_opencv_bytes(bytes), "marker {id} rotation mismatch");
            }
            assert_eq!(
                rotate_word_ccw(w),
                marker_word(id).unwrap(),
                "four rotations must return to the canonical word"
            );
        }
    }

    #[test]
    fn markers_are_distinct_under_all_rotations() {
        // The two Reference Markers disambiguate the LEFT and RIGHT wall
        // ends, so no rotation of one may look like a rotation of the other.
        let mut a = marker_word(LEFT_MARKER_ID).unwrap();
        for _ in 0..4 {
            let mut b = marker_word(RIGHT_MARKER_ID).unwrap();
            for _ in 0..4 {
                assert_ne!(a, b);
                b = rotate_word_ccw(b);
            }
            a = rotate_word_ccw(a);
        }
    }

    #[test]
    fn cells_have_black_border_and_word_interior() {
        for id in [LEFT_MARKER_ID, RIGHT_MARKER_ID] {
            let cells = marker_cells(id).unwrap();
            let word = marker_word(id).unwrap();
            for i in 0..CELLS_PER_SIDE {
                assert!(cells[0][i] && cells[CELLS_PER_SIDE - 1][i], "top/bottom border black");
                assert!(cells[i][0] && cells[i][CELLS_PER_SIDE - 1], "left/right border black");
            }
            for r in 0..DATA_CELLS {
                for c in 0..DATA_CELLS {
                    let white = (word >> (15 - (r * DATA_CELLS + c))) & 1 == 1;
                    assert_eq!(cells[r + 1][c + 1], !white, "marker {id} cell ({r},{c})");
                }
            }
        }
        assert!(marker_cells(2).is_none());
    }

    #[test]
    fn print_geometry_is_sane() {
        // Quiet zone per ArUco convention, and marker + quiet zone must fit
        // an A4 page width (210 mm).
        assert!(QUIET_ZONE_CELLS >= 1);
        let cell_mm = MARKER_SIDE_MM / CELLS_PER_SIDE as f64;
        let total = MARKER_SIDE_MM + 2.0 * cell_mm * QUIET_ZONE_CELLS as f64;
        assert!(total <= 210.0, "marker + quiet zone exceeds A4 width: {total} mm");
        assert!(RULER_LENGTH_MM >= 200.0);
        assert!(RULER_LENGTH_MM <= 210.0, "ruler strip must fit A4 width");
    }
}
