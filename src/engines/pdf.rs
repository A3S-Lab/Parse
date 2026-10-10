use lopdf::{Document, Object, ObjectId};

use super::{RawBlock, RawDocument, RawPage, RawSpan};
use crate::error::ParseError;
use crate::model::{norm_box_from_pdf, Anchor, AnchorKind, BlockType, PageGeom, SpanType};

pub(crate) fn decode(bytes: &[u8]) -> Result<RawDocument, ParseError> {
    let document = Document::load_mem(bytes)
        .map_err(|error| ParseError::Unavailable(format!("pdf could not be read: {error}")))?;
    let mut pages = Vec::new();
    let mut saw_text = false;
    for (index, (page_number, page_id)) in document.get_pages().into_iter().enumerate() {
        let page_idx = index as u32;
        let _ = page_number;
        let (width, height) = page_box(&document, page_id).unwrap_or((612.0, 792.0));
        let content = page_content(&document, page_id).unwrap_or_default();
        let runs = text_runs(&content);
        let mut blocks = Vec::new();
        if runs.is_empty() {
            pages.push(RawPage {
                page_idx,
                geom: geom(page_idx, width, height),
                failure: Some("needs_ocr".into()),
                blocks,
            });
            continue;
        }
        saw_text = true;
        for run in runs {
            let digits = run.text.chars().all(|ch| ch.is_ascii_digit());
            let bbox = norm_box_from_pdf(run.x, run.y, run.x + run.width, run.y + run.height, width, height);
            let anchor = Anchor {
                kind: AnchorKind::PageBox,
                page_idx,
                bbox,
                part: None,
            };
            let span = RawSpan {
                text: run.text.clone(),
                span_type: SpanType::Text,
                anchors: vec![anchor.clone()],
                confidence: None,
            };
            blocks.push(RawBlock {
                block_type: if digits { BlockType::PageNumber } else { BlockType::Text },
                text: run.text,
                text_level: None,
                anchors: vec![anchor],
                unanchored: bbox.is_none(),
                discarded: digits,
                table_continue: false,
                spans: vec![span],
            });
        }
        pages.push(RawPage {
            page_idx,
            geom: geom(page_idx, width, height),
            failure: None,
            blocks,
        });
    }
    if !saw_text {
        return Err(ParseError::NeedsOcr);
    }
    let page_count = pages.len() as u32;
    Ok(RawDocument {
        parser: "pdf-text/1".into(),
        page_count,
        pages,
    })
}

fn geom(page_idx: u32, width: f64, height: f64) -> PageGeom {
    PageGeom {
        page_idx,
        width_pt: width,
        height_pt: height,
        rotation: 0,
    }
}

struct Run {
    text: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

fn text_runs(content: &[u8]) -> Vec<Run> {
    let tokens = tokenize(content);
    let mut runs = Vec::new();
    let mut stack: Vec<Token> = Vec::new();
    let mut x = 0.0;
    let mut y = 0.0;
    let mut font_size = 12.0;
    let mut in_text = false;
    for token in tokens {
        match token {
            Token::Op(op) if op == "BT" => {
                in_text = true;
                x = 0.0;
                y = 0.0;
                stack.clear();
            }
            Token::Op(op) if op == "ET" => {
                in_text = false;
                stack.clear();
            }
            Token::Op(op) if op == "Tm" && in_text => {
                let nums = numbers(&stack);
                if nums.len() >= 6 {
                    x = nums[nums.len() - 2];
                    y = nums[nums.len() - 1];
                }
                stack.clear();
            }
            Token::Op(op) if (op == "Td" || op == "TD") && in_text => {
                let nums = numbers(&stack);
                if nums.len() >= 2 {
                    x += nums[nums.len() - 2];
                    y += nums[nums.len() - 1];
                }
                stack.clear();
            }
            Token::Op(op) if op == "Tf" && in_text => {
                if let Some(size) = numbers(&stack).last().copied() {
                    if size > 0.0 {
                        font_size = size;
                    }
                }
                stack.clear();
            }
            Token::Op(op) if op == "Tj" && in_text => {
                if let Some(text) = last_string(&stack) {
                    push_run(&mut runs, text, x, y, font_size);
                }
                stack.clear();
            }
            Token::Op(op) if op == "TJ" && in_text => {
                if let Some(Token::Array(items)) = stack.last() {
                    let text = items.iter().filter_map(token_string).collect::<String>();
                    if !text.is_empty() {
                        push_run(&mut runs, text, x, y, font_size);
                    }
                }
                stack.clear();
            }
            Token::Op(_) => stack.clear(),
            other => stack.push(other),
        }
    }
    runs
}

fn push_run(runs: &mut Vec<Run>, text: String, x: f64, y: f64, font_size: f64) {
    let width = text.chars().count() as f64 * font_size * 0.5;
    runs.push(Run {
        text,
        x,
        y,
        width,
        height: font_size,
    });
}

fn numbers(stack: &[Token]) -> Vec<f64> {
    stack.iter().filter_map(|token| match token {
        Token::Num(value) => Some(*value),
        _ => None,
    }).collect()
}

fn last_string(stack: &[Token]) -> Option<String> {
    stack.iter().rev().find_map(token_string)
}

fn token_string(token: &Token) -> Option<String> {
    match token {
        Token::Str(value) if !value.is_empty() => Some(value.clone()),
        _ => None,
    }
}

#[allow(dead_code)]
enum Token {
    Num(f64),
    Name(String),
    Str(String),
    Array(Vec<Token>),
    Op(String),
}

fn tokenize(content: &[u8]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < content.len() {
        if let Some((token, next)) = read_token(content, index) {
            tokens.push(token);
            index = next;
        } else {
            index += 1;
        }
    }
    tokens
}

fn read_token(content: &[u8], mut index: usize) -> Option<(Token, usize)> {
    while index < content.len() && content[index].is_ascii_whitespace() {
        index += 1;
    }
    if index >= content.len() {
        return None;
    }
    match content[index] {
        b'%' => {
            while index < content.len() && content[index] != b'\n' {
                index += 1;
            }
            read_token(content, index)
        }
        b'(' => read_literal(content, index),
        b'<' => read_hex_or_dict(content, index),
        b'[' => read_array(content, index),
        b'/' => read_name(content, index),
        b'+' | b'-' | b'.' | b'0'..=b'9' => read_number(content, index),
        b'a'..=b'z' | b'A'..=b'Z' | b'\'' | b'"' => read_op(content, index),
        _ => None,
    }
}

fn read_literal(content: &[u8], start: usize) -> Option<(Token, usize)> {
    let mut text = String::new();
    let mut index = start + 1;
    while index < content.len() {
        match content[index] {
            b')' => return Some((Token::Str(text), index + 1)),
            b'\\' if index + 1 < content.len() => {
                index += 1;
                text.push(match content[index] {
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    other => other as char,
                });
                index += 1;
            }
            byte => {
                text.push(byte as char);
                index += 1;
            }
        }
    }
    None
}

fn read_hex_or_dict(content: &[u8], start: usize) -> Option<(Token, usize)> {
    if content.get(start + 1) == Some(&b'<') {
        let end = content[start + 2..].windows(2).position(|pair| pair == b">>")? + start + 4;
        return Some((Token::Op("dict".into()), end));
    }
    let end = content[start + 1..].iter().position(|byte| *byte == b'>')? + start + 2;
    Some((Token::Str(String::new()), end))
}

fn read_array(content: &[u8], start: usize) -> Option<(Token, usize)> {
    let mut items = Vec::new();
    let mut index = start + 1;
    while index < content.len() {
        if content[index] == b']' {
            return Some((Token::Array(items), index + 1));
        }
        if let Some((token, next)) = read_token(content, index) {
            if next == index {
                return None;
            }
            items.push(token);
            index = next;
        } else {
            index += 1;
        }
    }
    None
}

fn read_name(content: &[u8], start: usize) -> Option<(Token, usize)> {
    let mut index = start + 1;
    while index < content.len() && is_regular(content[index]) {
        index += 1;
    }
    let name = String::from_utf8_lossy(&content[start + 1..index]).into_owned();
    Some((Token::Name(name), index))
}

fn read_number(content: &[u8], start: usize) -> Option<(Token, usize)> {
    let mut index = start + 1;
    while index < content.len() && matches!(content[index], b'0'..=b'9' | b'.') {
        index += 1;
    }
    let text = std::str::from_utf8(&content[start..index]).ok()?;
    let value = text.parse().ok()?;
    Some((Token::Num(value), index))
}

fn read_op(content: &[u8], start: usize) -> Option<(Token, usize)> {
    let mut index = start + 1;
    while index < content.len() && is_regular(content[index]) {
        index += 1;
    }
    let op = String::from_utf8_lossy(&content[start..index]).into_owned();
    Some((Token::Op(op), index))
}

fn is_regular(byte: u8) -> bool {
    !byte.is_ascii_whitespace()
        && !matches!(byte, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
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

fn page_content(document: &Document, page_id: ObjectId) -> Option<Vec<u8>> {
    let Object::Dictionary(dict) = document.get_object(page_id).ok()? else {
        return None;
    };
    stream_bytes(document, dict.get(b"Contents").ok()?)
}

fn stream_bytes(document: &Document, object: &Object) -> Option<Vec<u8>> {
    match object {
        Object::Stream(stream) => stream.decompressed_content().ok(),
        Object::Reference(id) => match document.get_object(*id).ok()? {
            Object::Stream(stream) => stream.decompressed_content().ok(),
            Object::Array(items) => {
                let mut bytes = Vec::new();
                for item in items {
                    bytes.extend(stream_bytes(document, item)?);
                    bytes.push(b'\n');
                }
                Some(bytes)
            }
            _ => None,
        },
        Object::Array(items) => {
            let mut bytes = Vec::new();
            for item in items {
                bytes.extend(stream_bytes(document, item)?);
                bytes.push(b'\n');
            }
            Some(bytes)
        }
        _ => None,
    }
}

#[cfg(test)]
pub(crate) fn fixture_hello_pdf() -> Vec<u8> {
    let mut document = Document::with_version("1.4");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(
        lopdf::dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica"
        },
    );
    let content_id = document.add_object(lopdf::Stream::new(
        lopdf::dictionary! {},
        b"BT /F1 12 Tf 72 720 Td (Hello) Tj ET".to_vec(),
    ));
    let page_id = document.add_object(
        lopdf::dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Contents" => content_id,
            "Resources" => lopdf::dictionary! {
                "Font" => lopdf::dictionary! {
                    "F1" => font_id
                }
            }
        },
    );
    document.objects.insert(
        pages_id,
        Object::Dictionary(lopdf::dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1
        }),
    );
    let catalog_id = document.add_object(
        lopdf::dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id
        },
    );
    document.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).expect("fixture pdf");
    bytes
}
