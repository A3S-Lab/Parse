use crate::engines::{self, RawBlock, RawDocument};
use crate::error::ParseError;
use crate::model::{
    Block, MarkdownSpan, OutputKind, PageFailure, ParseDocument, ParseMode, ParseRequest, Span,
};

const MARKDOWN_BYTES: usize = 64 * 1024;
const MAX_SPANS: usize = 2000;
const MAX_WINDOW: u32 = 20;

pub(crate) fn parse_document(request: &ParseRequest) -> Result<ParseDocument, ParseError> {
    if request.bytes.len() > 32 * 1024 * 1024 {
        return Err(ParseError::InvalidRequest(
            "document exceeds 32MiB".into(),
        ));
    }
    if request.mode == ParseMode::Ocr {
        return Err(ParseError::NeedsOcr);
    }
    let (start, end) = page_window(request.pages.as_deref())?;
    let media = engines::resolve_media_type(request.media_type.as_deref(), &request.bytes)?;
    if engines::is_image(&media) {
        return Err(ParseError::NeedsOcr);
    }
    let mut raw = engines::decode(&media, &request.bytes)?;
    let page_count = raw.page_count.max(raw.pages.len() as u32);
    raw.page_count = page_count;
    let window_end = end.min(page_count);
    let truncated = window_end < page_count || end < page_count;
    let next_page = truncated.then_some(window_end + 1);
    let include_spans = request.outputs.contains(&OutputKind::Spans);
    let include_discarded = request.outputs.contains(&OutputKind::Discarded);
    let (blocks, discarded, spans, markdown, markdown_map, discarded_count, pages_failed) =
        project(&raw, start, window_end, include_spans, include_discarded);
    let to_page = if window_end == 0 {
        0
    } else {
        window_end - 1
    };
    Ok(ParseDocument {
        media_type: media,
        parser: raw.parser,
        page_count,
        from_page: start,
        to_page,
        truncated,
        next_page,
        markdown,
        blocks,
        spans,
        markdown_map,
        discarded_count,
        discarded,
        pages_failed,
        page_geom: raw
            .pages
            .iter()
            .filter(|page| page.page_idx >= start && page.page_idx < window_end)
            .map(|page| page.geom.clone())
            .collect(),
    })
}

fn page_window(spec: Option<&str>) -> Result<(u32, u32), ParseError> {
    let Some(spec) = spec.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok((0, MAX_WINDOW));
    };
    let (left, right) = spec.split_once('-').ok_or_else(|| {
        ParseError::InvalidRequest("pages must look like 1-20".into())
    })?;
    let start: u32 = left
        .trim()
        .parse()
        .map_err(|_| ParseError::InvalidRequest("pages must look like 1-20".into()))?;
    let end: u32 = right
        .trim()
        .parse()
        .map_err(|_| ParseError::InvalidRequest("pages must look like 1-20".into()))?;
    if start == 0 || end < start || end - start + 1 > MAX_WINDOW {
        return Err(ParseError::InvalidRequest(
            "pages must be a 1-based window of at most 20 pages".into(),
        ));
    }
    Ok((start - 1, end))
}

fn project(
    raw: &RawDocument,
    start: u32,
    end: u32,
    include_spans: bool,
    include_discarded: bool,
) -> (
    Vec<Block>,
    Vec<Block>,
    Vec<Span>,
    String,
    Vec<MarkdownSpan>,
    u32,
    Vec<PageFailure>,
) {
    let mut kept = Vec::new();
    let mut discarded_raw = Vec::new();
    let mut pages_failed = Vec::new();
    for page in &raw.pages {
        if page.page_idx < start || page.page_idx >= end {
            continue;
        }
        if let Some(reason) = &page.failure {
            pages_failed.push(PageFailure {
                page_idx: page.page_idx,
                reason: reason.clone(),
            });
        }
        for block in &page.blocks {
            if block.discarded {
                discarded_raw.push(block.clone());
            } else {
                kept.push(block.clone());
            }
        }
    }
    let kept = merge_tables(kept);
    let discarded_count = discarded_raw.len() as u32;
    let mut blocks = Vec::new();
    let mut spans = Vec::new();
    let mut markdown = String::new();
    let mut markdown_map = Vec::new();
    for (index, block) in kept.into_iter().enumerate() {
        let page_idx = block.anchors.first().map(|anchor| anchor.page_idx).unwrap_or(0);
        let id = format!("p{page_idx}-b{index}");
        if include_spans && spans.len() < MAX_SPANS {
            for (line, span) in block.spans.iter().enumerate() {
                if spans.len() >= MAX_SPANS {
                    break;
                }
                spans.push(Span {
                    id: format!("{id}-s{line}"),
                    block_id: id.clone(),
                    line: line as u32,
                    text: span.text.clone(),
                    span_type: span.span_type,
                    anchors: span.anchors.clone(),
                    confidence: span.confidence,
                });
            }
        }
        if !markdown.is_empty() && markdown.len() + 2 <= MARKDOWN_BYTES {
            markdown.push_str("\n\n");
        }
        let text_start = markdown.chars().count();
        push_bounded(&mut markdown, &block.text);
        let text_end = markdown.chars().count();
        markdown_map.push(MarkdownSpan {
            start: text_start,
            end: text_end,
            block_id: id.clone(),
        });
        let stop = markdown.len() >= MARKDOWN_BYTES;
        blocks.push(Block {
            id,
            order: index as u32,
            block_type: block.block_type,
            text: block.text,
            text_level: block.text_level,
            parent_id: None,
            anchors: block.anchors,
            unanchored: block.unanchored,
        });
        if stop {
            break;
        }
    }
    // Drop a map entry whose text was cut off past the markdown budget.
    markdown_map.retain(|item| item.end <= markdown.chars().count());
    let discarded = if include_discarded {
        discarded_raw
            .into_iter()
            .enumerate()
            .map(|(index, block)| Block {
                id: format!("d{index}"),
                order: index as u32,
                block_type: block.block_type,
                text: block.text,
                text_level: None,
                parent_id: None,
                anchors: block.anchors,
                unanchored: block.unanchored,
            })
            .collect()
    } else {
        Vec::new()
    };
    (
        blocks,
        discarded,
        spans,
        markdown,
        markdown_map,
        discarded_count,
        pages_failed,
    )
}

fn push_bounded(markdown: &mut String, text: &str) {
    for ch in text.chars() {
        let next = markdown.len() + ch.len_utf8();
        if next > MARKDOWN_BYTES {
            return;
        }
        markdown.push(ch);
    }
}

fn merge_tables(blocks: Vec<RawBlock>) -> Vec<RawBlock> {
    let mut merged: Vec<RawBlock> = Vec::new();
    for block in blocks {
        let continue_table = block.table_continue
            && merged
                .last()
                .is_some_and(|previous| previous.block_type == crate::model::BlockType::Table);
        if continue_table {
            if let Some(previous) = merged.last_mut() {
                if !previous.text.is_empty() {
                    previous.text.push('\n');
                }
                previous.text.push_str(&block.text);
                previous.anchors.extend(block.anchors);
                previous.spans.extend(block.spans);
                previous.unanchored = previous.unanchored && block.unanchored;
            }
        } else {
            merged.push(block);
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{restore_x_pt, AnchorKind, BlockType, ParseMode};

    fn parse(bytes: &str) -> ParseDocument {
        let mut request = ParseRequest::new(bytes.as_bytes().to_vec());
        request.media_type = Some("text/plain".into());
        request.outputs = vec![
            OutputKind::Markdown,
            OutputKind::Blocks,
            OutputKind::Spans,
            OutputKind::Discarded,
        ];
        parse_document(&request).unwrap()
    }

    #[test]
    fn markdown_map_points_at_block_text() {
        let document = parse("Alpha\n\nBeta");
        assert_eq!(document.blocks.len(), 2);
        for (block, map) in document.blocks.iter().zip(&document.markdown_map) {
            let slice: String = document
                .markdown
                .chars()
                .skip(map.start)
                .take(map.end - map.start)
                .collect();
            assert_eq!(slice, block.text);
            assert_eq!(map.block_id, block.id);
        }
    }

    #[test]
    fn page_numbers_are_discarded_and_counted() {
        let document = parse("12\n\nBody");
        assert_eq!(document.discarded_count, 1);
        assert_eq!(document.markdown, "Body");
        assert_eq!(document.discarded[0].block_type, BlockType::PageNumber);
    }

    #[test]
    fn a_table_keeps_an_anchor_on_every_page() {
        let document = parse("| a |\n| b |\u{000c}| c |\n| d |");
        assert_eq!(document.blocks.len(), 1);
        assert_eq!(document.blocks[0].block_type, BlockType::Table);
        let pages: Vec<_> = document.blocks[0]
            .anchors
            .iter()
            .map(|anchor| anchor.page_idx)
            .collect();
        assert_eq!(pages, vec![0, 1]);
    }

    #[test]
    fn span_output_stops_at_2000() {
        let text = (0..2001)
            .map(|index| format!("row {index}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let document = parse(&text);
        assert_eq!(document.spans.len(), 2000);
    }

    #[test]
    fn the_next_window_keeps_original_page_indexes() {
        let text = (0..30)
            .map(|index| format!("page {index}"))
            .collect::<Vec<_>>()
            .join("\u{000c}");
        let mut request = ParseRequest::new(text.into_bytes());
        request.media_type = Some("text/plain".into());
        request.pages = Some("1-20".into());
        let first = parse_document(&request).unwrap();
        assert_eq!(first.next_page, Some(21));
        assert_eq!(first.blocks[0].anchors[0].page_idx, 0);
        request.pages = Some("21-40".into());
        let second = parse_document(&request).unwrap();
        assert_eq!(second.blocks[0].anchors[0].page_idx, 20);
        assert!(second.next_page.is_none());
    }

    #[test]
    fn ocr_mode_is_refused() {
        let mut request = ParseRequest::new(b"hello".to_vec());
        request.mode = ParseMode::Ocr;
        assert!(matches!(parse_document(&request), Err(ParseError::NeedsOcr)));
    }

    #[test]
    fn a_png_needs_ocr() {
        let mut request = ParseRequest::new(vec![0x89, b'P', b'N', b'G', 0, 0, 0, 0]);
        request.media_type = Some("image/png".into());
        assert!(matches!(parse_document(&request), Err(ParseError::NeedsOcr)));
    }

    #[test]
    fn magic_must_match_the_declared_type() {
        let mut request = ParseRequest::new(b"hello".to_vec());
        request.media_type = Some("application/pdf".into());
        assert!(matches!(
            parse_document(&request),
            Err(ParseError::InvalidRequest(_))
        ));
    }

    #[test]
    fn pdf_text_box_restores_near_the_text_origin() {
        let bytes = crate::engines::pdf::fixture_hello_pdf();
        let mut request = ParseRequest::new(bytes);
        request.media_type = Some("application/pdf".into());
        let document = parse_document(&request).unwrap();
        let anchor = &document.blocks[0].anchors[0];
        assert_eq!(anchor.kind, AnchorKind::PageBox);
        let bbox = anchor.bbox.expect("text layer has a box");
        let x = restore_x_pt(bbox[0], document.page_geom[0].width_pt);
        assert!((x - 72.0).abs() < 1.0, "{x}");
        assert_eq!(document.blocks[0].text, "Hello");
        assert!(!document.blocks[0].unanchored);
    }
}
