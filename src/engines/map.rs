use anydoc::model::{inlines_to_plain_text, Block, CellSlot, Document, Inline, List};

use super::{paragraph_anchor, RawBlock, RawDocument, RawPage, RawSpan};
use crate::error::ParseError;
use crate::model::{Anchor, AnchorKind, BlockType, PageGeom, SpanType};

pub(crate) fn document_to_raw(
    document: &Document,
    parser: &str,
    cell_parts: bool,
) -> Result<RawDocument, ParseError> {
    let mut mapper = Mapper {
        index: 0,
        sheet: "sheet1".to_string(),
        cell_parts,
        blocks: Vec::new(),
    };
    for block in &document.blocks {
        mapper.push_block(block);
    }
    for note in &document.notes {
        for block in &note.blocks {
            mapper.push_block(block);
        }
    }
    if mapper.blocks.is_empty() {
        return Err(ParseError::InvalidRequest("document has no text".into()));
    }
    Ok(RawDocument {
        parser: parser.to_string(),
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
            blocks: mapper.blocks,
        }],
    })
}

struct Mapper {
    index: usize,
    sheet: String,
    cell_parts: bool,
    blocks: Vec<RawBlock>,
}

impl Mapper {
    fn push_block(&mut self, block: &Block) {
        match block {
            Block::Heading { level, content, .. } => {
                let text = inlines_to_plain_text(content);
                if self.cell_parts && !text.trim().is_empty() {
                    self.sheet = text.trim().to_string();
                }
                self.emit(
                    BlockType::Title,
                    text,
                    Some(*level),
                    self.paragraph(),
                    SpanType::Text,
                );
            }
            Block::Paragraph(inlines) => {
                let text = inlines_to_plain_text(inlines);
                let kind = if inlines
                    .iter()
                    .all(|inline| matches!(inline, Inline::Image { .. }))
                    && inlines
                        .iter()
                        .any(|inline| matches!(inline, Inline::Image { .. }))
                {
                    BlockType::Image
                } else {
                    BlockType::Text
                };
                self.emit(kind, text, None, self.paragraph(), SpanType::Text);
            }
            Block::List(list) => {
                self.emit(
                    BlockType::List,
                    list_text(list),
                    None,
                    self.paragraph(),
                    SpanType::Text,
                );
            }
            Block::Table(table) => self.push_table(table),
            Block::BlockQuote(blocks) => {
                for block in blocks {
                    self.push_block(block);
                }
            }
            Block::CodeBlock { text, .. } => {
                self.emit(
                    BlockType::Code,
                    text.clone(),
                    None,
                    self.paragraph(),
                    SpanType::Text,
                );
            }
            Block::Math(tex) => {
                self.emit(
                    BlockType::Equation,
                    tex.clone(),
                    None,
                    self.paragraph(),
                    SpanType::InterlineEquation,
                );
            }
            Block::Rule => {}
        }
    }

    fn push_table(&mut self, table: &anydoc::model::Table) {
        if self.cell_parts {
            for (row_index, row) in table.grid.iter().enumerate() {
                for (col_index, slot) in row.iter().enumerate() {
                    let CellSlot::Origin(cell) = slot else {
                        continue;
                    };
                    let text = cell
                        .blocks
                        .iter()
                        .map(block_text)
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    let anchor = Anchor {
                        kind: AnchorKind::Cell,
                        page_idx: 0,
                        bbox: None,
                        part: Some(format!(
                            "{}:{}:{}",
                            self.sheet,
                            row_index + 1,
                            col_index + 1
                        )),
                    };
                    self.emit(BlockType::Text, text, None, anchor, SpanType::Text);
                }
            }
            return;
        }
        let mut rows = Vec::new();
        for row in &table.grid {
            let cells = row
                .iter()
                .filter_map(|slot| match slot {
                    CellSlot::Origin(cell) => {
                        let text = cell
                            .blocks
                            .iter()
                            .map(block_text)
                            .filter(|text| !text.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ");
                        Some(text)
                    }
                    CellSlot::Covered { .. } => None,
                })
                .collect::<Vec<_>>();
            if cells.iter().any(|cell| !cell.is_empty()) {
                rows.push(cells.join(" | "));
            }
        }
        self.emit(
            BlockType::Table,
            rows.join("\n"),
            None,
            self.paragraph(),
            SpanType::Text,
        );
    }

    fn paragraph(&self) -> Anchor {
        paragraph_anchor(0, self.index)
    }

    fn emit(
        &mut self,
        block_type: BlockType,
        text: String,
        text_level: Option<u8>,
        anchor: Anchor,
        span_type: SpanType,
    ) {
        if text.trim().is_empty() {
            return;
        }
        let span = RawSpan {
            text: text.clone(),
            span_type,
            anchors: vec![anchor.clone()],
            confidence: None,
        };
        self.blocks.push(RawBlock {
            block_type,
            text,
            text_level,
            anchors: vec![anchor],
            unanchored: false,
            discarded: false,
            table_continue: false,
            spans: vec![span],
        });
        self.index += 1;
    }
}

fn list_text(list: &List) -> String {
    list.items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let label = item
                .marker_label
                .clone()
                .unwrap_or_else(|| list.marker.label(list.start.saturating_add(index as u64)));
            let body = item
                .blocks
                .iter()
                .map(block_text)
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            format!("{label} {body}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn block_text(block: &Block) -> String {
    match block {
        Block::Heading { content, .. } | Block::Paragraph(content) => {
            inlines_to_plain_text(content)
        }
        Block::List(list) => list_text(list),
        Block::Table(table) => table
            .grid
            .iter()
            .flat_map(|row| row.iter())
            .filter_map(|slot| match slot {
                CellSlot::Origin(cell) => {
                    let text = cell
                        .blocks
                        .iter()
                        .map(block_text)
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join(" ");
                    (!text.is_empty()).then_some(text)
                }
                CellSlot::Covered { .. } => None,
            })
            .collect::<Vec<_>>()
            .join(" | "),
        Block::BlockQuote(blocks) => blocks
            .iter()
            .map(block_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        Block::CodeBlock { text, .. } | Block::Math(text) => text.clone(),
        Block::Rule => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anydoc::model::{Cell, Table};

    #[test]
    fn a_heading_keeps_its_level_and_a_paragraph_anchor() {
        let document = Document {
            blocks: vec![Block::heading(2, vec![Inline::plain("Title")])],
            ..Document::default()
        };
        let raw = document_to_raw(&document, "anydoc/0.2.4", false).unwrap();
        let block = &raw.pages[0].blocks[0];
        assert_eq!(block.block_type, BlockType::Title);
        assert_eq!(block.text_level, Some(2));
        assert_eq!(block.anchors[0].kind, AnchorKind::Paragraph);
        assert!(block.anchors[0].bbox.is_none());
    }

    #[test]
    fn a_docx_table_stays_one_block() {
        let table = Table {
            grid: vec![vec![
                CellSlot::Origin(Cell::from_inlines(vec![Inline::plain("A")])),
                CellSlot::Origin(Cell::from_inlines(vec![Inline::plain("B")])),
            ]],
            ..Table::default()
        };
        let document = Document {
            blocks: vec![Block::Table(table)],
            ..Document::default()
        };
        let raw = document_to_raw(&document, "anydoc/0.2.4", false).unwrap();
        let block = &raw.pages[0].blocks[0];
        assert_eq!(block.block_type, BlockType::Table);
        assert_eq!(block.text, "A | B");
        assert_eq!(block.anchors[0].kind, AnchorKind::Paragraph);
    }

    #[test]
    fn a_sheet_cell_records_the_sheet_row_and_column() {
        let table = Table {
            grid: vec![vec![CellSlot::Origin(Cell::from_inlines(vec![
                Inline::plain("Hello"),
            ]))]],
            ..Table::default()
        };
        let document = Document {
            blocks: vec![
                Block::heading(2, vec![Inline::plain("Data")]),
                Block::Table(table),
            ],
            ..Document::default()
        };
        let raw = document_to_raw(&document, "anydoc/0.2.4", true).unwrap();
        let cell = raw.pages[0]
            .blocks
            .iter()
            .find(|block| block.anchors[0].kind == AnchorKind::Cell)
            .expect("cell");
        assert_eq!(cell.anchors[0].part.as_deref(), Some("Data:1:1"));
        assert_eq!(cell.text, "Hello");
    }
}
