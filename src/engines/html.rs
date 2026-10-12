use super::{paragraph_anchor, text_block, RawDocument, RawPage};
use crate::model::{BlockType, PageGeom};

pub(crate) fn decode(bytes: &[u8]) -> RawDocument {
    let html = String::from_utf8_lossy(bytes);
    let mut blocks = Vec::new();
    let mut paragraph = String::new();
    let mut index = 0usize;
    let mut chars = html.chars();
    while let Some(ch) = chars.next() {
        if ch == '<' {
            let mut tag = String::new();
            for inner in chars.by_ref() {
                if inner == '>' {
                    break;
                }
                tag.push(inner);
            }
            let name = tag
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_start_matches('/')
                .to_ascii_lowercase();
            if matches!(
                name.as_str(),
                "p" | "div" | "h1" | "h2" | "h3" | "li" | "br"
            ) {
                flush(
                    &mut blocks,
                    &mut paragraph,
                    &mut index,
                    heading_level(&name),
                );
            }
        } else if !ch.is_control() {
            paragraph.push(ch);
        }
    }
    flush(&mut blocks, &mut paragraph, &mut index, None);
    RawDocument {
        parser: "html/1".into(),
        page_count: 1,
        pages: vec![RawPage {
            page_idx: 0,
            geom: PageGeom {
                page_idx: 0,
                width_pt: 0.0,
                height_pt: 0.0,
                rotation: 0,
            },
            failure: None,
            blocks,
        }],
    }
}

fn heading_level(name: &str) -> Option<u8> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        _ => None,
    }
}

fn flush(
    blocks: &mut Vec<super::RawBlock>,
    paragraph: &mut String,
    index: &mut usize,
    level: Option<u8>,
) {
    let text = paragraph.split_whitespace().collect::<Vec<_>>().join(" ");
    paragraph.clear();
    if text.is_empty() {
        return;
    }
    let mut block = text_block(
        if level.is_some() {
            BlockType::Title
        } else {
            BlockType::Text
        },
        text,
        paragraph_anchor(0, *index),
        false,
        false,
    );
    block.text_level = level;
    *index += 1;
    blocks.push(block);
}
