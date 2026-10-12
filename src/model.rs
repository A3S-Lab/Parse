/// How a block points back at the original document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorKind {
    /// PDF box. `bbox` uses `norm-1000`, origin top-left, y down.
    PageBox,
    /// Worksheet cell. `part` is `sheet:row:col`.
    Cell,
    /// Paragraph index. `part` is the index. No page box is invented.
    Paragraph,
}

impl AnchorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PageBox => "page-box",
            Self::Cell => "cell",
            Self::Paragraph => "paragraph",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub kind: AnchorKind,
    pub page_idx: u32,
    pub bbox: Option<[u32; 4]>,
    pub part: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    Text,
    Title,
    List,
    Table,
    Image,
    Equation,
    Code,
    PageNumber,
}

impl BlockType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Title => "title",
            Self::List => "list",
            Self::Table => "table",
            Self::Image => "image",
            Self::Equation => "equation",
            Self::Code => "code",
            Self::PageNumber => "page_number",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanType {
    Text,
    InterlineEquation,
}

impl SpanType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::InterlineEquation => "interline_equation",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub id: String,
    pub block_id: String,
    pub line: u32,
    pub text: String,
    pub span_type: SpanType,
    pub anchors: Vec<Anchor>,
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub id: String,
    pub order: u32,
    pub block_type: BlockType,
    pub text: String,
    pub text_level: Option<u8>,
    pub anchors: Vec<Anchor>,
    pub unanchored: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageGeom {
    pub page_idx: u32,
    pub width_pt: f64,
    pub height_pt: f64,
    pub rotation: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownSpan {
    pub start: usize,
    pub end: usize,
    pub block_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageFailure {
    pub page_idx: u32,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParseDocument {
    pub media_type: String,
    pub parser: String,
    pub page_count: u32,
    pub from_page: u32,
    pub to_page: u32,
    pub truncated: bool,
    pub next_page: Option<u32>,
    pub markdown: String,
    pub blocks: Vec<Block>,
    pub spans: Vec<Span>,
    pub markdown_map: Vec<MarkdownSpan>,
    pub discarded_count: u32,
    pub discarded: Vec<Block>,
    pub pages_failed: Vec<PageFailure>,
    pub page_geom: Vec<PageGeom>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Markdown,
    Blocks,
    Spans,
    Discarded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseMode {
    Auto,
    Text,
    Ocr,
}

#[derive(Debug, Clone)]
pub struct ParseRequest {
    pub bytes: Vec<u8>,
    pub media_type: Option<String>,
    pub pages: Option<String>,
    pub outputs: Vec<OutputKind>,
    pub mode: ParseMode,
}

impl ParseRequest {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            media_type: None,
            pages: None,
            outputs: vec![OutputKind::Markdown, OutputKind::Blocks],
            mode: ParseMode::Auto,
        }
    }
}

/// `x_pt = x / 1000 * width_pt`. `x` is a `norm-1000` coordinate.
pub fn restore_x_pt(norm: u32, width_pt: f64) -> f64 {
    f64::from(norm) / 1000.0 * width_pt
}

/// `y_pt = y / 1000 * height_pt`, measured from the top of the page.
pub fn restore_y_pt(norm: u32, height_pt: f64) -> f64 {
    f64::from(norm) / 1000.0 * height_pt
}

pub(crate) fn norm_box_from_pdf(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    width: f64,
    height: f64,
) -> Option<[u32; 4]> {
    if !(width.is_finite() && height.is_finite()) || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let (left, right) = if x1 >= x0 { (x0, x1) } else { (x1, x0) };
    let (bottom, top) = if y1 >= y0 { (y0, y1) } else { (y1, y0) };
    Some([
        scale_norm(left / width),
        scale_norm((height - top) / height),
        scale_norm(right / width),
        scale_norm((height - bottom) / height),
    ])
}

fn scale_norm(unit: f64) -> u32 {
    if !unit.is_finite() {
        return 0;
    }
    unit.clamp(0.0, 1.0).mul_add(1000.0, 0.0).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_uses_the_norm_1000_formula() {
        assert_eq!(restore_x_pt(500, 612.0), 306.0);
        assert_eq!(restore_y_pt(250, 792.0), 198.0);
    }

    #[test]
    fn pdf_box_origin_is_top_left() {
        let box_ = norm_box_from_pdf(0.0, 692.0, 100.0, 792.0, 612.0, 792.0).unwrap();
        assert_eq!(box_[1], 0);
        assert!(box_[3] > box_[1]);
    }
}
