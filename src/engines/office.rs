use std::io::Cursor;

use anydoc::ConvertError;
use anydoc::Format;
use zip::ZipArchive;

use super::map;
use crate::error::ParseError;

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

pub(crate) fn decode(media: &str, bytes: &[u8]) -> Result<super::RawDocument, ParseError> {
    let (format, cell_parts) = match media {
        DOCX => (Format::Docx, false),
        XLSX => (Format::Excel, true),
        PPTX => (Format::Pptx, false),
        other => return Err(ParseError::UnsupportedType(other.to_string())),
    };
    let document = anydoc::to_document(bytes, format).map_err(map_convert)?;
    map::document_to_raw(&document, "anydoc/0.2.4", cell_parts)
}

fn map_convert(error: ConvertError) -> ParseError {
    match error {
        ConvertError::Unsupported(detail) => ParseError::UnsupportedType(detail),
        ConvertError::NeedsOcr { .. } => ParseError::NeedsOcr,
        ConvertError::Malformed { detail, .. } => ParseError::InvalidRequest(detail),
        ConvertError::Encrypted => ParseError::InvalidRequest("document is encrypted".into()),
        ConvertError::ResourceLimit { detail, .. } => ParseError::InvalidRequest(detail),
        ConvertError::MissingPart { part } => ParseError::InvalidRequest(format!("missing {part}")),
        ConvertError::Io(error) => ParseError::Unavailable(error.to_string()),
        other => ParseError::Unavailable(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AnchorKind;
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
        assert_eq!(document.parser, "anydoc/0.2.4");
    }
}
