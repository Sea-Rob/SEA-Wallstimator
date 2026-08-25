//! Reference Marker PDF generator (ADR-0002).
//!
//! Emits a deterministic two-page A4 PDF — one ArUco Reference Marker per
//! page (LEFT / RIGHT end of the wall area) plus a 200 mm ruler strip the
//! Homeowner measures to self-verify print scale. Marker patterns and all
//! nominal dimensions come from `geometry-core::marker` (single source of
//! truth shared with the future in-browser detector).
//!
//! The writer is a minimal hand-rolled PDF 1.4 emitter: vector rectangles
//! for marker cells and ruler ticks, standard Helvetica for text, no
//! compression, no timestamps — so the output is byte-for-byte reproducible
//! and the generated file is committed at `web/reference-marker.pdf`.
//! CI regenerates it and fails if the committed copy drifts from the code.

use std::fmt::Write as _;

use geometry_core::marker::{
    marker_cells, CELLS_PER_SIDE, LEFT_MARKER_ID, MARKER_SIDE_MM, QUIET_ZONE_CELLS,
    RIGHT_MARKER_ID, RULER_LENGTH_MM,
};

/// A4 portrait, millimetres.
const PAGE_W_MM: f64 = 210.0;
const PAGE_H_MM: f64 = 297.0;

fn mm_to_pt(mm: f64) -> f64 {
    mm * 72.0 / 25.4
}

/// Page layout, all in millimetres, vertical positions measured from the
/// page TOP (converted to PDF's bottom-left origin only at emission time).
struct Layout {
    /// Side of one marker cell.
    cell_mm: f64,
    /// White quiet zone kept clear around the marker's black square.
    quiet_mm: f64,
    /// Left edge of the marker's black square (horizontally centred).
    marker_x_mm: f64,
    /// Top edge of the marker's black square.
    marker_top_mm: f64,
    /// Left edge of the ruler strip (position of the 0 mm mark).
    ruler_x_mm: f64,
    /// Top edge of the ruler baseline band.
    ruler_top_mm: f64,
}

fn layout() -> Layout {
    let cell_mm = MARKER_SIDE_MM / CELLS_PER_SIDE as f64;
    Layout {
        cell_mm,
        quiet_mm: cell_mm * QUIET_ZONE_CELLS as f64,
        marker_x_mm: (PAGE_W_MM - MARKER_SIDE_MM) / 2.0,
        marker_top_mm: 97.0,
        ruler_x_mm: (PAGE_W_MM - RULER_LENGTH_MM) / 2.0,
        ruler_top_mm: 56.0,
    }
}

/// Content-stream builder. Only two primitives are needed: filled
/// rectangles and single-line Helvetica text.
struct Content {
    ops: String,
}

impl Content {
    fn new() -> Self {
        Content { ops: String::new() }
    }

    /// Filled rectangle; `y_top_mm` is the rectangle's top edge measured
    /// from the top of the page. `gray` 0.0 = black, 1.0 = white.
    fn fill_rect(&mut self, x_mm: f64, y_top_mm: f64, w_mm: f64, h_mm: f64, gray: f64) {
        writeln!(
            self.ops,
            "{gray:.3} g\n{:.2} {:.2} {:.2} {:.2} re f",
            mm_to_pt(x_mm),
            mm_to_pt(PAGE_H_MM - y_top_mm - h_mm),
            mm_to_pt(w_mm),
            mm_to_pt(h_mm),
        )
        .unwrap();
    }

    /// Single line of text; `baseline_top_mm` is the text baseline measured
    /// from the top of the page.
    fn text(&mut self, x_mm: f64, baseline_top_mm: f64, size_pt: f64, bold: bool, s: &str) {
        let font = if bold { "F2" } else { "F1" };
        let escaped: String = s
            .chars()
            .flat_map(|c| match c {
                '(' | ')' | '\\' => vec!['\\', c],
                _ => vec![c],
            })
            .collect();
        writeln!(
            self.ops,
            "BT /{font} {size_pt:.1} Tf 0 g {:.2} {:.2} Td ({escaped}) Tj ET",
            mm_to_pt(x_mm),
            mm_to_pt(PAGE_H_MM - baseline_top_mm),
        )
        .unwrap();
    }

    /// Digits-only label centred on `x_center_mm`. Exact for Helvetica:
    /// every digit has width 556/1000 em.
    fn digits_centered(&mut self, x_center_mm: f64, baseline_top_mm: f64, size_pt: f64, s: &str) {
        debug_assert!(s.chars().all(|c| c.is_ascii_digit()));
        let width_mm = 0.556 * size_pt * s.len() as f64 * 25.4 / 72.0;
        self.text(x_center_mm - width_mm / 2.0, baseline_top_mm, size_pt, false, s);
    }
}

/// Draw one full page: instructions, ruler strip, marker with quiet zone.
fn draw_page(page_no: usize, marker_id: u16, letter: char, wall_end: &str) -> String {
    let l = layout();
    let mut c = Content::new();
    let margin = 15.0;

    // --- Instructions (kept above the ruler; nothing may enter the quiet zone).
    c.text(margin, 18.0, 14.0, true, &format!("Wallstimator Reference Marker - Page {page_no} of 2"));
    c.text(
        margin,
        26.0,
        11.0,
        true,
        &format!("Marker {letter} (ArUco 4x4_50, ID {marker_id}) - tape at the {wall_end} end of the wall area"),
    );
    c.text(
        margin,
        34.0,
        11.0,
        true,
        "PRINT AT 100% (ACTUAL SIZE). Do not use 'fit to page' or 'shrink to fit'.",
    );
    c.text(
        margin,
        41.0,
        10.0,
        false,
        "After printing, the app asks you to measure the ruler strip below with a tape",
    );
    c.text(
        margin,
        46.0,
        10.0,
        false,
        "measure and corrects for your printer's scaling. Tape the page flat - no folds.",
    );

    // --- Ruler strip: baseline band + cm ticks + mm labels.
    let baseline_h = 0.5;
    c.fill_rect(l.ruler_x_mm, l.ruler_top_mm, RULER_LENGTH_MM, baseline_h, 0.0);
    let tick_w = 0.35;
    let mut pos = 0u32;
    while pos as f64 <= RULER_LENGTH_MM {
        let x_center = l.ruler_x_mm + pos as f64;
        // Major ticks every 5 cm and at both ends; minor every 1 cm.
        let tick_top = if pos % 50 == 0 { l.ruler_top_mm - 6.0 } else { l.ruler_top_mm - 4.0 };
        c.fill_rect(x_center - tick_w / 2.0, tick_top, tick_w, l.ruler_top_mm - tick_top, 0.0);
        c.digits_centered(x_center, l.ruler_top_mm + 4.5, 6.0, &pos.to_string());
        pos += 10;
    }
    c.text(
        l.ruler_x_mm,
        l.ruler_top_mm + 10.0,
        8.0,
        false,
        &format!(
            "Ruler strip (millimetres) - end to end it should measure exactly {:.0} mm at 100% scale.",
            RULER_LENGTH_MM
        ),
    );

    // --- Reference Marker: black square, then white cells punched out.
    // The quiet zone (l.quiet_mm on every side) is simply left unpainted.
    let cells = marker_cells(marker_id).expect("marker ID must exist in the dictionary");
    c.fill_rect(l.marker_x_mm, l.marker_top_mm, MARKER_SIDE_MM, MARKER_SIDE_MM, 0.0);
    for (r, row) in cells.iter().enumerate() {
        for (col, &black) in row.iter().enumerate() {
            if !black {
                c.fill_rect(
                    l.marker_x_mm + col as f64 * l.cell_mm,
                    l.marker_top_mm + r as f64 * l.cell_mm,
                    l.cell_mm,
                    l.cell_mm,
                    1.0,
                );
            }
        }
    }

    // --- Footer, below the quiet zone.
    c.text(
        margin,
        l.marker_top_mm + MARKER_SIDE_MM + l.quiet_mm + 9.0,
        8.0,
        false,
        &format!(
            "Keep the white margin around the black square clear. Marker side {:.0} mm nominal.",
            MARKER_SIDE_MM
        ),
    );

    c.ops
}

/// Assemble the final PDF: fixed object numbering, uncompressed streams,
/// correct xref offsets.
fn build_pdf(page_streams: &[String]) -> Vec<u8> {
    let media_box = format!("[0 0 {:.2} {:.2}]", mm_to_pt(PAGE_W_MM), mm_to_pt(PAGE_H_MM));
    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            page_streams
                .iter()
                .enumerate()
                .map(|(i, _)| format!("{} 0 R", 5 + 2 * i))
                .collect::<Vec<_>>()
                .join(" "),
            page_streams.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".to_string(),
    ];
    for (i, stream) in page_streams.iter().enumerate() {
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox {media_box} \
             /Resources << /Font << /F1 3 0 R /F2 4 0 R >> >> /Contents {} 0 R >>",
            6 + 2 * i
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{stream}endstream",
            stream.len()
        ));
    }

    let mut out = b"%PDF-1.4\n%\xB5\xB2\xB5\xB2\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref_at = out.len();
    let mut xref = format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for off in &offsets {
        write!(xref, "{off:010} 00000 n \n").unwrap();
    }
    write!(
        xref,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
        objects.len() + 1
    )
    .unwrap();
    out.extend_from_slice(xref.as_bytes());
    out
}

/// The complete two-page Reference Marker PDF.
pub fn generate_pdf() -> Vec<u8> {
    build_pdf(&[
        draw_page(1, LEFT_MARKER_ID, 'A', "LEFT"),
        draw_page(2, RIGHT_MARKER_ID, 'B', "RIGHT"),
    ])
}

fn main() {
    let default_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/reference-marker.pdf");
    let path = std::env::args().nth(1).unwrap_or_else(|| default_path.to_string());
    let bytes = generate_pdf();
    std::fs::write(&path, &bytes)
        .unwrap_or_else(|e| panic!("failed to write {path}: {e}"));
    eprintln!("wrote {path} ({} bytes, 2 pages)", bytes.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_deterministic() {
        assert_eq!(generate_pdf(), generate_pdf(), "PDF must be byte-for-byte reproducible");
    }

    #[test]
    fn pdf_has_two_pages_and_valid_framing() {
        let bytes = generate_pdf();
        let text = String::from_utf8_lossy(&bytes);
        assert!(bytes.starts_with(b"%PDF-1.4\n"));
        assert!(text.ends_with("%%EOF\n"));
        assert!(text.contains("/Count 2"));
        assert_eq!(text.matches("/Type /Page ").count(), 2);
        // A4 media box in points.
        assert!(text.contains("[0 0 595.28 841.89]"));
    }

    #[test]
    fn xref_offsets_point_at_their_objects() {
        // Byte offsets, so work on bytes: the binary header line is not UTF-8.
        let bytes = generate_pdf();
        let xref_at = {
            let needle = b"startxref\n";
            let at = bytes
                .windows(needle.len())
                .rposition(|w| w == needle)
                .expect("startxref keyword");
            let tail = std::str::from_utf8(&bytes[at + needle.len()..]).unwrap();
            tail.split_whitespace().next().unwrap().parse::<usize>().unwrap()
        };
        assert!(bytes[xref_at..].starts_with(b"xref\n"));
        // The xref section itself is pure ASCII. Skip "xref", the subsection
        // header, and the object-0 free entry.
        let xref = std::str::from_utf8(&bytes[xref_at..]).unwrap();
        for (i, line) in xref.lines().skip(3).enumerate() {
            if line.starts_with("trailer") {
                break;
            }
            let off: usize = line[..10].parse().expect("xref entry offset");
            let expected = format!("{} 0 obj", i + 1);
            assert!(
                bytes[off..].starts_with(expected.as_bytes()),
                "xref entry {} does not point at {expected:?}",
                i + 1,
            );
        }
    }

    #[test]
    fn both_marker_ids_and_wall_ends_appear() {
        let text = String::from_utf8_lossy(&generate_pdf()).into_owned();
        assert!(text.contains("ID 0"));
        assert!(text.contains("ID 1"));
        assert!(text.contains("LEFT end of the wall area"));
        assert!(text.contains("RIGHT end of the wall area"));
        assert!(text.contains("PRINT AT 100%"));
    }

    #[test]
    fn quiet_zone_is_at_least_one_cell_and_marker_fits_page() {
        let l = layout();
        assert!(l.quiet_mm >= l.cell_mm * QUIET_ZONE_CELLS as f64);
        assert!(l.marker_x_mm - l.quiet_mm >= 0.0, "quiet zone must fit left of marker");
        assert!(
            l.marker_x_mm + MARKER_SIDE_MM + l.quiet_mm <= PAGE_W_MM,
            "quiet zone must fit right of marker"
        );
        // Ruler labels/caption end above the quiet zone; footer starts below it.
        assert!(l.ruler_top_mm + 10.0 < l.marker_top_mm - l.quiet_mm);
        assert!(l.marker_top_mm + MARKER_SIDE_MM + l.quiet_mm < PAGE_H_MM);
        // The ruler strip fits the page at full nominal length.
        assert!(l.ruler_x_mm >= 0.0 && l.ruler_x_mm + RULER_LENGTH_MM <= PAGE_W_MM);
    }

    #[test]
    fn white_cell_count_matches_dictionary_words() {
        // Each page paints one black square then one white rect per 1-bit.
        let ones_left = geometry_core::marker::marker_word(LEFT_MARKER_ID).unwrap().count_ones();
        let ones_right = geometry_core::marker::marker_word(RIGHT_MARKER_ID).unwrap().count_ones();
        let left = draw_page(1, LEFT_MARKER_ID, 'A', "LEFT");
        let right = draw_page(2, RIGHT_MARKER_ID, 'B', "RIGHT");
        assert_eq!(left.matches("1.000 g").count() as u32, ones_left);
        assert_eq!(right.matches("1.000 g").count() as u32, ones_right);
    }
}
