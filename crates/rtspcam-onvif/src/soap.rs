//! SOAP over plain HTTP, and the WS-Security UsernameToken header.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rand::RngCore as _;
use sha1::{Digest as _, Sha1};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use url::Url;

use crate::OnvifError;
use crate::time::{iso8601, unix_now};
use crate::xml::Node;

/// Replies larger than this are not an ONVIF device.
const MAX_REPLY: usize = 4 * 1024 * 1024;

const NS_DEVICE: &str = "http://www.onvif.org/ver10/device/wsdl";
const NS_MEDIA: &str = "http://www.onvif.org/ver10/media/wsdl";
const NS_SCHEMA: &str = "http://www.onvif.org/ver10/schema";

/// Escapes text for an XML element.
pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The PasswordDigest of WS-Security: `base64(sha1(nonce + created + password))`.
pub(crate) fn password_digest(nonce: &[u8], created: &str, password: &str) -> String {
    let mut sha = Sha1::new();
    sha.update(nonce);
    sha.update(created.as_bytes());
    sha.update(password.as_bytes());
    STANDARD.encode(sha.finalize())
}

fn security_header(user: &str, password: &str, clock_offset: i64) -> String {
    let mut nonce = [0u8; 16];
    rand::rng().fill_bytes(&mut nonce);
    let created = iso8601(unix_now() + clock_offset);
    format!(
        "<s:Header><Security xmlns=\"http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd\" s:mustUnderstand=\"1\"><UsernameToken><Username>{}</Username><Password Type=\"http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest\">{}</Password><Nonce EncodingType=\"http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary\">{}</Nonce><Created xmlns=\"http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd\">{created}</Created></UsernameToken></Security></s:Header>",
        escape(user),
        password_digest(&nonce, &created, password),
        STANDARD.encode(nonce),
    )
}

/// A SOAP 1.2 envelope around `body`, signed when `auth` is `(user, password, clock offset)`.
pub(crate) fn envelope(body: &str, auth: Option<(&str, &str, i64)>) -> String {
    let header = auth
        .map(|(u, p, off)| security_header(u, p, off))
        .unwrap_or_default();
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\" xmlns:tds=\"{NS_DEVICE}\" xmlns:trt=\"{NS_MEDIA}\" xmlns:tt=\"{NS_SCHEMA}\">{header}<s:Body>{body}</s:Body></s:Envelope>"
    )
}

/// Posts `envelope` to `url` and returns the reply's body.
pub(crate) async fn post(
    url: &Url,
    envelope: &str,
    timeout: Duration,
) -> Result<String, OnvifError> {
    if url.scheme() != "http" {
        return Err(OnvifError::UnsupportedScheme(url.scheme().to_owned()));
    }
    tokio::time::timeout(timeout, exchange(url, envelope))
        .await
        .map_err(|_| OnvifError::Timeout)?
}

async fn exchange(url: &Url, envelope: &str) -> Result<String, OnvifError> {
    let host = url
        .host_str()
        .ok_or_else(|| OnvifError::Network("the address has no host".into()))?;
    let port = url.port().unwrap_or(80);
    let mut stream = TcpStream::connect((host.trim_matches(['[', ']']), port))
        .await
        .map_err(|e| OnvifError::Network(e.to_string()))?;

    let mut target = url.path().to_owned();
    if let Some(q) = url.query() {
        target.push('?');
        target.push_str(q);
    }
    let request = format!(
        "POST {target} HTTP/1.1\r\nHost: {host}:{port}\r\nUser-Agent: rtspcam\r\nContent-Type: application/soap+xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{envelope}",
        envelope.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| OnvifError::Network(e.to_string()))?;

    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|e| OnvifError::Network(e.to_string()))?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..n]);
        if raw.len() > MAX_REPLY {
            return Err(OnvifError::Protocol("the reply is too large".into()));
        }
    }
    interpret(parse_http(&raw)?)
}

struct HttpReply {
    status: u16,
    body: String,
}

fn parse_http(raw: &[u8]) -> Result<HttpReply, OnvifError> {
    let protocol = |why: &str| OnvifError::Protocol(format!("not an HTTP reply ({why})"));
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| protocol("no header end"))?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| protocol("bad status line"))?;
    let mut chunked = false;
    let mut length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "transfer-encoding" => chunked = value.to_ascii_lowercase().contains("chunked"),
            "content-length" => length = value.parse::<usize>().ok(),
            _ => {}
        }
    }
    let body = &raw[split + 4..];
    let body = if chunked {
        dechunk(body).ok_or_else(|| protocol("bad chunked body"))?
    } else if let Some(n) = length {
        body[..n.min(body.len())].to_vec()
    } else {
        body.to_vec()
    };
    Ok(HttpReply {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn dechunk(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = body.windows(2).position(|w| w == b"\r\n")?;
        let size = std::str::from_utf8(&body[..eol]).ok()?;
        let size = usize::from_str_radix(size.split(';').next()?.trim(), 16).ok()?;
        body = &body[eol + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(body.get(..size)?);
        body = body.get(size + 2..)?;
    }
}

/// Turns HTTP errors and SOAP faults into [`OnvifError`]s.
fn interpret(reply: HttpReply) -> Result<String, OnvifError> {
    if reply.status == 401 || reply.status == 403 {
        return Err(OnvifError::Unauthorized);
    }
    if let Ok(root) = Node::parse(&reply.body)
        && let Some(fault) = root.find("Fault")
    {
        let code = fault
            .find("Subcode")
            .and_then(|s| s.text_of("Value"))
            .or_else(|| fault.path(&["Code", "Value"]).map(Node::text))
            .unwrap_or_default();
        let reason = fault.text_of("Text").unwrap_or("unknown error");
        if code.contains("NotAuthorized") {
            return Err(OnvifError::Unauthorized);
        }
        return Err(OnvifError::Fault(if code.is_empty() {
            reason.to_owned()
        } else {
            format!("{reason} ({code})")
        }));
    }
    if !(200..300).contains(&reply.status) {
        return Err(OnvifError::Protocol(format!(
            "the camera answered HTTP {}",
            reply.status
        )));
    }
    Ok(reply.body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_matches_the_ws_security_example() {
        // Example from the OASIS UsernameToken profile 1.0 specification.
        let nonce = STANDARD.decode("LKqI6G/AikKCQrN0zqZFlg==").unwrap();
        assert_eq!(
            password_digest(&nonce, "2010-09-16T07:50:45Z", "userpassword"),
            "tuOSpGlFlIXsozq4HFNeeGeFLEI="
        );
    }

    #[test]
    fn envelope_escapes_credentials() {
        let signed = envelope("<x/>", Some(("a<b&c", "pw", 0)));
        assert!(signed.contains("<Username>a&lt;b&amp;c</Username>"));
        assert!(!signed.contains(">pw<"), "the password is only sent hashed");
        assert!(!envelope("<x/>", None).contains("Security"));
    }

    #[test]
    fn parses_content_length_and_chunked_replies() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhelloEXTRA";
        assert_eq!(parse_http(plain).unwrap().body, "hello");
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n";
        assert_eq!(parse_http(chunked).unwrap().body, "abcde");
        assert!(parse_http(b"garbage").is_err());
    }

    fn reply(status: u16, body: &str) -> HttpReply {
        HttpReply {
            status,
            body: body.to_owned(),
        }
    }

    #[test]
    fn http_401_and_faults_are_classified() {
        assert_eq!(
            interpret(reply(401, "")).unwrap_err(),
            OnvifError::Unauthorized
        );
        let fault = r#"<s:Envelope xmlns:s="x"><s:Body><s:Fault><s:Code><s:Value>s:Sender</s:Value>
            <s:Subcode><s:Value>ter:NotAuthorized</s:Value></s:Subcode></s:Code>
            <s:Reason><s:Text>Sender not Authorized</s:Text></s:Reason></s:Fault></s:Body></s:Envelope>"#;
        assert_eq!(
            interpret(reply(400, fault)).unwrap_err(),
            OnvifError::Unauthorized
        );
        let other = fault
            .replace("ter:NotAuthorized", "ter:ActionNotSupported")
            .replace("Sender not Authorized", "Optional Action Not Implemented");
        match interpret(reply(500, &other)).unwrap_err() {
            OnvifError::Fault(m) => {
                assert!(m.contains("Not Implemented") && m.contains("ActionNotSupported"));
            }
            e => panic!("{e:?}"),
        }
        assert!(matches!(
            interpret(reply(404, "nope")),
            Err(OnvifError::Protocol(_))
        ));
        assert!(interpret(reply(200, "<a/>")).is_ok());
    }
}
