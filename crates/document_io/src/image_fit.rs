// SPDX-License-Identifier: GPL-3.0-only
// SPDX-FileCopyrightText: 2026 Cyril Jacquet

//! Fitting an inline picture into the page's text area, shared by the DOCX and ODT writers.
//!
//! Both writers used to place a picture at its display size (the one the document gives it, or
//! the file's own pixel dimensions at 96 dpi), so a phone photo or a cover illustration came out
//! two or three times the size of the page and ran off its edge. Word and LibreOffice both fit a
//! picture to the margins when it is inserted; this is the same rule applied on the way out.
//!
//! The rule is deliberately narrow: shrink, proportionally, until the picture fits inside the
//! text area on **both** axes, and never enlarge. It applies to a size the document states too,
//! because a picture larger than the page is never what a writer meant, whereas a smaller one
//! may be exactly what they meant and is left alone.

/// Scale `(width, height)` down, keeping its aspect ratio, so that it fits inside
/// `(max_width, max_height)`. All four values share one unit, whichever the caller works in.
///
/// Returns the input unchanged when it already fits, when it has no area to scale, or when the
/// box itself has none: a text area of zero or negative width means margins wider than the page,
/// and there is no size a picture could be shrunk to that would honour that, so the caller's
/// geometry is left for the reader to resolve rather than collapsing the picture to nothing.
pub(crate) fn fit_within(width: f64, height: f64, max_width: f64, max_height: f64) -> (f64, f64) {
    if !(width > 0.0 && height > 0.0 && max_width > 0.0 && max_height > 0.0) {
        return (width, height);
    }
    let scale = (max_width / width).min(max_height / height);
    if scale >= 1.0 {
        return (width, height);
    }
    (width * scale, height * scale)
}

#[cfg(test)]
mod tests {
    use super::fit_within;

    #[test]
    fn a_picture_that_fits_is_left_alone() {
        assert_eq!(fit_within(100.0, 50.0, 400.0, 600.0), (100.0, 50.0));
    }

    #[test]
    fn a_too_wide_picture_shrinks_to_the_width() {
        let (w, h) = fit_within(800.0, 200.0, 400.0, 600.0);
        assert_eq!((w, h), (400.0, 100.0));
    }

    #[test]
    fn a_too_tall_picture_shrinks_to_the_height() {
        let (w, h) = fit_within(200.0, 1200.0, 400.0, 600.0);
        assert_eq!((w, h), (100.0, 600.0));
    }

    #[test]
    fn the_tighter_axis_decides() {
        // 1024x1536 into a 600x900 box: both axes overflow, by the same ratio here.
        let (w, h) = fit_within(1024.0, 1536.0, 600.0, 800.0);
        assert!((h - 800.0).abs() < 1e-9, "{h}");
        assert!((w - 1024.0 * 800.0 / 1536.0).abs() < 1e-9, "{w}");
    }

    #[test]
    fn a_degenerate_box_or_picture_is_returned_unchanged() {
        assert_eq!(fit_within(800.0, 200.0, 0.0, 600.0), (800.0, 200.0));
        assert_eq!(fit_within(800.0, 200.0, 400.0, -1.0), (800.0, 200.0));
        assert_eq!(fit_within(0.0, 200.0, 400.0, 600.0), (0.0, 200.0));
    }
}
