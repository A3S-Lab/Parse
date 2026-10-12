use crate::engines::{self, RawBlock, RawDocument};
use crate::error::ParseError;
use crate::model::{
    Block, MarkdownSpan, OutputKind, PageFailure, ParseDocument, ParseMode, ParseRequest, Span,
};

const MARKDOWN_BYTES: usize = 2 * 1024 * 1024;
const MAX_SPANS: usize = 2000;
const MAX_WINDOW: u32 = 500;

pub(crate) fn parse_document(request: &ParseRequest) -> Result<ParseDocument, ParseError> {
    if request.bytes.len() > 32 * 1024 * 1024 {
        return Err(ParseError::InvalidRequest("document exceeds 32MiB".into()));
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
    let include_spans = request.outputs.contains(&OutputKind::Spans);
    let include_discarded = request.outputs.contains(&OutputKind::Discarded);
    let (
        blocks,
        discarded,
        spans,
        markdown,
        markdown_map,
        discarded_count,
        pages_failed,
        budget_resume,
    ) = project(&raw, start, window_end, include_spans, include_discarded);
    let (to_exclusive, truncated, next_page) = match budget_resume {
        Some(resume) => (
            resume.covered_end,
            true,
            (resume.next_page < raw.page_count).then_some(resume.next_page + 1),
        ),
        None => (
            window_end,
            window_end < page_count || end < page_count,
            (window_end < page_count || end < page_count).then_some(window_end + 1),
        ),
    };
    let to_page = if to_exclusive == 0 {
        0
    } else {
        to_exclusive - 1
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
            .filter(|page| page.page_idx >= start && page.page_idx < to_exclusive)
            .map(|page| page.geom.clone())
            .collect(),
    })
}

fn page_window(spec: Option<&str>) -> Result<(u32, u32), ParseError> {
    let Some(spec) = spec.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok((0, MAX_WINDOW));
    };
    let (left, right) = spec
        .split_once('-')
        .ok_or_else(|| ParseError::InvalidRequest("pages must look like 1-10".into()))?;
    let start: u32 = left
        .trim()
        .parse()
        .map_err(|_| ParseError::InvalidRequest("pages must look like 1-10".into()))?;
    let end: u32 = right
        .trim()
        .parse()
        .map_err(|_| ParseError::InvalidRequest("pages must look like 1-10".into()))?;
    if start == 0 || end < start || end - start + 1 > MAX_WINDOW {
        return Err(ParseError::InvalidRequest(format!(
            "pages must be a 1-based window of at most {MAX_WINDOW} pages"
        )));
    }
    Ok((start - 1, end))
}

struct BudgetResume {
    covered_end: u32,
    next_page: u32,
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
    Option<BudgetResume>,
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
    let mut blocks = Vec::new();
    let mut spans = Vec::new();
    let mut markdown = String::new();
    let mut markdown_map = Vec::new();
    let mut budget_resume = None;
    let mut last_included = None;
    for (index, block) in kept.iter().enumerate() {
        let page_idx = block
            .anchors
            .first()
            .map(|anchor| anchor.page_idx)
            .unwrap_or(0);
        let id = format!("p{page_idx}-b{index}");
        let index = index as u32;
        let mut stored_spans = 0_usize;
        if include_spans {
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
                stored_spans += 1;
            }
        }
        let before_separator = markdown.len();
        if !markdown.is_empty() && markdown.len() + 2 <= MARKDOWN_BYTES {
            markdown.push_str("\n\n");
        }
        let text_start = markdown.chars().count();
        let text_fit = push_bounded(&mut markdown, &block.text);
        let written_chars = markdown.chars().count() - text_start;
        if !text_fit && text_start > 0 {
            spans.retain(|span| span.block_id != id);
            markdown.truncate(before_separator);
            budget_resume = Some(BudgetResume {
                covered_end: last_included.map(|page| page + 1).unwrap_or(start),
                next_page: page_idx,
            });
            break;
        }
        let text = if text_fit {
            block.text.clone()
        } else {
            block.text.chars().take(written_chars).collect()
        };
        if written_chars > 0 {
            markdown_map.push(MarkdownSpan {
                start: text_start,
                end: text_start + written_chars,
                block_id: id.clone(),
            });
        }
        let spans_cut = include_spans && stored_spans < block.spans.len();
        let later_spans = include_spans
            && spans.len() >= MAX_SPANS
            && kept[index as usize + 1..]
                .iter()
                .any(|later| !later.spans.is_empty());
        let next_page_idx = kept
            .get(index as usize + 1)
            .and_then(|later| later.anchors.first().map(|anchor| anchor.page_idx));
        blocks.push(Block {
            id,
            order: index,
            block_type: block.block_type,
            text,
            text_level: block.text_level,
            anchors: block.anchors.clone(),
            unanchored: block.unanchored,
        });
        last_included = Some(page_idx);
        let more = index as usize + 1 < kept.len();
        if !text_fit || spans_cut || later_spans || (markdown.len() >= MARKDOWN_BYTES && more) {
            let complete = text_fit && !spans_cut;
            let next_page = if complete {
                next_page_idx
                    .filter(|page| *page > page_idx)
                    .unwrap_or(page_idx + 1)
            } else {
                page_idx + 1
            };
            budget_resume = Some(BudgetResume {
                covered_end: page_idx + 1,
                next_page,
            });
            break;
        }
    }
    if let Some(resume) = &budget_resume {
        pages_failed.retain(|failure| failure.page_idx < resume.covered_end);
        discarded_raw.retain(|block| {
            block
                .anchors
                .first()
                .map(|anchor| anchor.page_idx)
                .unwrap_or(0)
                < resume.covered_end
        });
    }
    let discarded_count = discarded_raw.len() as u32;
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
        budget_resume,
    )
}

fn push_bounded(markdown: &mut String, text: &str) -> bool {
    for ch in text.chars() {
        let next = markdown.len() + ch.len_utf8();
        if next > MARKDOWN_BYTES {
            return false;
        }
        markdown.push(ch);
    }
    true
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
    fn a_markdown_budget_names_the_next_page() {
        let page = "a".repeat(1024 * 1024);
        let text = format!("{page}\u{000c}{page}\u{000c}{page}");
        let mut request = ParseRequest::new(text.into_bytes());
        request.media_type = Some("text/plain".into());
        let document = parse_document(&request).unwrap();
        assert!(document.truncated);
        assert_eq!(document.from_page, 0);
        assert_eq!(document.to_page, 0);
        assert_eq!(document.next_page, Some(2));
        assert_eq!(document.markdown.len(), 1024 * 1024);
    }

    #[test]
    fn a_page_larger_than_the_markdown_budget_keeps_only_the_prefix() {
        let page = "b".repeat(2 * 1024 * 1024 + 32);
        let text = format!("{page}\u{000c}tail");
        let mut request = ParseRequest::new(text.into_bytes());
        request.media_type = Some("text/plain".into());
        let document = parse_document(&request).unwrap();
        assert!(document.truncated);
        assert_eq!(document.to_page, 0);
        assert_eq!(document.next_page, Some(2));
        assert!(document.markdown.len() <= 2 * 1024 * 1024);
        assert_eq!(document.blocks.len(), 1);
        assert_eq!(document.blocks[0].text, document.markdown);
        assert!(document.markdown.len() < page.len());
    }

    #[test]
    fn a_document_past_twenty_pages_parses_in_one_window() {
        let text = (0..40)
            .map(|index| format!("page {index}"))
            .collect::<Vec<_>>()
            .join("\u{000c}");
        let mut request = ParseRequest::new(text.into_bytes());
        request.media_type = Some("text/plain".into());
        let document = parse_document(&request).unwrap();
        assert_eq!(document.page_count, 40);
        assert!(!document.truncated);
        assert!(document.next_page.is_none());
    }

    #[test]
    fn a_window_above_five_hundred_pages_is_rejected() {
        let mut request = ParseRequest::new(b"hello".to_vec());
        request.pages = Some("1-501".into());
        match parse_document(&request) {
            Err(ParseError::InvalidRequest(message)) => {
                assert!(message.contains("500"), "{message}")
            }
            other => panic!("expected a window rejection, got {other:?}"),
        }
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
        assert!(matches!(
            parse_document(&request),
            Err(ParseError::NeedsOcr)
        ));
    }

    #[test]
    fn a_png_needs_ocr() {
        let mut request = ParseRequest::new(vec![0x89, b'P', b'N', b'G', 0, 0, 0, 0]);
        request.media_type = Some("image/png".into());
        assert!(matches!(
            parse_document(&request),
            Err(ParseError::NeedsOcr)
        ));
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
