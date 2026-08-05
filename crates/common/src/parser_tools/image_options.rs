//! Image payloads supplied to an export.
//!
//! This crate never touches the filesystem — an inline image stores only a
//! `src` string, and resolving that string to bytes is the embedding
//! application's job. Exports that need to *embed* an image therefore receive
//! its bytes the same way the PDF exporter already receives fonts: handed over
//! by the caller, keyed by exactly the `src` the document carries.
//!
//! That indirection is deliberate. The alternative — having the exporter open
//! files named by the document — would make export depend on the caller's
//! working directory, would let a document reference paths outside the
//! project, and would make this crate's behaviour untestable without a
//! filesystem fixture.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One image's bytes, together with what they are.
///
/// `mime_type` is carried rather than sniffed because the caller already knows
/// it (it stored the image) and because container formats need it verbatim:
/// EPUB writes it into the OPF manifest, and getting it wrong produces a book
/// that validates but will not render.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ExportImage {
    pub bytes: Vec<u8>,
    pub mime_type: String,
}

impl ExportImage {
    pub fn new(bytes: impl Into<Vec<u8>>, mime_type: impl Into<String>) -> Self {
        Self {
            bytes: bytes.into(),
            mime_type: mime_type.into(),
        }
    }

    /// The conventional file extension for this image's media type, used when a
    /// container has to name the packaged file. Falls back to `bin` rather than
    /// guessing, so an unknown type is visible instead of silently mislabelled.
    pub fn extension(&self) -> &'static str {
        match self.mime_type.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            "image/gif" => "gif",
            "image/svg+xml" => "svg",
            _ => "bin",
        }
    }
}

/// Image bytes for one export, keyed by the `src` of the inline images that
/// reference them.
///
/// A `BTreeMap` (not a `HashMap`) so packaged filenames come out in a stable
/// order: an EPUB or DOCX built twice from the same document must be
/// byte-comparable, which a randomised iteration order quietly prevents.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ExportImages(BTreeMap<String, ExportImage>);

impl ExportImages {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register bytes for the image referenced by `src`.
    pub fn insert(&mut self, src: impl Into<String>, image: ExportImage) -> &mut Self {
        self.0.insert(src.into(), image);
        self
    }

    pub fn get(&self, src: &str) -> Option<&ExportImage> {
        self.0.get(src)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &ExportImage)> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl<S: Into<String>> FromIterator<(S, ExportImage)> for ExportImages {
    fn from_iter<I: IntoIterator<Item = (S, ExportImage)>>(iter: I) -> Self {
        Self(iter.into_iter().map(|(s, i)| (s.into(), i)).collect())
    }
}

/// How an HTML export represents an inline image.
///
/// HTML is the one text export where the choice is genuinely open: the output
/// is a single string, but an `<img>` can either point at a file the caller
/// will write beside it or carry the bytes inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HtmlImageMode {
    /// Emit `src` exactly as the document stores it and embed nothing.
    ///
    /// The default, because it is the only mode that cannot silently inflate
    /// the output: resolving the reference is then the caller's business, which
    /// is also what makes a sidecar-assets layout possible.
    #[default]
    Reference,
    /// Inline the bytes as a `data:` URI, producing a self-contained file.
    ///
    /// Base64 costs about a third more than the raw bytes, so a document with
    /// large photographs produces a correspondingly large `.html`.
    DataUri,
    /// Drop images entirely, keeping their alt text as the accessible fallback.
    Omit,
}

/// Options for an HTML export.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HtmlExportOptions {
    pub image_mode: HtmlImageMode,
    /// Bytes for [`HtmlImageMode::DataUri`]. Unused in the other modes.
    #[serde(default)]
    pub images: ExportImages,
}

/// Base64 (standard alphabet, padded) for `data:` URIs.
pub fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_test_vectors() {
        // RFC 4648 §10.
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(input.as_bytes()), expected, "{input:?}");
        }
    }

    #[test]
    fn base64_handles_bytes_above_ascii() {
        assert_eq!(base64_encode(&[0xff, 0xfe, 0xfd]), "//79");
        assert_eq!(base64_encode(&[0x00, 0x00, 0x00]), "AAAA");
    }

    #[test]
    fn extension_falls_back_visibly_for_unknown_types() {
        assert_eq!(ExportImage::new(vec![], "image/png").extension(), "png");
        assert_eq!(ExportImage::new(vec![], "image/jpeg").extension(), "jpg");
        assert_eq!(
            ExportImage::new(vec![], "application/x-thing").extension(),
            "bin"
        );
    }

    #[test]
    fn images_iterate_in_a_stable_order() {
        // Packaged filenames are derived from iteration order, so two exports of
        // the same document must agree.
        let build = || {
            ExportImages::from_iter([
                ("z.png", ExportImage::new(vec![1], "image/png")),
                ("a.png", ExportImage::new(vec![2], "image/png")),
                ("m.png", ExportImage::new(vec![3], "image/png")),
            ])
        };
        let first: Vec<String> = build().iter().map(|(k, _)| k.clone()).collect();
        let second: Vec<String> = build().iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(first, second);
        assert_eq!(first, vec!["a.png", "m.png", "z.png"]);
    }
}
