# a3s-parse

Local document structure service for A3S OS.

`a3s-parse serve` answers `GET /health` and `POST /v1/parse`. Callers send a
multipart file. The service returns markdown plus anchors that point back at
the original page, paragraph, or cell. It does not summarize the document and
it does not perform OCR. A scanned page returns `422 NEEDS_OCR`.

DOCX, XLSX, and PPTX are decoded with anydoc 0.2.4 (`parser` is
`anydoc/0.2.4`). PDF text positions come from pdf-inspector (`parser` is
`pdf-text/2`). HTML stays on the local tag scanner because anydoc 0.2.4 has
no HTML format. PPTX blocks use paragraph anchors.

A non-loopback listen address requires `A3S_PARSE_API_KEY`. `GET /health`
stays open.

```bash
A3S_PARSE_API_KEY=local-secret cargo run -- serve --listen 127.0.0.1:8091
```
