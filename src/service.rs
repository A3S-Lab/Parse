//! Native document parse service.
//!
//! `a3s-parse serve` answers `GET /health` and `POST /v1/parse`.

use std::time::Duration;

use serde_json::{json, Value};

use crate::model::{OutputKind, ParseDocument, ParseMode, ParseRequest};
use crate::ParseError;

const PARSE_TIMEOUT: Duration = Duration::from_secs(25);

pub(crate) enum ParseReply {
    Json { status: u16, body: Value },
}

pub(crate) fn reply(status: u16, code: &str, message: &str) -> ParseReply {
    ParseReply::Json {
        status,
        body: json!({
            "success": false,
            "error": message,
            "code": code,
        }),
    }
}

pub(crate) fn reply_for(error: ParseError) -> ParseReply {
    match error {
        ParseError::InvalidRequest(message) => reply(400, "INVALID_REQUEST", &message),
        ParseError::UnsupportedType(message) => reply(415, "UNSUPPORTED_TYPE", &message),
        ParseError::NeedsOcr => reply(422, "NEEDS_OCR", "document needs OCR"),
        ParseError::Unavailable(message) => reply(503, "UNAVAILABLE", &message),
    }
}

pub(crate) struct IncomingParse {
    pub request: ParseRequest,
}

pub(crate) fn parse_multipart(
    content_type: &str,
    body: &[u8],
) -> Result<IncomingParse, ParseReply> {
    if content_type.to_ascii_lowercase().contains("application/json") {
        return Err(reply(
            400,
            "INVALID_REQUEST",
            "file must be multipart",
        ));
    }
    let boundary = content_type.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        name.eq_ignore_ascii_case("boundary").then(|| value.trim().trim_matches('"').to_string())
    });
    let Some(boundary) = boundary else {
        return Err(reply(400, "INVALID_REQUEST", "file must be multipart"));
    };
    let mut file = None;
    let mut media_type = None;
    let mut pages = None;
    let mut mode = ParseMode::Auto;
    let mut outputs = Vec::new();
    for (name, value) in split_parts(body, &boundary) {
        match name.as_str() {
            "file" => file = Some(value),
            "media_type" => media_type = Some(String::from_utf8_lossy(&value).trim().to_string()),
            "pages" => pages = Some(String::from_utf8_lossy(&value).trim().to_string()),
            "mode" => {
                mode = match String::from_utf8_lossy(&value).trim() {
                    "auto" => ParseMode::Auto,
                    "text" => ParseMode::Text,
                    "ocr" => ParseMode::Ocr,
                    _ => {
                        return Err(reply(
                            400,
                            "INVALID_REQUEST",
                            "mode must be auto, text, or ocr",
                        ))
                    }
                }
            }
            "outputs" => {
                for item in String::from_utf8_lossy(&value).split(|ch: char| ch == ',' || ch.is_whitespace()) {
                    if item.is_empty() {
                        continue;
                    }
                    outputs.push(match item {
                        "markdown" => OutputKind::Markdown,
                        "blocks" => OutputKind::Blocks,
                        "spans" => OutputKind::Spans,
                        "discarded" => OutputKind::Discarded,
                        _ => {
                            return Err(reply(
                                400,
                                "INVALID_REQUEST",
                                "outputs must be markdown, blocks, spans, or discarded",
                            ))
                        }
                    });
                }
            }
            _ => {}
        }
    }
    let Some(bytes) = file.filter(|bytes| !bytes.is_empty()) else {
        return Err(reply(400, "INVALID_REQUEST", "file is required"));
    };
    if outputs.is_empty() {
        outputs = vec![OutputKind::Markdown, OutputKind::Blocks];
    }
    Ok(IncomingParse {
        request: ParseRequest {
            bytes,
            media_type,
            pages,
            outputs,
            mode,
        },
    })
}

fn split_parts(body: &[u8], boundary: &str) -> Vec<(String, Vec<u8>)> {
    let marker = format!("--{boundary}");
    let text_marker = marker.as_bytes();
    let mut parts = Vec::new();
    let mut rest = body;
    while let Some(start) = find_subsequence(rest, text_marker) {
        let after = &rest[start + text_marker.len()..];
        if after.starts_with(b"--") {
            break;
        }
        let after = after.strip_prefix(b"\r\n").unwrap_or(after);
        let Some(header_end) = find_subsequence(after, b"\r\n\r\n") else {
            break;
        };
        let headers = String::from_utf8_lossy(&after[..header_end]);
        let content = &after[header_end + 4..];
        let next = find_subsequence(content, text_marker).unwrap_or(content.len());
        let mut value = content[..next].to_vec();
        if value.ends_with(b"\r\n") {
            value.truncate(value.len() - 2);
        }
        if let Some(name) = disposition_name(&headers) {
            parts.push((name, value));
        }
        rest = &content[next..];
    }
    parts
}

fn disposition_name(headers: &str) -> Option<String> {
    for line in headers.lines() {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("content-disposition") {
            continue;
        }
        for piece in value.split(';') {
            let piece = piece.trim();
            if let Some(raw) = piece.strip_prefix("name=") {
                return Some(raw.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

pub(crate) fn document_json(document: &ParseDocument) -> Value {
    json!({
        "success": true,
        "data": {
            "document": {
                "media_type": document.media_type,
                "parser": document.parser,
                "page_count": document.page_count,
                "pages": {
                    "from": document.from_page,
                    "to": document.to_page,
                    "truncated": document.truncated,
                    "next": document.next_page,
                },
                "markdown": document.markdown,
                "blocks": document.blocks.iter().map(|block| json!({
                    "id": block.id,
                    "order": block.order,
                    "type": block.block_type.as_str(),
                    "text": block.text,
                    "text_level": block.text_level,
                    "unanchored": block.unanchored,
                    "anchors": block.anchors.iter().map(anchor_json).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "spans": document.spans.iter().map(|span| json!({
                    "id": span.id,
                    "block_id": span.block_id,
                    "line": span.line,
                    "text": span.text,
                    "type": span.span_type.as_str(),
                    "confidence": span.confidence,
                    "anchors": span.anchors.iter().map(anchor_json).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "markdown_map": document.markdown_map.iter().map(|item| json!({
                    "start": item.start,
                    "end": item.end,
                    "block_id": item.block_id,
                })).collect::<Vec<_>>(),
                "discarded_count": document.discarded_count,
                "discarded": document.discarded.iter().map(|block| json!({
                    "id": block.id,
                    "type": block.block_type.as_str(),
                    "text": block.text,
                })).collect::<Vec<_>>(),
                "pages_failed": document.pages_failed.iter().map(|page| json!({
                    "page_idx": page.page_idx,
                    "reason": page.reason,
                })).collect::<Vec<_>>(),
                "page_geom": document.page_geom.iter().map(|page| json!({
                    "page_idx": page.page_idx,
                    "width_pt": page.width_pt,
                    "height_pt": page.height_pt,
                    "rotation": page.rotation,
                })).collect::<Vec<_>>(),
            }
        }
    })
}

fn anchor_json(anchor: &crate::model::Anchor) -> Value {
    json!({
        "kind": anchor.kind.as_str(),
        "page_idx": anchor.page_idx,
        "bbox": anchor.bbox,
        "space": anchor.bbox.as_ref().map(|_| "norm-1000"),
        "part": anchor.part,
    })
}

pub(crate) async fn run_parse(request: ParseRequest) -> Result<ParseDocument, ParseError> {
    let handle = tokio::task::spawn_blocking(move || crate::parse_document(&request));
    match tokio::time::timeout(PARSE_TIMEOUT, handle).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(ParseError::Unavailable("parse worker stopped".into())),
        Err(_) => Err(ParseError::Unavailable("parse timed out".into())),
    }
}
