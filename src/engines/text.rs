use super::{paragraph_anchor, text_block, RawDocument, RawPage};
use crate::model::{BlockType, PageGeom};

pub(crate) fn decode(bytes: &[u8]) -> RawDocument {
    let text = String::from_utf8_lossy(bytes);
    let pieces = text.split('\u{000c}').collect::<Vec<_>>();
    let page_count = pieces.len() as u32;
    let mut previous_was_table = false;
    let mut pages = Vec::new();
    for (page_idx, piece) in pieces.into_iter().enumerate() {
        let mut blocks = Vec::new();
        let mut paragraph_index = 0usize;
        for paragraph in piece.split("\n\n") {
            let paragraph = paragraph.trim();
            if paragraph.is_empty() {
                continue;
            }
            let table = paragraph.lines().all(|line| line.trim().starts_with('|'));
            let page_number = paragraph.chars().all(|ch| ch.is_ascii_digit());
            let anchor = paragraph_anchor(page_idx as u32, paragraph_index);
            paragraph_index += 1;
            let continue_table = table && previous_was_table && blocks.is_empty();
            blocks.push(text_block(
                if page_number {
                    BlockType::PageNumber
                } else if table {
                    BlockType::Table
                } else {
                    BlockType::Text
                },
                paragraph.to_string(),
                anchor,
                page_number,
                continue_table,
            ));
            previous_was_table = table;
        }
        if blocks.is_empty() {
            previous_was_table = false;
        }
        pages.push(RawPage {
            page_idx: page_idx as u32,
            geom: PageGeom {
                page_idx: page_idx as u32,
                width_pt: 0.0,
                height_pt: 0.0,
                rotation: 0,
            },
            failure: None,
            blocks,
        });
    }
    RawDocument {
        parser: "text/1".into(),
        page_count,
        pages,
    }
}
