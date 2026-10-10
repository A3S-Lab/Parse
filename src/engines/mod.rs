mod html;
mod map;
mod office;
pub(crate) mod pdf;
mod text;

use crate::error::ParseError;
use crate::model::{Anchor, BlockType, PageGeom, SpanType};

#[derive(Clone)]
pub(crate) struct RawSpan {
    pub text: String,
    pub span_type: SpanType,
    pub anchors: Vec<Anchor>,
    pub confidence: Option<f64>,
}

#[derive(Clone)]
pub(crate) struct RawBlock {
    pub block_type: BlockType,
    pub text: String,
    pub text_level: Option<u8>,
    pub anchors: Vec<Anchor>,
    pub unanchored: bool,
    pub discarded: bool,
    pub table_continue: bool,
    pub spans: Vec<RawSpan>,
}

pub(crate) struct RawPage {
    pub page_idx: u32,
    pub geom: PageGeom,
    pub failure: Option<String>,
    pub blocks: Vec<RawBlock>,
}

pub(crate) struct RawDocument {
    pub parser: String,
    pub page_count: u32,
    pub pages: Vec<RawPage>,
}

pub(crate) fn resolve_media_type(
    declared: Option<&str>,
    bytes: &[u8],
) -> Result<String, ParseError> {
    let sniffed = sniff(bytes);
    let declared = declared.map(str::trim).filter(|value| !value.is_empty());
    match (declared, sniffed) {
        (None, Some(sniffed)) => Ok(sniffed.to_string()),
        (Some(declared), Some(sniffed)) if same_type(declared, sniffed) => Ok(declared.to_string()),
        (Some(_), Some(_)) => Err(ParseError::InvalidRequest(
            "media type does not match the file bytes".into(),
        )),
        (Some(declared), None) if is_text(declared) && is_utf8_text(bytes) => {
            Ok(declared.to_string())
        }
        (Some(declared), None) if expects_signature(declared) => Err(ParseError::InvalidRequest(
            "media type does not match the file bytes".into(),
        )),
        (Some(declared), None) => Err(ParseError::UnsupportedType(declared.to_string())),
        (None, None) if is_utf8_text(bytes) => Ok("text/plain".into()),
        (None, None) => Err(ParseError::UnsupportedType(
            "application/octet-stream".into(),
        )),
    }
}

pub(crate) fn is_image(media: &str) -> bool {
    matches!(media, "image/png" | "image/jpeg" | "image/webp")
}

pub(crate) fn decode(media: &str, bytes: &[u8]) -> Result<RawDocument, ParseError> {
    match media {
        "text/plain" | "text/markdown" | "text/csv" => Ok(text::decode(bytes)),
        // anydoc 0.2.4 has no HTML format. This scanner stays local.
        "text/html" => Ok(html::decode(bytes)),
        "application/pdf" => pdf::decode(bytes),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            office::decode(media, bytes)
        }
        other => Err(ParseError::UnsupportedType(other.to_string())),
    }
}

fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"%PDF-") {
        return Some("application/pdf");
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"PK\x03\x04") {
        return office::sniff_zip(bytes);
    }
    if looks_like_html(bytes) {
        return Some("text/html");
    }
    None
}

fn looks_like_html(bytes: &[u8]) -> bool {
    let head = bytes.iter().take(256).copied().collect::<Vec<_>>();
    let text = String::from_utf8_lossy(&head).to_ascii_lowercase();
    text.contains("<html") || text.contains("<!doctype html") || text.contains("<body")
}

fn same_type(declared: &str, sniffed: &str) -> bool {
    if declared.eq_ignore_ascii_case(sniffed) {
        return true;
    }
    is_text(declared) && sniffed == "text/html"
}

fn expects_signature(media: &str) -> bool {
    media.eq_ignore_ascii_case("application/pdf")
        || media.eq_ignore_ascii_case("image/png")
        || media.eq_ignore_ascii_case("image/jpeg")
        || media.eq_ignore_ascii_case("image/webp")
        || media.contains("officedocument")
}

fn is_text(media: &str) -> bool {
    matches!(
        media,
        "text/plain" | "text/markdown" | "text/csv" | "text/html"
    )
}

fn is_utf8_text(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok() && !bytes.contains(&0)
}

pub(crate) fn paragraph_anchor(page_idx: u32, index: usize) -> Anchor {
    Anchor {
        kind: crate::model::AnchorKind::Paragraph,
        page_idx,
        bbox: None,
        part: Some(index.to_string()),
    }
}

pub(crate) fn text_block(
    block_type: BlockType,
    text: String,
    anchor: Anchor,
    discarded: bool,
    table_continue: bool,
) -> RawBlock {
    let span = RawSpan {
        text: text.clone(),
        span_type: SpanType::Text,
        anchors: vec![anchor.clone()],
        confidence: None,
    };
    RawBlock {
        block_type,
        text,
        text_level: None,
        anchors: vec![anchor],
        unanchored: false,
        discarded,
        table_continue,
        spans: vec![span],
    }
}
