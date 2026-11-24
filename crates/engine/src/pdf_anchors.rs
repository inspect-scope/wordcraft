//! PDF and bookmark ranges from one final print layout.

use std::collections::HashMap;

use serde::Serialize;
use wordcraft_doc::{Document, InlineObject, Pos, StoryRef};
use wordcraft_layout::{DocLayout, LayoutOptions};

use crate::io::ExportOptions;

const MAX_MARKERS: usize = 40_000;
const MAX_RECTS: usize = 200_000;
const MAX_RANGE_WORK: usize = 1_000_000;
const SCHEMA_VERSION: u8 = 1;
const MIN_PDF_SIDE: f32 = 3.0;
const MAX_PDF_SIDE: f32 = 14_400.0;
const START_PREFIX: &str = "scope_anchor_";
const END_PREFIX: &str = "scope_anchor_end_";

pub(crate) fn write(doc: &Document, opts: &ExportOptions, pdf_path: &str, map_path: &str) -> Result<(), String> {
    if same_path(pdf_path, map_path) {
        return Err("The anchor map cannot replace the PDF".into());
    }
    crate::file_access::check_write(pdf_path)?;
    crate::file_access::check_write(map_path)?;
    let (pdf, map) = render(doc, opts, START_PREFIX, END_PREFIX)?;
    let json = serde_json::to_vec(&map).map_err(|e| e.to_string())?;
    crate::io::write_file(pdf_path, &pdf)?;
    crate::io::write_file(map_path, &json)
}

pub(crate) fn same_path(left: &str, right: &str) -> bool {
    let canonical = |path: &str| {
        let path = std::path::Path::new(path);
        std::fs::canonicalize(path).or_else(|_| {
            let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| std::path::Path::new("."));
            std::fs::canonicalize(parent).map(|parent| parent.join(path.file_name().unwrap_or_default()))
        })
    };
    left == right || matches!((canonical(left), canonical(right)), (Ok(left), Ok(right)) if left == right)
}

#[derive(Serialize)]
struct PageSize {
    width: f32,
    height: f32,
}

#[derive(Serialize)]
struct AnchorRect {
    page: usize,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[derive(Serialize)]
struct Anchor {
    name: String,
    page: usize,
    x: f32,
    y: f32,
    height: f32,
    rects: Vec<AnchorRect>,
}

/// Coordinates use zero-based pages and points from the page's top left.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AnchorMap {
    schema_version: u8,
    pages: Vec<PageSize>,
    anchors: Vec<Anchor>,
}

/// Render the PDF and ranges after print fields settle. The source is unchanged.
/// Duplicate, missing and reversed marker pairs have no guessed destination.
fn render(doc: &Document, opts: &ExportOptions, start_prefix: &str, end_prefix: &str) -> Result<(Vec<u8>, AnchorMap), String> {
    if start_prefix.is_empty() || end_prefix.is_empty() || start_prefix == end_prefix || start_prefix.len() > 40 || end_prefix.len() > 40 {
        return Err("Invalid PDF bookmark prefixes".into());
    }
    if opts.markup {
        return Err("Bookmark geometry requires a PDF without print markup".into());
    }
    let lo = LayoutOptions { hide_deleted: true, numeral: opts.numeral, ..Default::default() };
    let output = crate::print_fields::prepare(doc, &lo)?;
    let anchors = read_anchors(&output.doc, &output.layout, start_prefix, end_prefix)?;
    let pdf_opts = wordcraft_pdf::PdfOptions { numeral: opts.numeral, ..Default::default() };
    let pdf = wordcraft_pdf::export_layout(&output.doc, &output.layout, &pdf_opts).map_err(|e| e.to_string())?;
    Ok((pdf, anchors))
}

fn read_anchors(doc: &Document, lay: &DocLayout, start_prefix: &str, end_prefix: &str) -> Result<AnchorMap, String> {
    // PDF export clamps page dimensions. Refuse a map in a different coordinate space.
    if lay.pages.iter().any(|page| ![page.w, page.h].iter().all(|side| side.is_finite() && (MIN_PDF_SIDE..=MAX_PDF_SIDE).contains(side))) {
        return Err("PDF page dimensions cannot support exact anchors".into());
    }
    let mut starts: HashMap<String, Option<Pos>> = HashMap::new();
    let mut ends: HashMap<String, Option<Pos>> = HashMap::new();
    let mut marker_count = 0usize;
    let stories = std::iter::once(StoryRef::Body).chain(doc.parts.keys().copied().map(StoryRef::Part));
    for story in stories {
        for path in doc.para_paths(story) {
            let Some(para) = doc.para(story, &path) else { continue };
            for (off, object) in para.objects_with_offsets() {
                let InlineObject::BookmarkStart { name } = object else { continue };
                let (target, suffix) = if let Some(suffix) = name.strip_prefix(end_prefix) {
                    (&mut ends, suffix)
                } else if let Some(suffix) = name.strip_prefix(start_prefix) {
                    (&mut starts, suffix)
                } else {
                    continue;
                };
                if suffix.is_empty() || suffix.len() > 20 || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
                    continue;
                }
                marker_count = marker_count.saturating_add(1);
                if marker_count > MAX_MARKERS {
                    return Err("PDF bookmark count exceeds the limit".into());
                }
                let pos = Pos { story, path: path.clone(), off };
                target.entry(suffix.to_owned()).and_modify(|prior| *prior = None).or_insert(Some(pos));
            }
        }
    }

    let pages = lay.pages.iter().map(|page| PageSize { width: page.w, height: page.h }).collect();
    let mut anchors = Vec::new();
    let mut rect_count = 0usize;
    let mut range_work = 0usize;
    for (suffix, start) in starts {
        let (Some(start), Some(Some(end))) = (start, ends.get(&suffix)) else { continue };
        if start.story != end.story || start >= *end {
            continue;
        }
        let repeat = if start.story == StoryRef::Body { 1 } else { lay.pages.len() };
        range_work = range_work.saturating_add(doc.paths_between(&start, end).len().saturating_mul(repeat));
        if range_work > MAX_RANGE_WORK {
            return Err("PDF bookmark ranges exceed the work limit".into());
        }
        let hints: Box<dyn Iterator<Item = usize>> =
            if start.story == StoryRef::Body { Box::new(std::iter::once(0)) } else { Box::new(0..lay.pages.len()) };
        for hint in hints {
            let Some(caret) = lay.caret_on(&start, hint) else { continue };
            if start.story != StoryRef::Body && caret.page != hint {
                continue;
            }
            let mut rects: Vec<_> = lay
                .selection_rects(doc, &start, end, hint)
                .into_iter()
                .filter(|(page, _)| start.story == StoryRef::Body || *page == hint)
                .map(|(page, rect)| AnchorRect { page, x: rect.x, y: rect.y, width: rect.w, height: rect.h })
                .collect();
            rects.sort_by(|left, right| left.page.cmp(&right.page).then(left.y.total_cmp(&right.y)).then(left.x.total_cmp(&right.x)));
            rect_count = rect_count.saturating_add(rects.len());
            if rect_count > MAX_RECTS {
                return Err("PDF bookmark rectangles exceed the limit".into());
            }
            let (page, x, y, height) =
                rects.first().map(|rect| (rect.page, rect.x, rect.y, rect.height)).unwrap_or((caret.page, caret.x, caret.top, caret.height));
            anchors.push(Anchor { name: format!("{start_prefix}{suffix}"), page, x, y, height, rects });
        }
    }
    anchors.sort_by(|left, right| {
        left.page.cmp(&right.page).then(left.y.total_cmp(&right.y)).then(left.x.total_cmp(&right.x)).then(left.name.cmp(&right.name))
    });
    Ok(AnchorMap { schema_version: SCHEMA_VERSION, pages, anchors })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wordcraft_doc::{Block, CharProps, Paragraph};

    const START: &str = "scope_anchor_";
    const END: &str = "scope_anchor_end_";

    fn paragraph(text: &str, start: Option<&str>, end: Option<&str>) -> Paragraph {
        let mut p = Paragraph::new();
        p.insert_text(0, text, &CharProps::default()).unwrap();
        if let Some(name) = end {
            p.insert_object(p.len(), InlineObject::BookmarkStart { name: name.into() }, &CharProps::default()).unwrap();
        }
        if let Some(name) = start {
            p.insert_object(0, InlineObject::BookmarkStart { name: name.into() }, &CharProps::default()).unwrap();
        }
        p
    }

    #[test]
    fn final_pdf_matches_range() {
        let mut doc = Document::new();
        doc.body = vec![
            Block::Para(paragraph("First paragraph", Some("scope_anchor_0"), None)).into(),
            Block::Para(paragraph("Second paragraph", None, Some("scope_anchor_end_0"))).into(),
        ];
        let before = serde_json::to_value(&doc).unwrap();
        let (pdf, map) = render(&doc, &ExportOptions::default(), START, END).unwrap();
        assert_eq!(pdf, crate::io::save_bytes("same.pdf", &doc).unwrap());
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        assert_eq!(map.anchors.len(), 1);
        assert!(map.anchors[0].rects.len() >= 2);
        assert_eq!(map.anchors[0].page, 0);
    }

    #[test]
    fn missing_pairs_stay_unmapped() {
        let mut doc = Document::new();
        doc.body = vec![
            Block::Para(paragraph("Missing", Some("scope_anchor_0"), None)).into(),
            Block::Para(paragraph("Duplicate one", Some("scope_anchor_1"), Some("scope_anchor_end_1"))).into(),
            Block::Para(paragraph("Duplicate two", Some("scope_anchor_1"), None)).into(),
        ];
        let (_, map) = render(&doc, &ExportOptions::default(), START, END).unwrap();
        assert!(map.anchors.is_empty());
    }

    #[test]
    fn ranges_cross_pages() {
        let mut doc = Document::new();
        let first = paragraph("Page one", Some("scope_anchor_0"), None);
        let mut second = paragraph("Page two", None, Some("scope_anchor_end_0"));
        second.props.page_break_before = Some(true);
        doc.body = vec![Block::Para(first).into(), Block::Para(second).into()];
        let (_, map) = render(&doc, &ExportOptions::default(), START, END).unwrap();
        assert_eq!(map.pages.len(), 2);
        let anchor = &map.anchors[0];
        assert!(anchor.rects.iter().any(|rect| rect.page == 0));
        assert!(anchor.rects.iter().any(|rect| rect.page == 1));
    }

    #[test]
    fn anchors_use_print_ref() {
        let mut doc = Document::new();
        let props = CharProps::default();
        let mut reference = Paragraph::new();
        reference.insert_object(0, InlineObject::BookmarkStart { name: "scope_anchor_0".into() }, &props).unwrap();
        reference
            .insert_object(
                reference.len(),
                InlineObject::Field {
                    instr: "REF source".into(),
                    result: "old".into(),
                    locked: false,
                    dirty: true,
                    code: false,
                    form: String::new(),
                },
                &props,
            )
            .unwrap();
        reference.insert_object(reference.len(), InlineObject::BookmarkStart { name: "scope_anchor_end_0".into() }, &props).unwrap();
        let mut source = Paragraph::with_text(&"resolved source text ".repeat(30), props.clone());
        source.insert_object(source.len(), InlineObject::BookmarkEnd { name: "source".into() }, &props).unwrap();
        source.insert_object(0, InlineObject::BookmarkStart { name: "source".into() }, &props).unwrap();
        doc.body = vec![Block::Para(reference).into(), Block::Para(source).into()];
        let (_, map) = render(&doc, &ExportOptions::default(), START, END).unwrap();
        assert!(map.anchors[0].rects.len() > 1, "the short saved field cache was not used");
    }
}
