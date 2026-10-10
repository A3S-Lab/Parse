//! Document structure parsing for `POST /v1/parse`.
//!
//! The service returns blocks and spans that point at the original page.
//! Markdown is a reading projection of those blocks.

mod assemble;
mod engines;
mod error;
mod http;
mod model;
mod service;

pub use error::ParseError;
pub use http::serve_native;
pub use model::{
    restore_x_pt, restore_y_pt, Anchor, AnchorKind, Block, BlockType, OutputKind, PageGeom,
    ParseDocument, ParseMode, ParseRequest,
};

pub fn parse_document(request: &ParseRequest) -> Result<ParseDocument, ParseError> {
    assemble::parse_document(request)
}
