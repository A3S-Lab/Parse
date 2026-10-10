//! HTTP front for the native parse service.

use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::service::{document_json, parse_multipart, reply, reply_for, run_parse, ParseReply};

const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

pub(crate) fn parse_listen_addr(listen: &str) -> Result<SocketAddr, String> {
    listen
        .parse()
        .map_err(|_| format!("listen address must be host:port, got {listen}"))
}

pub(crate) fn public_bind_requires_key(
    addr: SocketAddr,
    api_key: Option<&str>,
) -> Result<(), String> {
    if addr.ip().is_loopback() || api_key.is_some_and(|key| !key.is_empty()) {
        Ok(())
    } else {
        Err("a non-loopback parse listener requires A3S_PARSE_API_KEY".to_string())
    }
}

pub(crate) fn deployment_api_key() -> Option<String> {
    std::env::var("A3S_PARSE_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub async fn serve_native(listen: &str) -> Result<(), String> {
    let addr = parse_listen_addr(listen)?;
    let api_key = deployment_api_key();
    public_bind_requires_key(addr, api_key.as_deref())?;
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|error| error.to_string())?;
    let bound = listener.local_addr().map_err(|error| error.to_string())?;
    tracing::info!(%bound, "native parse service listening");
    serve_on(listener, api_key)
        .await
        .map_err(|error| error.to_string())
}

pub(crate) async fn serve_on(
    listener: TcpListener,
    api_key: Option<String>,
) -> std::io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let api_key = api_key.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, api_key).await {
                tracing::debug!(%error, "parse connection ended");
            }
        });
    }
}

async fn handle_connection(mut stream: TcpStream, api_key: Option<String>) -> std::io::Result<()> {
    let received = match read_http(&mut stream).await? {
        Some(received) => received,
        None => return Ok(()),
    };
    let reply = dispatch(&received, api_key.as_deref()).await;
    write_json(&mut stream, &reply).await
}

struct Received {
    method: String,
    path: String,
    headers: String,
    body: Vec<u8>,
}

async fn dispatch(received: &Received, api_key: Option<&str>) -> ParseReply {
    match (received.method.as_str(), received.path.as_str()) {
        ("GET", "/health") => ParseReply::Json {
            status: 200,
            body: serde_json::json!({"status": "ok"}),
        },
        ("POST", "/v1/parse") => {
            if let Some(expected) = api_key {
                if !presented_key(&received.headers)
                    .is_some_and(|presented| keys_match(expected, &presented))
                {
                    return reply(401, "UNAUTHORIZED", "parse credential was rejected");
                }
            }
            let content_type = header_value(&received.headers, "content-type").unwrap_or_default();
            match parse_multipart(&content_type, &received.body) {
                Ok(incoming) => match run_parse(incoming.request).await {
                    Ok(document) => ParseReply::Json {
                        status: 200,
                        body: document_json(&document),
                    },
                    Err(error) => reply_for(error),
                },
                Err(reply) => reply,
            }
        }
        _ => reply(404, "NOT_FOUND", "parse route was not found"),
    }
}

fn header_value(headers: &str, wanted: &str) -> Option<String> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(wanted)
            .then(|| value.trim().to_string())
    })
}

fn presented_key(headers: &str) -> Option<String> {
    let mut authorization = None;
    for line in headers.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("authorization") {
            authorization = bearer(value.trim());
        }
    }
    authorization
}

fn bearer(value: &str) -> Option<String> {
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

fn keys_match(expected: &str, presented: &str) -> bool {
    let left = expected.as_bytes();
    let right = presented.as_bytes();
    let mut diff = left.len() ^ right.len();
    let width = left.len().max(right.len());
    for index in 0..width {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(a ^ b);
    }
    diff == 0
}

async fn read_http(stream: &mut TcpStream) -> std::io::Result<Option<Received>> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];
    let header_end = loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_BODY_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "parse request exceeds 32MiB",
            ));
        }
        if let Some(offset) = find_headers_end(&buffer) {
            break offset;
        }
        if buffer.len() > 64 * 1024 && find_headers_end(&buffer).is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "parse headers are too large",
            ));
        }
    };
    let header_text = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = header_text.lines();
    let Some(request_line) = lines.next() else {
        return Ok(None);
    };
    let mut parts = request_line.split_whitespace();
    let Some(method) = parts.next() else {
        return Ok(None);
    };
    let Some(path) = parts.next() else {
        return Ok(None);
    };
    let content_length = header_text.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    });
    let length = content_length.unwrap_or(0);
    if length > MAX_BODY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "parse body exceeds 32MiB",
        ));
    }
    while buffer.len() < header_end + length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > header_end + MAX_BODY_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "parse body exceeds 32MiB",
            ));
        }
    }
    let body_end = (header_end + length).min(buffer.len());
    Ok(Some(Received {
        method: method.to_string(),
        path: path.to_string(),
        headers: header_text,
        body: buffer[header_end..body_end].to_vec(),
    }))
}

fn find_headers_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|offset| offset + 4)
}

async fn write_json(stream: &mut TcpStream, reply: &ParseReply) -> std::io::Result<()> {
    let ParseReply::Json { status, body } = reply;
    let payload = serde_json::to_vec(body).unwrap_or_else(|_| {
        br#"{"success":false,"error":"parse response could not be encoded","code":"UNAVAILABLE"}"#
            .to_vec()
    });
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Entity",
        _ => "Service Unavailable",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&payload).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_public_bind_needs_a_key() {
        let loopback = parse_listen_addr("127.0.0.1:8091").unwrap();
        assert!(public_bind_requires_key(loopback, None).is_ok());
        let public = parse_listen_addr("0.0.0.0:8091").unwrap();
        assert!(public_bind_requires_key(public, None).is_err());
        assert!(public_bind_requires_key(public, Some("deployment-secret")).is_ok());
    }

    #[tokio::test]
    async fn serve_rejects_json_png_and_a_bad_key() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(serve_on(listener, Some("deployment-secret".to_string())));

        let health = http("GET", &address, "/health", None, "text/plain", "").await;
        assert!(health.starts_with("HTTP/1.1 200"), "{health}");

        let denied = http(
            "POST",
            &address,
            "/v1/parse",
            Some("Bearer other"),
            "multipart/form-data; boundary=bound",
            "--bound\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\nhello\r\n--bound--\r\n",
        )
        .await;
        assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");

        let json = http(
            "POST",
            &address,
            "/v1/parse",
            Some("Bearer deployment-secret"),
            "application/json",
            "{\"file\":\"hello\"}",
        )
        .await;
        assert!(json.contains("400"), "{json}");
        assert!(json.contains("INVALID_REQUEST"), "{json}");

        let png = multipart(
            &address,
            "deployment-secret",
            &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
            "image/png",
        )
        .await;
        assert!(png.contains("422"), "{png}");
        assert!(png.contains("NEEDS_OCR"), "{png}");

        let unknown = multipart(
            &address,
            "deployment-secret",
            &[0, 1, 2, 3],
            "application/octet-stream",
        )
        .await;
        assert!(unknown.contains("415"), "{unknown}");
        assert!(unknown.contains("UNSUPPORTED_TYPE"), "{unknown}");

        let text = multipart(&address, "deployment-secret", b"Hello", "text/plain").await;
        assert!(text.contains("200"), "{text}");
        assert!(text.contains("Hello"), "{text}");
        assert!(!text.contains("/tmp"), "{text}");
    }

    async fn multipart(
        address: &std::net::SocketAddr,
        key: &str,
        file: &[u8],
        media: &str,
    ) -> String {
        let mut body = format!(
            "--bound\r\nContent-Disposition: form-data; name=\"media_type\"\r\n\r\n{media}\r\n--bound\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(file);
        body.extend_from_slice(b"\r\n--bound--\r\n");
        http_bytes(
            "POST",
            address,
            "/v1/parse",
            Some(&format!("Bearer {key}")),
            "multipart/form-data; boundary=bound",
            &body,
        )
        .await
    }

    async fn http(
        method: &str,
        address: &std::net::SocketAddr,
        path: &str,
        authorization: Option<&str>,
        content_type: &str,
        body: &str,
    ) -> String {
        http_bytes(
            method,
            address,
            path,
            authorization,
            content_type,
            body.as_bytes(),
        )
        .await
    }

    async fn http_bytes(
        method: &str,
        address: &std::net::SocketAddr,
        path: &str,
        authorization: Option<&str>,
        content_type: &str,
        body: &[u8],
    ) -> String {
        let mut stream = TcpStream::connect(address).await.unwrap();
        let auth = authorization
            .map(|value| format!("Authorization: {value}\r\n"))
            .unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\n{auth}Content-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).into_owned()
    }
}
