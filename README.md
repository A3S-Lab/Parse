# a3s-parse

Local document structure service for A3S OS.

`a3s-parse serve` answers `GET /health` and `POST /v1/parse`. Callers send a
multipart file. The service returns markdown plus anchors that point back at
the original page, paragraph, cell, or slide. It does not summarize the
document and it does not perform OCR. A scanned page returns `422 NEEDS_OCR`.

A non-loopback listen address requires `A3S_PARSE_API_KEY`. `GET /health`
stays open.

```bash
A3S_PARSE_API_KEY=local-secret cargo run -- serve --listen 127.0.0.1:8091
```
