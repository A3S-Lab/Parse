use std::io::{Cursor, Read};

use quick_xml::events::Event;
use quick_xml::Reader;
use zip::ZipArchive;

use super::{paragraph_anchor, text_block, RawDocument, RawPage};
use crate::error::ParseError;
use crate::model::{Anchor, AnchorKind, BlockType, PageGeom};

const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const PPTX: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";

pub(crate) fn sniff_zip(bytes: &[u8]) -> Option<&'static str> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).ok()?;
    let mut names = Vec::new();
    for index in 0..archive.len().min(64) {
        if let Ok(file) = archive.by_index(index) {
            names.push(file.name().to_string());
        }
    }
    if names.iter().any(|name| name == "word/document.xml") {
        Some(DOCX)
    } else if names.iter().any(|name| name.starts_with("xl/")) {
        Some(XLSX)
    } else if names.iter().any(|name| name.starts_with("ppt/")) {
        Some(PPTX)
    } else {
        None
    }
}

pub(crate) fn decode(media: &str, bytes: &[u8]) -> Result<RawDocument, ParseError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| ParseError::Unavailable(format!("office package could not be read: {error}")))?;
    match media {
        DOCX => decode_docx(&mut archive),
        XLSX => decode_xlsx(&mut archive),
        PPTX => decode_pptx(&mut archive),
        other => Err(ParseError::UnsupportedType(other.to_string())),
    }
}

fn decode_docx(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Result<RawDocument, ParseError> {
    let xml = read_entry(archive, "word/document.xml")?;
    let paragraphs = xml_texts(&xml, "p", "t");
    if paragraphs.is_empty() {
        return Err(ParseError::InvalidRequest("docx has no paragraphs".into()));
    }
    let blocks = paragraphs
        .into_iter()
        .enumerate()
        .map(|(index, text)| text_block(BlockType::Text, text, paragraph_anchor(0, index), false, false))
        .collect();
    Ok(one_page("docx/1", blocks))
}

fn decode_xlsx(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Result<RawDocument, ParseError> {
    let shared = archive
        .by_name("xl/sharedStrings.xml")
        .ok()
        .and_then(|mut file| read_all(&mut file).ok())
        .map(|bytes| xml_texts(&bytes, "si", "t"))
        .unwrap_or_default();
    let sheet_name = sheet_name(archive)?;
    let xml = read_entry(archive, &sheet_name)?;
    let cells = xlsx_cells(&xml, &shared);
    if cells.is_empty() {
        return Err(ParseError::InvalidRequest("xlsx has no cells".into()));
    }
    let blocks = cells
        .into_iter()
        .map(|(row, col, text)| {
            let anchor = Anchor {
                kind: AnchorKind::Cell,
                page_idx: 0,
                bbox: None,
                part: Some(format!("{sheet_name}:{row}:{col}")),
            };
            text_block(BlockType::Text, text, anchor, false, false)
        })
        .collect();
    Ok(one_page("xlsx/1", blocks))
}

fn decode_pptx(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Result<RawDocument, ParseError> {
    let (width, height) = slide_size(archive).unwrap_or((0.0, 0.0));
    let mut names = Vec::new();
    for index in 0..archive.len().min(128) {
        if let Ok(file) = archive.by_index(index) {
            let name = file.name().to_string();
            if name.starts_with("ppt/slides/slide") && name.ends_with(".xml") {
                names.push(name);
            }
        }
    }
    names.sort();
    if names.is_empty() {
        return Err(ParseError::InvalidRequest("pptx has no slides".into()));
    }
    let mut pages = Vec::new();
    for (page_idx, name) in names.into_iter().enumerate() {
        let xml = read_entry(archive, &name)?;
        let shapes = pptx_shapes(&xml, width, height, page_idx as u32);
        pages.push(RawPage {
            page_idx: page_idx as u32,
            geom: PageGeom {
                page_idx: page_idx as u32,
                width_pt: width,
                height_pt: height,
                rotation: 0,
            },
            failure: None,
            blocks: shapes,
        });
    }
    let page_count = pages.len() as u32;
    Ok(RawDocument {
        parser: "pptx/1".into(),
        page_count,
        pages,
    })
}

fn one_page(parser: &str, blocks: Vec<super::RawBlock>) -> RawDocument {
    RawDocument {
        parser: parser.into(),
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

fn sheet_name(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Result<String, ParseError> {
    for index in 0..archive.len().min(128) {
        if let Ok(file) = archive.by_index(index) {
            let name = file.name().to_string();
            if name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml") {
                return Ok(name);
            }
        }
    }
    Err(ParseError::InvalidRequest("xlsx has no worksheet".into()))
}

fn slide_size(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Option<(f64, f64)> {
    let bytes = read_entry(archive, "ppt/presentation.xml").ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let cx = attr_f64(&text, "cx")?;
    let cy = attr_f64(&text, "cy")?;
    Some((cx, cy))
}

fn attr_f64(xml: &str, name: &str) -> Option<f64> {
    let needle = format!("{name}=\"");
    let start = xml.find(&needle)? + needle.len();
    let end = xml[start..].find('"')? + start;
    xml[start..end].parse().ok()
}

fn read_entry(archive: &mut ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<Vec<u8>, ParseError> {
    let mut file = archive
        .by_name(name)
        .map_err(|_| ParseError::InvalidRequest(format!("office package is missing {name}")))?;
    read_all(&mut file)
}

fn read_all(file: &mut zip::read::ZipFile<'_>) -> Result<Vec<u8>, ParseError> {
    let mut bytes = Vec::new();
    file.take(8 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|error| ParseError::Unavailable(format!("office part could not be read: {error}")))?;
    Ok(bytes)
}

fn xml_texts(bytes: &[u8], block: &str, text: &str) -> Vec<String> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut items = Vec::new();
    let mut current = String::new();
    let mut in_block = false;
    let mut in_text = false;
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = local_name(event.name().as_ref());
                if name == block {
                    in_block = true;
                    current.clear();
                }
                in_text = name == text;
            }
            Ok(Event::Empty(event)) => {
                if local_name(event.name().as_ref()) == block {
                    items.push(String::new());
                }
            }
            Ok(Event::Text(event)) if in_block && in_text => {
                current.push_str(&xml_text(&event));
            }
            Ok(Event::End(event)) => {
                let name = local_name(event.name().as_ref());
                if name == text {
                    in_text = false;
                }
                if name == block {
                    let value = current.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !value.is_empty() {
                        items.push(value);
                    }
                    in_block = false;
                    current.clear();
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buffer.clear();
    }
    items
}

fn xlsx_cells(bytes: &[u8], shared: &[String]) -> Vec<(u32, u32, String)> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut cells = Vec::new();
    let mut reference = String::new();
    let mut shared_string = false;
    let mut in_value = false;
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "c" {
                    reference.clear();
                    shared_string = false;
                    for attribute in event.attributes().flatten() {
                        if attribute.key.as_ref() == b"r" {
                            reference = String::from_utf8_lossy(&attribute.value).into_owned();
                        }
                        if attribute.key.as_ref() == b"t" && attribute.value.as_ref() == b"s" {
                            shared_string = true;
                        }
                    }
                }
                in_value = name == "v";
            }
            Ok(Event::Text(event)) if in_value => {
                let raw = xml_text(&event);
                let text = if shared_string {
                    raw.parse::<usize>()
                        .ok()
                        .and_then(|index| shared.get(index))
                        .cloned()
                        .unwrap_or_default()
                } else {
                    raw.to_string()
                };
                if let Some((col, row)) = split_cell(&reference) {
                    if !text.is_empty() {
                        cells.push((row, col, text));
                    }
                }
            }
            Ok(Event::End(event)) if local_name(event.name().as_ref()) == "v" => in_value = false,
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buffer.clear();
    }
    cells
}

fn split_cell(reference: &str) -> Option<(u32, u32)> {
    let split = reference.find(|ch: char| ch.is_ascii_digit())?;
    let column = reference[..split].chars().fold(0u32, |value, ch| {
        value.saturating_mul(26).saturating_add(ch.to_ascii_uppercase() as u32 - b'A' as u32 + 1)
    });
    let row = reference[split..].parse().ok()?;
    Some((column, row))
}

fn pptx_shapes(bytes: &[u8], width: f64, height: f64, page_idx: u32) -> Vec<super::RawBlock> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut blocks = Vec::new();
    let mut off_x = None;
    let mut off_y = None;
    let mut ext_x = None;
    let mut ext_y = None;
    let mut in_text = false;
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Empty(event)) | Ok(Event::Start(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "off" {
                    off_x = attribute_f64(&event, b"x");
                    off_y = attribute_f64(&event, b"y");
                }
                if name == "ext" {
                    ext_x = attribute_f64(&event, b"cx");
                    ext_y = attribute_f64(&event, b"cy");
                }
                in_text = name == "t";
            }
            Ok(Event::Text(event)) if in_text => {
                let text = xml_text(&event);
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if !text.is_empty() {
                    let bbox = match (off_x, off_y, ext_x, ext_y) {
                        (Some(x), Some(y), Some(cx), Some(cy)) if width > 0.0 && height > 0.0 => {
                            Some(norm_emu(x, y, cx, cy, width, height))
                        }
                        _ => None,
                    };
                    let anchor = Anchor {
                        kind: AnchorKind::SlideBox,
                        page_idx,
                        bbox,
                        part: Some(page_idx.to_string()),
                    };
                    let mut block = text_block(BlockType::Text, text, anchor, false, false);
                    block.unanchored = bbox.is_none();
                    blocks.push(block);
                }
            }
            Ok(Event::End(event)) if local_name(event.name().as_ref()) == "sp" => {
                off_x = None;
                off_y = None;
                ext_x = None;
                ext_y = None;
            }
            Ok(Event::End(event)) if local_name(event.name().as_ref()) == "t" => in_text = false,
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buffer.clear();
    }
    blocks
}

fn attribute_f64(event: &quick_xml::events::BytesStart<'_>, key: &[u8]) -> Option<f64> {
    event.attributes().flatten().find_map(|attribute| {
        if attribute.key.as_ref() == key {
            String::from_utf8_lossy(&attribute.value).parse().ok()
        } else {
            None
        }
    })
}

fn norm_emu(x: f64, y: f64, cx: f64, cy: f64, width: f64, height: f64) -> [u32; 4] {
    let scale = |value: f64, span: f64| ((value / span) * 1000.0).clamp(0.0, 1000.0).round() as u32;
    [scale(x, width), scale(y, height), scale(x + cx, width), scale(y + cy, height)]
}

fn xml_text(event: &quick_xml::events::BytesText<'_>) -> String {
    String::from_utf8_lossy(event.as_ref()).into_owned()
}

fn local_name(name: &[u8]) -> String {
    let text = String::from_utf8_lossy(name);
    text.rsplit(':').next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn package(files: &[(&str, &str)]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = ZipWriter::new(&mut cursor);
            for (name, body) in files {
                zip.start_file(*name, SimpleFileOptions::default()).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn docx_uses_paragraph_anchors_and_no_page_box() {
        let bytes = package(&[(
            "word/document.xml",
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p></w:body></w:document>"#,
        )]);
        let document = decode(DOCX, &bytes).unwrap();
        let anchor = &document.pages[0].blocks[0].anchors[0];
        assert_eq!(anchor.kind, AnchorKind::Paragraph);
        assert!(anchor.bbox.is_none());
        assert_eq!(document.pages[0].blocks[0].text, "Hello");
    }
}
