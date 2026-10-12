use std::collections::BTreeMap;

use lopdf::{Document, Object, ObjectId};
use pdf_inspector::{PdfError, TextItem};

use super::{RawBlock, RawDocument, RawPage, RawSpan};
use crate::error::ParseError;
use crate::model::{norm_box_from_pdf, Anchor, AnchorKind, BlockType, PageGeom, SpanType};

const SAME_LINE_PT: f32 = 2.0;

pub(crate) fn decode(bytes: &[u8]) -> Result<RawDocument, ParseError> {
    let mut info = page_info(bytes);
    let items = pdf_inspector::extract_text_with_positions_mem(bytes).map_err(map_pdf)?;
    if info.count == 0 {
        let max_page = items.iter().map(|item| item.page).max().unwrap_or(0);
        info.count = max_page;
        info.sizes = vec![(612.0, 792.0); max_page as usize];
    }
    let mut by_page: BTreeMap<u32, Vec<TextItem>> = BTreeMap::new();
    for item in items {
        if item.text.is_empty() || item.page == 0 {
            continue;
        }
        by_page.entry(item.page - 1).or_default().push(item);
    }
    if by_page.is_empty() {
        return Err(ParseError::NeedsOcr);
    }
    let page_count = info
        .count
        .max(by_page.keys().next_back().copied().unwrap_or(0) + 1);
    let mut pages = Vec::new();
    for page_idx in 0..page_count {
        let (width, height) = info
            .sizes
            .get(page_idx as usize)
            .copied()
            .unwrap_or((612.0, 792.0));
        let runs = by_page.remove(&page_idx).unwrap_or_default();
        if runs.is_empty() {
            pages.push(RawPage {
                page_idx,
                geom: geom(page_idx, width, height),
                failure: Some("needs_ocr".into()),
                blocks: Vec::new(),
            });
            continue;
        }
        pages.push(RawPage {
            page_idx,
            geom: geom(page_idx, width, height),
            failure: None,
            blocks: lines_to_blocks(runs, page_idx, width, height),
        });
    }
    Ok(RawDocument {
        parser: "pdf-text/2".into(),
        page_count,
        pages,
    })
}

fn lines_to_blocks(items: Vec<TextItem>, page_idx: u32, width: f64, height: f64) -> Vec<RawBlock> {
    let mut blocks = Vec::new();
    for line in group_lines(items) {
        let mut text = String::new();
        let mut spans = Vec::new();
        let mut left = f64::MAX;
        let mut bottom = f64::MAX;
        let mut right = f64::MIN;
        let mut top = f64::MIN;
        let mut previous_right: Option<f32> = None;
        for item in &line {
            if let Some(edge) = previous_right {
                if item.x > edge + 0.5 {
                    text.push(' ');
                }
            }
            text.push_str(&item.text);
            previous_right = Some(item.x + item.width);
            let x0 = f64::from(item.x);
            let y0 = f64::from(item.y);
            let x1 = f64::from(item.x + item.width);
            let y1 = f64::from(item.y + item.height);
            left = left.min(x0);
            bottom = bottom.min(y0);
            right = right.max(x1);
            top = top.max(y1);
            let bbox = norm_box_from_pdf(x0, y0, x1, y1, width, height);
            spans.push(RawSpan {
                text: item.text.clone(),
                span_type: SpanType::Text,
                anchors: vec![Anchor {
                    kind: AnchorKind::PageBox,
                    page_idx,
                    bbox,
                    part: None,
                }],
                confidence: None,
            });
        }
        let digits = text.chars().all(|ch| ch.is_ascii_digit());
        let bbox = norm_box_from_pdf(left, bottom, right, top, width, height);
        blocks.push(RawBlock {
            block_type: if digits {
                BlockType::PageNumber
            } else {
                BlockType::Text
            },
            text,
            text_level: None,
            anchors: vec![Anchor {
                kind: AnchorKind::PageBox,
                page_idx,
                bbox,
                part: None,
            }],
            unanchored: bbox.is_none(),
            discarded: digits,
            table_continue: false,
            spans,
        });
    }
    blocks
}

fn group_lines(mut items: Vec<TextItem>) -> Vec<Vec<TextItem>> {
    items.sort_by(|left, right| {
        right
            .line_y()
            .total_cmp(&left.line_y())
            .then(left.x.total_cmp(&right.x))
    });
    let mut lines: Vec<Vec<TextItem>> = Vec::new();
    for item in items {
        let same_line = lines
            .last()
            .and_then(|line| line.first())
            .is_some_and(|first| (first.line_y() - item.line_y()).abs() <= SAME_LINE_PT);
        if same_line {
            if let Some(line) = lines.last_mut() {
                line.push(item);
                continue;
            }
        }
        lines.push(vec![item]);
    }
    for line in &mut lines {
        line.sort_by(|left, right| left.x.total_cmp(&right.x));
    }
    lines
}

fn geom(page_idx: u32, width: f64, height: f64) -> PageGeom {
    PageGeom {
        page_idx,
        width_pt: width,
        height_pt: height,
        rotation: 0,
    }
}

struct PageInfo {
    count: u32,
    sizes: Vec<(f64, f64)>,
}

fn page_info(bytes: &[u8]) -> PageInfo {
    let Ok(document) = Document::load_mem(bytes) else {
        return PageInfo {
            count: 0,
            sizes: Vec::new(),
        };
    };
    let pages = document.get_pages();
    let sizes = pages
        .into_iter()
        .map(|(_number, page_id)| page_box(&document, page_id).unwrap_or((612.0, 792.0)))
        .collect::<Vec<_>>();
    let count = sizes.len() as u32;
    PageInfo { count, sizes }
}

fn page_box(document: &Document, page_id: ObjectId) -> Option<(f64, f64)> {
    let mut current = page_id;
    for _ in 0..8 {
        let Object::Dictionary(dict) = document.get_object(current).ok()? else {
            return None;
        };
        if let Ok(object) = dict.get(b"MediaBox") {
            if let Some(box_) = rectangle(document, object) {
                return Some(box_);
            }
        }
        let Ok(Object::Reference(parent)) = dict.get(b"Parent") else {
            return None;
        };
        current = *parent;
    }
    None
}

fn rectangle(document: &Document, object: &Object) -> Option<(f64, f64)> {
    let values = match object {
        Object::Array(values) => values,
        Object::Reference(id) => match document.get_object(*id).ok()? {
            Object::Array(values) => values,
            _ => return None,
        },
        _ => return None,
    };
    if values.len() != 4 {
        return None;
    }
    let x0 = number(&values[0])?;
    let y0 = number(&values[1])?;
    let x1 = number(&values[2])?;
    let y1 = number(&values[3])?;
    let width = (x1 - x0).abs();
    let height = (y1 - y0).abs();
    (width > 0.0 && height > 0.0).then_some((width, height))
}

fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(*value as f64),
        _ => None,
    }
}

fn map_pdf(error: PdfError) -> ParseError {
    match error {
        PdfError::Encrypted => ParseError::InvalidRequest("document is encrypted".into()),
        PdfError::NotAPdf(detail) => ParseError::InvalidRequest(detail),
        other => ParseError::Unavailable(other.to_string()),
    }
}

#[cfg(test)]
pub(crate) fn fixture_hello_pdf() -> Vec<u8> {
    fixture_pdf(b"BT /F1 12 Tf 72 720 Td (Hello) Tj ET")
}

#[cfg(test)]
pub(crate) fn fixture_pdf(content: &[u8]) -> Vec<u8> {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(lopdf::dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica"
    });
    let content_id =
        document.add_object(lopdf::Stream::new(lopdf::dictionary! {}, content.to_vec()));
    let page_id = document.add_object(lopdf::dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Contents" => content_id,
        "Resources" => lopdf::dictionary! {
            "Font" => lopdf::dictionary! {
                "F1" => font_id
            }
        }
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(lopdf::dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1
        }),
    );
    let catalog_id = document.add_object(lopdf::dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id
    });
    document.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).expect("fixture pdf");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_runs_on_one_baseline_become_one_block() {
        let bytes = fixture_pdf(b"BT /F1 12 Tf 72 720 Td (Hel) Tj 30 0 Td (lo) Tj ET");
        let document = decode(&bytes).unwrap();
        assert_eq!(document.parser, "pdf-text/2");
        assert_eq!(document.pages[0].blocks.len(), 1);
        assert!(document.pages[0].blocks[0].text.contains("Hel"));
        assert!(document.pages[0].blocks[0].text.contains("lo"));
        assert!(document.pages[0].blocks[0].spans.len() >= 2);
        assert!(document.pages[0].blocks[0].spans.iter().all(|span| {
            span.anchors[0].kind == AnchorKind::PageBox && span.anchors[0].bbox.is_some()
        }));
    }
}
