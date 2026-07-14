//! Paginate a single tall PDF page into fixed-height pages.
//!
//! A webview whole-page capture (macOS `WKWebView.createPDF`, design 4.5)
//! produces ONE page as tall as the whole document, which is awkward to read or
//! print. This module windows that page into normal fixed-height pages: every
//! output page references the SAME content stream and resources as the original
//! but carries its own `MediaBox` exposing just one horizontal band. Nothing is
//! re-rasterized, so text stays selectable and vector art stays sharp.
//!
//! The transform is pure bytes-in/bytes-out and has no platform dependency, so
//! it lives in the core and is unit-tested against a synthetic tall page.

use std::fmt;
use std::io::Cursor;

use lopdf::{Document, Object};

/// Why pagination failed. All variants carry a human-readable detail.
#[derive(Debug)]
pub enum PdfError {
    /// The input could not be parsed as a PDF.
    Parse(String),
    /// The PDF parsed but lacked a structure this code needs (pages, MediaBox).
    Structure(String),
    /// The paginated PDF could not be serialized.
    Write(String),
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PdfError::Parse(m) => write!(f, "could not parse PDF: {m}"),
            PdfError::Structure(m) => write!(f, "unexpected PDF structure: {m}"),
            PdfError::Write(m) => write!(f, "could not write PDF: {m}"),
        }
    }
}

impl std::error::Error for PdfError {}

/// Slice a single-page PDF into pages `page_height_pt` points tall.
///
/// The page WIDTH is inherited from the source capture, so content is never
/// clipped horizontally; only the height is paginated. `page_height_pt` is in
/// PDF points (72pt = 1in; US Letter is 792, A4 about 842).
///
/// A source taller than one page is split into uniform full-height pages (the
/// last page runs past the content into blank space, so every page is the same
/// size and prints one-to-one). A source no taller than one page is returned as
/// a single page kept snug to its content, not padded to a full empty page.
///
/// `background`, if given as `[r, g, b]` components in 0.0..=1.0, is painted
/// behind every page. This matters on the last page, whose window runs past the
/// content into space the capture never painted: without a fill that shows as
/// white, which clashes with a dark surface. Passing the rendered surface color
/// makes the trailing space match the document.
pub fn paginate(
    single_page_pdf: &[u8],
    page_height_pt: f32,
    background: Option<[f32; 3]>,
) -> Result<Vec<u8>, PdfError> {
    if !page_height_pt.is_finite() || page_height_pt <= 0.0 {
        return Err(PdfError::Structure(
            "page height must be a positive number of points".to_string(),
        ));
    }

    let mut doc =
        Document::load_mem(single_page_pdf).map_err(|e| PdfError::Parse(e.to_string()))?;

    // A capture is always one page; window the first page if there are several.
    let page_id = *doc
        .get_pages()
        .values()
        .next()
        .ok_or_else(|| PdfError::Structure("PDF has no pages".to_string()))?;

    let page_dict = doc
        .get_dictionary(page_id)
        .map_err(|e| PdfError::Structure(e.to_string()))?
        .clone();

    let [x0, y0, x1, y1] = media_box(&doc, &page_dict, page_id)?;
    let height = y1 - y0;

    // The page's existing content, reused by every windowed page. A background
    // fill is prepended before it (drawn underneath) when a color is given.
    let original_contents = page_dict.get(b"Contents").ok().cloned();

    let pages_id = doc
        .catalog()
        .and_then(|c| c.get(b"Pages"))
        .and_then(|p| p.as_reference())
        .map_err(|e| PdfError::Structure(format!("no page tree root: {e}")))?;

    // Number of bands, from the top of the content down.
    let count = ((height / page_height_pt).ceil() as i64).max(1);

    let mut kids = Vec::with_capacity(count as usize);
    for i in 0..count {
        // PDF space is y-up: y1 is the top of the content, y0 the bottom.
        let (lower, upper) = if count == 1 {
            // The whole document fits one page: keep it snug to the content so a
            // short document is not padded out to a full empty page.
            (y0, y1)
        } else {
            // Uniform full-height pages. The last page's lower edge runs past the
            // content bottom into blank space, so every page is the same size (a
            // normal document look) rather than ending on a thin sliver.
            (
                y1 - ((i + 1) as f32) * page_height_pt,
                y1 - (i as f32) * page_height_pt,
            )
        };

        let mut d = page_dict.clone();
        d.set(
            b"MediaBox".to_vec(),
            Object::Array(vec![
                Object::Real(x0),
                Object::Real(lower),
                Object::Real(x1),
                Object::Real(upper),
            ]),
        );
        // A CropBox would shadow our MediaBox window; annotations would repeat on
        // every page. Drop both so each page shows exactly its band.
        d.remove(b"CropBox");
        d.remove(b"Annots");
        d.set(b"Parent".to_vec(), Object::Reference(pages_id));

        // Paint the surface color under the page, then the original content.
        if let (Some(rgb), Some(contents)) = (background, original_contents.clone()) {
            let fill_id = doc.add_object(background_fill([x0, lower, x1, upper], rgb));
            let mut streams = vec![Object::Reference(fill_id)];
            match contents {
                Object::Array(items) => streams.extend(items),
                single => streams.push(single),
            }
            d.set(b"Contents".to_vec(), Object::Array(streams));
        }

        let id = doc.add_object(Object::Dictionary(d));
        kids.push(Object::Reference(id));
    }

    {
        let pages_dict = doc
            .objects
            .get_mut(&pages_id)
            .ok_or_else(|| PdfError::Structure("page tree root object missing".to_string()))?
            .as_dict_mut()
            .map_err(|e| PdfError::Structure(e.to_string()))?;
        pages_dict.set(b"Kids".to_vec(), Object::Array(kids));
        pages_dict.set(b"Count".to_vec(), Object::Integer(count));
        // The tall box must not be inherited by the new windowed pages.
        pages_dict.remove(b"MediaBox");
    }

    // The original tall page is now unreferenced; drop it (its shared Contents
    // and Resources objects stay, referenced by every windowed page).
    doc.objects.remove(&page_id);

    let mut buf = Cursor::new(Vec::new());
    doc.save_to(&mut buf)
        .map_err(|e| PdfError::Write(e.to_string()))?;
    Ok(buf.into_inner())
}

/// Read a page's MediaBox, following `/Parent` for an inherited box if the page
/// does not carry its own. Returns `[x0, y0, x1, y1]` in points.
fn media_box(
    doc: &Document,
    page_dict: &lopdf::Dictionary,
    page_id: (u32, u16),
) -> Result<[f32; 4], PdfError> {
    if let Ok(mb) = page_dict.get(b"MediaBox") {
        return parse_rect(mb);
    }
    let mut next = page_dict
        .get(b"Parent")
        .ok()
        .and_then(|p| p.as_reference().ok());
    let mut guard = 0;
    while let Some(pid) = next {
        // Cheap cycle guard: a malformed parent chain must not loop forever.
        guard += 1;
        if guard > 64 || pid == page_id {
            break;
        }
        let d = doc
            .get_dictionary(pid)
            .map_err(|e| PdfError::Structure(e.to_string()))?;
        if let Ok(mb) = d.get(b"MediaBox") {
            return parse_rect(mb);
        }
        next = d.get(b"Parent").ok().and_then(|p| p.as_reference().ok());
    }
    Err(PdfError::Structure(
        "page has no MediaBox (checked inherited)".to_string(),
    ))
}

/// Parse a 4-element PDF rectangle into `[x0, y0, x1, y1]`, accepting integer or
/// real components.
fn parse_rect(obj: &Object) -> Result<[f32; 4], PdfError> {
    let arr = obj
        .as_array()
        .map_err(|e| PdfError::Structure(format!("MediaBox is not an array: {e}")))?;
    if arr.len() != 4 {
        return Err(PdfError::Structure(format!(
            "MediaBox has {} elements, expected 4",
            arr.len()
        )));
    }
    let mut r = [0.0f32; 4];
    for (slot, o) in r.iter_mut().zip(arr) {
        *slot = o
            .as_float()
            .map_err(|e| PdfError::Structure(format!("MediaBox element not a number: {e}")))?;
    }
    Ok(r)
}

/// A content stream that fills the rectangle `[x0, y0, x1, y1]` with `rgb`
/// (components 0.0..=1.0). Wrapped in `q`/`Q` so it leaves the graphics state
/// untouched for the content drawn after it.
fn background_fill([x0, y0, x1, y1]: [f32; 4], [r, g, b]: [f32; 3]) -> lopdf::Stream {
    let (w, h) = (x1 - x0, y1 - y0);
    let ops = format!("q {r} {g} {b} rg {x0} {y0} {w} {h} re f Q");
    lopdf::Stream::new(lopdf::Dictionary::new(), ops.into_bytes())
}

/// Parse a CSS `rgb(...)`/`rgba(...)` color (as `getComputedStyle` reports it)
/// into `[r, g, b]` components in 0.0..=1.0. Accepts comma- or space-separated
/// values and ignores any alpha. Returns `None` for anything else (e.g. a named
/// color or `transparent`), so the caller simply skips the background fill.
pub fn parse_css_color(s: &str) -> Option<[f32; 3]> {
    let s = s.trim();
    let inner = s
        .strip_prefix("rgba(")
        .or_else(|| s.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let parts: Vec<f32> = inner
        .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.parse::<f32>().ok())
        .collect();
    if parts.len() < 3 {
        return None;
    }
    Some([parts[0] / 255.0, parts[1] / 255.0, parts[2] / 255.0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Stream};

    /// Build a minimal valid one-page PDF `width` x `height` points, with a
    /// trivial content stream and a font resource, mirroring the shape of a
    /// webview capture (page -> shared Contents + Resources).
    fn tall_page(width: f32, height: f32) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let content = Stream::new(dictionary! {}, b"BT /F1 12 Tf 20 20 Td (hi) Tj ET".to_vec());
        let content_id = doc.add_object(content);
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => dictionary! {
                    "Type" => "Font",
                    "Subtype" => "Type1",
                    "BaseFont" => "Helvetica",
                },
            },
        });
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(width), Object::Real(height)],
        });
        let pages = dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(width), Object::Real(height)],
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        let mut buf = Cursor::new(Vec::new());
        doc.save_to(&mut buf).unwrap();
        buf.into_inner()
    }

    /// Read back every page's MediaBox from paginated output, top page first.
    fn media_boxes(pdf: &[u8]) -> Vec<[f32; 4]> {
        let doc = Document::load_mem(pdf).unwrap();
        doc.get_pages()
            .values()
            .map(|&id| {
                let d = doc.get_dictionary(id).unwrap();
                parse_rect(d.get(b"MediaBox").unwrap()).unwrap()
            })
            .collect()
    }

    #[test]
    fn splits_tall_page_into_letter_bands() {
        let src = tall_page(612.0, 2000.0);
        let out = paginate(&src, 792.0, None).unwrap();
        let boxes = media_boxes(&out);
        // ceil(2000 / 792) = 3 pages.
        assert_eq!(boxes.len(), 3);
        // Page 1 is the top band: [0, 1208, 612, 2000].
        assert_eq!(boxes[0], [0.0, 2000.0 - 792.0, 612.0, 2000.0]);
        // Page 2 sits directly below it.
        assert_eq!(boxes[1], [0.0, 2000.0 - 1584.0, 612.0, 2000.0 - 792.0]);
        // The last page runs a full page tall past the content bottom (into blank
        // space) so every page is the same size, not a thin sliver.
        assert_eq!(boxes[2], [0.0, 2000.0 - 2376.0, 612.0, 2000.0 - 1584.0]);
        // Every page is a uniform full Letter page (792pt tall, 612pt wide).
        assert!(boxes
            .iter()
            .all(|b| (b[2] - b[0] - 612.0).abs() < 0.01 && (b[3] - b[1] - 792.0).abs() < 0.01));
    }

    #[test]
    fn short_page_stays_single() {
        let src = tall_page(612.0, 500.0);
        let out = paginate(&src, 792.0, None).unwrap();
        let boxes = media_boxes(&out);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0], [0.0, 0.0, 612.0, 500.0]);
    }

    #[test]
    fn exact_multiple_has_no_trailing_blank_page() {
        let src = tall_page(612.0, 1584.0); // exactly 2 * 792
        let out = paginate(&src, 792.0, None).unwrap();
        assert_eq!(media_boxes(&out).len(), 2);
    }

    #[test]
    fn windowed_pages_share_one_content_stream() {
        // Windowing must not duplicate the (potentially large) content; every
        // page should reference the same Contents object.
        let src = tall_page(612.0, 2000.0);
        let out = paginate(&src, 792.0, None).unwrap();
        let doc = Document::load_mem(&out).unwrap();
        let contents: Vec<_> = doc
            .get_pages()
            .values()
            .map(|&id| {
                doc.get_dictionary(id)
                    .unwrap()
                    .get(b"Contents")
                    .unwrap()
                    .as_reference()
                    .unwrap()
            })
            .collect();
        assert_eq!(contents.len(), 3);
        assert!(contents.iter().all(|&c| c == contents[0]));
    }

    #[test]
    fn rejects_non_pdf_input() {
        assert!(matches!(
            paginate(b"not a pdf at all", 792.0, None),
            Err(PdfError::Parse(_))
        ));
    }

    #[test]
    fn rejects_non_positive_page_height() {
        let src = tall_page(612.0, 2000.0);
        assert!(matches!(
            paginate(&src, 0.0, None),
            Err(PdfError::Structure(_))
        ));
        assert!(matches!(
            paginate(&src, -10.0, None),
            Err(PdfError::Structure(_))
        ));
        assert!(matches!(
            paginate(&src, f32::NAN, None),
            Err(PdfError::Structure(_))
        ));
    }

    /// A one-page PDF whose page inherits its MediaBox from the Pages node rather
    /// than carrying its own, exercising the inherited-box lookup.
    fn page_with_inherited_media_box(width: f32, height: f32) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let content_id = doc.add_object(Stream::new(dictionary! {}, b"BT ET".to_vec()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            // Deliberately no MediaBox here: it must be inherited from Pages.
        });
        let pages = dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(width), Object::Real(height)],
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        let mut buf = Cursor::new(Vec::new());
        doc.save_to(&mut buf).unwrap();
        buf.into_inner()
    }

    #[test]
    fn paginates_page_with_inherited_media_box() {
        let src = page_with_inherited_media_box(612.0, 2000.0);
        let out = paginate(&src, 792.0, None).unwrap();
        // The tall box was found via the parent chain, then split into 3 pages.
        assert_eq!(media_boxes(&out).len(), 3);
    }

    #[test]
    fn parse_rect_accepts_mixed_numbers_and_rejects_bad_shapes() {
        // Integers and reals mix freely.
        assert_eq!(
            parse_rect(&Object::Array(vec![
                0.into(),
                0.into(),
                Object::Real(612.0),
                792.into(),
            ]))
            .unwrap(),
            [0.0, 0.0, 612.0, 792.0]
        );
        // Not an array at all.
        assert!(parse_rect(&Object::Integer(5)).is_err());
        // Wrong element count.
        assert!(parse_rect(&Object::Array(vec![0.into(), 0.into(), 612.into()])).is_err());
        // A non-numeric element.
        assert!(parse_rect(&Object::Array(vec![
            0.into(),
            0.into(),
            612.into(),
            Object::Name(b"nope".to_vec()),
        ]))
        .is_err());
    }

    #[test]
    fn errors_display_readably() {
        assert!(PdfError::Parse("boom".into()).to_string().contains("parse"));
        assert!(PdfError::Structure("boom".into())
            .to_string()
            .contains("structure"));
        assert!(PdfError::Write("boom".into()).to_string().contains("write"));
    }

    #[test]
    fn background_prepends_fill_and_keeps_shared_content() {
        let src = tall_page(612.0, 2000.0);
        let out = paginate(&src, 792.0, Some([0.1, 0.1, 0.12])).unwrap();
        let doc = Document::load_mem(&out).unwrap();
        let mut originals = Vec::new();
        for &id in doc.get_pages().values() {
            let contents = doc
                .get_dictionary(id)
                .unwrap()
                .get(b"Contents")
                .unwrap()
                .as_array()
                .unwrap();
            // Each page is [fill, original]: a per-page fill then the shared body.
            assert_eq!(contents.len(), 2);
            originals.push(contents[1].as_reference().unwrap());
        }
        assert_eq!(originals.len(), 3);
        // The original content object is still shared, not duplicated per page.
        assert!(originals.iter().all(|&r| r == originals[0]));
    }

    #[test]
    fn parses_css_colors_and_rejects_others() {
        assert_eq!(parse_css_color("rgb(255, 0, 0)"), Some([1.0, 0.0, 0.0]));
        assert_eq!(parse_css_color("rgba(0, 0, 0, 1)"), Some([0.0, 0.0, 0.0]));
        // Space-separated (CSS Color 4 serialization) with alpha via slash.
        assert_eq!(
            parse_css_color("rgb(51 51 51 / 0.5)"),
            Some([51.0 / 255.0, 51.0 / 255.0, 51.0 / 255.0])
        );
        assert!(parse_css_color("transparent").is_none());
        assert!(parse_css_color("#ffffff").is_none());
        assert!(parse_css_color("rgb(1, 2)").is_none());
    }
}
