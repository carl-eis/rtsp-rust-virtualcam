//! Asking a camera for its streams: `GetDeviceInformation`, `GetProfiles`, `GetStreamUri`.

use std::net::IpAddr;
use std::time::Duration;

use url::{Host, Url};

use crate::OnvifError;
use crate::soap::{envelope, escape, post};
use crate::time::{unix_from_fields, unix_now};
use crate::xml::Node;

/// Per request. Cameras that take longer than this are not going to work well anyway.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// One media profile of a camera: usually "main stream", "sub stream" and so on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub token: String,
    pub name: String,
    /// `H264`, `H265`, `JPEG`, ...
    pub encoding: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The `rtsp://` address, without credentials.
    pub rtsp_uri: String,
}

impl Profile {
    /// For example `Main (H264 2304x1296)`.
    pub fn label(&self) -> String {
        let mut details = Vec::new();
        if let Some(e) = &self.encoding {
            details.push(e.clone());
        }
        if let (Some(w), Some(h)) = (self.width, self.height) {
            details.push(format!("{w}x{h}"));
        }
        if details.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, details.join(" "))
        }
    }
}

/// What a camera told us about itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CameraInfo {
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub profiles: Vec<Profile>,
}

impl CameraInfo {
    /// "Manufacturer Model", for suggesting a stream name.
    pub fn title(&self) -> Option<String> {
        let parts: Vec<&str> = [self.manufacturer.as_deref(), self.model.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}

struct Session<'a> {
    credentials: Option<&'a Credentials>,
    /// Seconds to add to this PC's clock to get the camera's, so signatures are accepted by
    /// cameras whose clock is off.
    clock_offset: i64,
}

impl Session<'_> {
    async fn call(&self, url: &Url, body: &str) -> Result<Node, OnvifError> {
        let auth = self
            .credentials
            .map(|c| (c.username.as_str(), c.password.as_str(), self.clock_offset));
        let reply = post(url, &envelope(body, auth), REQUEST_TIMEOUT).await?;
        Node::parse(&reply)
    }
}

/// Asks the camera whose device service is at `device_url` for its streams.
///
/// Fails with [`OnvifError::Unauthorized`] if the camera wants a login and the credentials are
/// missing or wrong. Profiles whose address the camera won't give are left out.
pub async fn query_camera(
    device_url: &Url,
    credentials: Option<&Credentials>,
) -> Result<CameraInfo, OnvifError> {
    // The first request also tells us early if the camera can't be reached at all.
    let clock_offset = clock_offset(device_url).await?.unwrap_or(0);
    let session = Session {
        credentials,
        clock_offset,
    };

    let mut info = CameraInfo::default();
    match session
        .call(device_url, "<tds:GetDeviceInformation/>")
        .await
    {
        Ok(reply) => {
            info.manufacturer = reply.text_of("Manufacturer").map(str::to_owned);
            info.model = reply.text_of("Model").map(str::to_owned);
        }
        Err(OnvifError::Unauthorized) => return Err(OnvifError::Unauthorized),
        Err(e) => tracing::debug!(error = %e, "GetDeviceInformation failed"),
    }

    let media_url = media_service(&session, device_url).await?;
    let reply = session.call(&media_url, "<trt:GetProfiles/>").await?;
    let mut nodes = Vec::new();
    reply.find_all("Profiles", &mut nodes);

    let mut last_error = None;
    for node in nodes {
        let Some(token) = node.attr("token") else {
            continue;
        };
        // Audio-only or metadata-only profiles have no video encoder.
        let Some(video) = node.child("VideoEncoderConfiguration") else {
            continue;
        };
        let body = format!(
            "<trt:GetStreamUri><trt:StreamSetup><tt:Stream>RTP-Unicast</tt:Stream><tt:Transport><tt:Protocol>RTSP</tt:Protocol></tt:Transport></trt:StreamSetup><trt:ProfileToken>{}</trt:ProfileToken></trt:GetStreamUri>",
            escape(token)
        );
        let uri = match session.call(&media_url, &body).await {
            Ok(reply) => reply.text_of("Uri").map(str::to_owned),
            Err(e) => {
                tracing::debug!(token, error = %e, "GetStreamUri failed");
                last_error = Some(e);
                None
            }
        };
        let Some(uri) = uri else { continue };
        info.profiles.push(Profile {
            token: token.to_owned(),
            name: node
                .child("Name")
                .map(|n| n.text().to_owned())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| token.to_owned()),
            encoding: video.text_of("Encoding").map(str::to_owned),
            width: video.text_of("Width").and_then(|v| v.parse().ok()),
            height: video.text_of("Height").and_then(|v| v.parse().ok()),
            rtsp_uri: fix_host(&uri, device_url),
        });
    }

    if info.profiles.is_empty() {
        return Err(last_error.unwrap_or_else(|| {
            OnvifError::Protocol("the camera lists no video streams".to_owned())
        }));
    }
    Ok(info)
}

/// The media service address: from `GetCapabilities`, else `GetServices`, else the device
/// service itself (some small cameras serve everything from one address).
async fn media_service(session: &Session<'_>, device_url: &Url) -> Result<Url, OnvifError> {
    let from_capabilities = session
        .call(
            device_url,
            "<tds:GetCapabilities><tds:Category>Media</tds:Category></tds:GetCapabilities>",
        )
        .await;
    let xaddr = match from_capabilities {
        Ok(reply) => reply
            .path(&["Body", "GetCapabilitiesResponse", "Capabilities", "Media"])
            .and_then(|m| m.text_of("XAddr"))
            .map(str::to_owned),
        Err(OnvifError::Unauthorized) => return Err(OnvifError::Unauthorized),
        Err(e) => {
            tracing::debug!(error = %e, "GetCapabilities failed");
            None
        }
    };
    let xaddr = match xaddr {
        Some(x) => Some(x),
        None => {
            let body = "<tds:GetServices><tds:IncludeCapability>false</tds:IncludeCapability></tds:GetServices>";
            match session.call(device_url, body).await {
                Ok(reply) => {
                    let mut services = Vec::new();
                    reply.find_all("Service", &mut services);
                    services
                        .into_iter()
                        .find(|s| {
                            s.text_of("Namespace") == Some("http://www.onvif.org/ver10/media/wsdl")
                        })
                        .and_then(|s| s.text_of("XAddr"))
                        .map(str::to_owned)
                }
                Err(OnvifError::Unauthorized) => return Err(OnvifError::Unauthorized),
                Err(_) => None,
            }
        }
    };
    Ok(xaddr
        .map(|x| fix_host(&x, device_url))
        .and_then(|x| Url::parse(&x).ok())
        .unwrap_or_else(|| device_url.clone()))
}

/// Cameras sometimes put `0.0.0.0`, `localhost` or an address from another network in the
/// addresses they return. Use the host we reached the camera on instead of those.
fn fix_host(address: &str, device_url: &Url) -> String {
    let Ok(mut url) = Url::parse(address) else {
        return address.to_owned();
    };
    let useless = |ip: IpAddr| ip.is_unspecified() || ip.is_loopback();
    let unusable = match url.host() {
        None => true,
        Some(Host::Ipv4(ip)) => useless(ip.into()),
        Some(Host::Ipv6(ip)) => useless(ip.into()),
        // Non-special schemes such as rtsp:// keep even IP addresses as plain text.
        Some(Host::Domain(d)) => {
            d.eq_ignore_ascii_case("localhost")
                || d.trim_matches(['[', ']'])
                    .parse::<IpAddr>()
                    .is_ok_and(useless)
        }
    };
    if unusable && let Some(host) = device_url.host_str() {
        let host = host.trim_matches(['[', ']']);
        if url.set_host(Some(host)).is_err() {
            return address.to_owned();
        }
    }
    url.to_string()
}

/// How far the camera's clock is from ours, from `GetSystemDateAndTime` (which needs no
/// login). `Ok(None)` if the camera answers but doesn't say; an error only if it can't be
/// reached.
async fn clock_offset(device_url: &Url) -> Result<Option<i64>, OnvifError> {
    let request = envelope("<tds:GetSystemDateAndTime/>", None);
    let reply = match post(device_url, &request, REQUEST_TIMEOUT).await {
        Ok(reply) => reply,
        Err(
            e @ (OnvifError::Network(_) | OnvifError::Timeout | OnvifError::UnsupportedScheme(_)),
        ) => return Err(e),
        Err(_) => return Ok(None),
    };
    Ok(parse_clock_offset(&reply))
}

fn parse_clock_offset(reply: &str) -> Option<i64> {
    let root = Node::parse(reply).ok()?;
    let utc = root.find("UTCDateTime")?;
    let field = |parent: &str, name: &str| -> Option<i64> {
        utc.child(parent)?.text_of(name)?.parse().ok()
    };
    let camera = unix_from_fields(
        field("Date", "Year")?,
        field("Date", "Month")?,
        field("Date", "Day")?,
        field("Time", "Hour")?,
        field("Time", "Minute")?,
        field("Time", "Second")?,
    );
    let offset = camera - unix_now();
    // Under a few seconds is just latency; leave the signature on our own clock.
    Some(if offset.abs() < 5 { 0 } else { offset })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    use super::*;

    /// A canned ONVIF camera: answers by the first element in the request body.
    struct FakeCamera {
        url: Url,
        requests: Arc<Mutex<Vec<String>>>,
    }

    async fn fake_camera(require_login: bool, clock_skew: i64) -> FakeCamera {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = requests.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let log = log.clone();
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let body = loop {
                        let n = stream.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        raw.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&raw).into_owned();
                        if let Some((head, body)) = text.split_once("\r\n\r\n") {
                            let want = head
                                .to_lowercase()
                                .split("content-length:")
                                .nth(1)
                                .and_then(|r| r.split_whitespace().next()?.parse::<usize>().ok())
                                .unwrap_or(0);
                            if body.len() >= want {
                                break body.to_owned();
                            }
                        }
                    };
                    log.lock().unwrap().push(body.clone());
                    let reply = respond(&body, port, require_login, clock_skew);
                    let (status, xml) = reply;
                    let out = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/soap+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{xml}",
                        xml.len()
                    );
                    let _ = stream.write_all(out.as_bytes()).await;
                });
            }
        });
        FakeCamera {
            url: Url::parse(&format!("http://127.0.0.1:{port}/onvif/device_service")).unwrap(),
            requests,
        }
    }

    fn wrap(inner: &str) -> String {
        format!(
            r#"<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:tds="d" xmlns:trt="m" xmlns:tt="t"><s:Body>{inner}</s:Body></s:Envelope>"#
        )
    }

    fn respond(
        body: &str,
        port: u16,
        require_login: bool,
        clock_skew: i64,
    ) -> (&'static str, String) {
        let ok = "200 OK";
        if body.contains("GetSystemDateAndTime") {
            let t = unix_now() + clock_skew;
            let (y, mo, d) = crate::time::civil_from_days(t.div_euclid(86_400));
            let s = t.rem_euclid(86_400);
            return (
                ok,
                wrap(&format!(
                    "<tds:GetSystemDateAndTimeResponse><tds:SystemDateAndTime><tt:UTCDateTime><tt:Time><tt:Hour>{}</tt:Hour><tt:Minute>{}</tt:Minute><tt:Second>{}</tt:Second></tt:Time><tt:Date><tt:Year>{y}</tt:Year><tt:Month>{mo}</tt:Month><tt:Day>{d}</tt:Day></tt:Date></tt:UTCDateTime></tds:SystemDateAndTime></tds:GetSystemDateAndTimeResponse>",
                    s / 3600,
                    s % 3600 / 60,
                    s % 60
                )),
            );
        }
        if require_login && !body.contains("UsernameToken") {
            return (
                "400 Bad Request",
                wrap(
                    "<s:Fault><s:Code><s:Value>s:Sender</s:Value><s:Subcode><s:Value>ter:NotAuthorized</s:Value></s:Subcode></s:Code><s:Reason><s:Text>Sender not Authorized</s:Text></s:Reason></s:Fault>",
                ),
            );
        }
        let body_text = if body.contains("GetDeviceInformation") {
            "<tds:GetDeviceInformationResponse><tds:Manufacturer>Acme</tds:Manufacturer><tds:Model>Cam 3000</tds:Model></tds:GetDeviceInformationResponse>".to_owned()
        } else if body.contains("GetCapabilities") {
            // The camera reports an address it can't be reached on.
            format!(
                "<tds:GetCapabilitiesResponse><tds:Capabilities><tt:Media><tt:XAddr>http://0.0.0.0:{port}/onvif/media_service</tt:XAddr></tt:Media></tds:Capabilities></tds:GetCapabilitiesResponse>"
            )
        } else if body.contains("GetProfiles") {
            "<trt:GetProfilesResponse>\
             <trt:Profiles token=\"p1\" fixed=\"true\"><tt:Name>mainStream</tt:Name><tt:VideoEncoderConfiguration token=\"v1\"><tt:Name>v</tt:Name><tt:Encoding>H264</tt:Encoding><tt:Resolution><tt:Width>2304</tt:Width><tt:Height>1296</tt:Height></tt:Resolution></tt:VideoEncoderConfiguration></trt:Profiles>\
             <trt:Profiles token=\"p2\"><tt:Name>subStream</tt:Name><tt:VideoEncoderConfiguration token=\"v2\"><tt:Encoding>JPEG</tt:Encoding><tt:Resolution><tt:Width>640</tt:Width><tt:Height>360</tt:Height></tt:Resolution></tt:VideoEncoderConfiguration></trt:Profiles>\
             <trt:Profiles token=\"audio\"><tt:Name>audioOnly</tt:Name></trt:Profiles>\
             </trt:GetProfilesResponse>".to_owned()
        } else if body.contains("GetStreamUri") {
            let uri = if body.contains(">p1<") {
                "rtsp://0.0.0.0:554/main?a=1&amp;b=2"
            } else {
                "rtsp://192.168.1.9:554/sub"
            };
            format!(
                "<trt:GetStreamUriResponse><trt:MediaUri><tt:Uri>{uri}</tt:Uri></trt:MediaUri></trt:GetStreamUriResponse>"
            )
        } else {
            return (
                "400 Bad Request",
                wrap("<s:Fault><s:Reason><s:Text>unknown</s:Text></s:Reason></s:Fault>"),
            );
        };
        (ok, wrap(&body_text))
    }

    fn creds() -> Credentials {
        Credentials {
            username: "admin".into(),
            password: "pw".into(),
        }
    }

    #[tokio::test]
    async fn lists_profiles_with_streams() {
        let camera = fake_camera(false, 0).await;
        let info = query_camera(&camera.url, None).await.unwrap();
        assert_eq!(info.title().as_deref(), Some("Acme Cam 3000"));
        assert_eq!(info.profiles.len(), 2, "the audio-only profile is skipped");
        let main = &info.profiles[0];
        assert_eq!(main.token, "p1");
        assert_eq!(main.encoding.as_deref(), Some("H264"));
        assert_eq!((main.width, main.height), (Some(2304), Some(1296)));
        assert_eq!(main.label(), "mainStream (H264 2304x1296)");
        // 0.0.0.0 in the reply is replaced by the host we reached the camera on.
        assert_eq!(main.rtsp_uri, "rtsp://127.0.0.1:554/main?a=1&b=2");
        // An address that is usable is left alone.
        assert_eq!(info.profiles[1].rtsp_uri, "rtsp://192.168.1.9:554/sub");
    }

    #[tokio::test]
    async fn a_login_is_required_and_used() {
        let camera = fake_camera(true, 0).await;
        assert_eq!(
            query_camera(&camera.url, None).await.unwrap_err(),
            OnvifError::Unauthorized
        );
        let info = query_camera(&camera.url, Some(&creds())).await.unwrap();
        assert_eq!(info.profiles.len(), 2);
        let sent = camera.requests.lock().unwrap().join("\n");
        assert!(sent.contains("<Username>admin</Username>"));
        assert!(!sent.contains(">pw<"));
    }

    #[tokio::test]
    async fn a_wrong_clock_on_the_camera_is_compensated() {
        let camera = fake_camera(true, 3 * 3600).await;
        query_camera(&camera.url, Some(&creds())).await.unwrap();
        let sent = camera.requests.lock().unwrap().join("\n");
        let created = sent
            .split("<Created")
            .nth(1)
            .and_then(|s| s.split('>').nth(1))
            .and_then(|s| s.split("</Created").next())
            .unwrap();
        // The signature is stamped with the camera's time, three hours ahead of ours.
        let want = crate::time::iso8601(unix_now() + 3 * 3600);
        assert_eq!(&created[..13], &want[..13], "{created} vs {want}");
    }

    #[tokio::test]
    async fn unreachable_and_unsupported_addresses() {
        let closed = Url::parse("http://127.0.0.1:1/onvif/device_service").unwrap();
        assert!(matches!(
            query_camera(&closed, None).await,
            Err(OnvifError::Network(_)) | Err(OnvifError::Timeout)
        ));
        let tls = Url::parse("https://127.0.0.1/onvif").unwrap();
        assert!(matches!(
            query_camera(&tls, None).await,
            Err(OnvifError::UnsupportedScheme(_))
        ));
    }

    #[test]
    fn credentials_are_redacted_in_debug() {
        assert!(!format!("{:?}", creds()).contains("pw"));
    }

    #[test]
    fn host_fixing() {
        let dev = Url::parse("http://192.168.1.5/onvif/device_service").unwrap();
        assert_eq!(fix_host("rtsp://0.0.0.0/a", &dev), "rtsp://192.168.1.5/a");
        assert_eq!(
            fix_host("rtsp://localhost:8554/a", &dev),
            "rtsp://192.168.1.5:8554/a"
        );
        assert_eq!(fix_host("rtsp://cam.lan/a", &dev), "rtsp://cam.lan/a");
    }
}
