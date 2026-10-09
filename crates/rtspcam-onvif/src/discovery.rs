//! WS-Discovery: asks the local network "who is an ONVIF camera?" and collects the answers.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use percent_encoding::percent_decode_str;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::Instant;
use url::Url;
use uuid::Uuid;

use crate::OnvifError;
use crate::xml::Node;

const MULTICAST: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const PORT: u16 = 3702;

/// A camera that answered the discovery probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDevice {
    /// The `name` scope, for example "Front Door" or "Axis P1234".
    pub name: Option<String>,
    /// The `hardware` scope, usually the model.
    pub hardware: Option<String>,
    /// A stable identifier, usually `urn:uuid:...`.
    pub endpoint: String,
    /// Where the camera's ONVIF device service listens.
    pub xaddrs: Vec<Url>,
    /// The address the answer came from.
    pub from: IpAddr,
}

impl DiscoveredDevice {
    /// The device service to talk to: an `http://` address on the host that answered if there
    /// is one, otherwise the first `http://` address.
    pub fn device_url(&self) -> Option<&Url> {
        let http = || self.xaddrs.iter().filter(|u| u.scheme() == "http");
        http()
            .find(|u| u.host_str().and_then(|h| h.parse::<IpAddr>().ok()) == Some(self.from))
            .or_else(|| http().next())
    }

    /// The host part of the device address, for the list in the UI.
    pub fn host(&self) -> String {
        self.device_url()
            .and_then(|u| u.host_str())
            .map_or_else(|| self.from.to_string(), str::to_owned)
    }

    /// A name for the list: the camera's own name or model, else its address.
    pub fn title(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.hardware.clone())
            .unwrap_or_else(|| self.host())
    }
}

/// The WS-Discovery `Probe` for ONVIF video transmitters.
pub fn probe_message(message_id: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><e:Envelope xmlns:e=\"http://www.w3.org/2003/05/soap-envelope\" xmlns:w=\"http://schemas.xmlsoap.org/ws/2004/08/addressing\" xmlns:d=\"http://schemas.xmlsoap.org/ws/2005/04/discovery\" xmlns:dn=\"http://www.onvif.org/ver10/network/wsdl\"><e:Header><w:MessageID>uuid:{message_id}</w:MessageID><w:To e:mustUnderstand=\"true\">urn:schemas-xmlsoap-org:ws:2005:04:discovery</w:To><w:Action e:mustUnderstand=\"true\">http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</w:Action></e:Header><e:Body><d:Probe><d:Types>dn:NetworkVideoTransmitter</d:Types></d:Probe></e:Body></e:Envelope>"
    )
}

/// The devices described by a `ProbeMatches` reply that arrived from `from`.
pub fn parse_probe_match(xml: &str, from: IpAddr) -> Vec<DiscoveredDevice> {
    let Ok(root) = Node::parse(xml) else {
        return Vec::new();
    };
    let mut matches = Vec::new();
    root.find_all("ProbeMatch", &mut matches);
    matches
        .into_iter()
        .filter_map(|m| {
            let xaddrs: Vec<Url> = m
                .text_of("XAddrs")?
                .split_whitespace()
                .filter_map(|u| Url::parse(u).ok())
                .collect();
            if xaddrs.is_empty() {
                return None;
            }
            let scopes = m.text_of("Scopes").unwrap_or_default();
            // Other WS-Discovery devices (Windows PCs, printers) answer every probe too.
            let is_onvif = m
                .text_of("Types")
                .is_some_and(|t| t.contains("NetworkVideoTransmitter"))
                || scopes.contains("onvif.org/");
            if !is_onvif {
                return None;
            }
            Some(DiscoveredDevice {
                name: scope_value(scopes, "name"),
                hardware: scope_value(scopes, "hardware"),
                endpoint: m
                    .path(&["EndpointReference", "Address"])
                    .map(|a| a.text().to_owned())
                    .unwrap_or_default(),
                xaddrs,
                from,
            })
        })
        .collect()
}

/// The value of `onvif://www.onvif.org/<key>/<value>` in a space-separated scope list.
fn scope_value(scopes: &str, key: &str) -> Option<String> {
    let prefix = format!("onvif://www.onvif.org/{key}/");
    scopes.split_whitespace().find_map(|scope| {
        let value = scope.strip_prefix(&prefix)?;
        let value = percent_decode_str(value).decode_utf8_lossy();
        // Cameras separate words with "_" or "%20".
        let value = value.replace('_', " ");
        (!value.trim().is_empty()).then(|| value.trim().to_owned())
    })
}

/// Probes every network interface and returns the cameras that answered within `wait`.
pub async fn discover(wait: Duration) -> Result<Vec<DiscoveredDevice>, OnvifError> {
    let mut locals: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
        .map_err(|e| OnvifError::Network(e.to_string()))?
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            IpAddr::V4(ip) if !ip.is_link_local() => Some(ip),
            _ => None,
        })
        .collect();
    locals.sort();
    locals.dedup();
    if locals.is_empty() {
        return Err(OnvifError::Network(
            "this PC is not connected to a network".into(),
        ));
    }

    let deadline = Instant::now() + wait;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut tasks = Vec::new();
    for local in locals {
        match socket_on(local) {
            Ok(socket) => tasks.push(tokio::spawn(probe_on(socket, deadline, tx.clone()))),
            Err(e) => tracing::debug!(%local, error = %e, "cannot probe on this interface"),
        }
    }
    drop(tx);
    if tasks.is_empty() {
        return Err(OnvifError::Network(
            "could not open a network socket for discovery".into(),
        ));
    }

    let mut found: Vec<DiscoveredDevice> = Vec::new();
    while let Some(device) = rx.recv().await {
        let known = found.iter().any(|d| {
            (!device.endpoint.is_empty() && d.endpoint == device.endpoint)
                || d.xaddrs == device.xaddrs
        });
        if !known {
            found.push(device);
        }
    }
    for t in tasks {
        let _ = t.await;
    }
    found.sort_by_key(|d| (d.host(), d.endpoint.clone()));
    Ok(found)
}

fn socket_on(local: Ipv4Addr) -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.bind(&SocketAddrV4::new(local, 0).into())?;
    socket.set_multicast_if_v4(&local)?;
    socket.set_multicast_ttl_v4(4)?;
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into())
}

/// Sends the probe a few times (UDP is lossy and cameras wake slowly) and forwards answers
/// until `deadline`.
async fn probe_on(
    socket: UdpSocket,
    deadline: Instant,
    out: mpsc::UnboundedSender<DiscoveredDevice>,
) {
    let target = SocketAddr::from((MULTICAST, PORT));
    let mut buf = vec![0u8; 16 * 1024];
    let mut sends = [
        Duration::ZERO,
        Duration::from_millis(500),
        Duration::from_millis(1500),
    ]
    .into_iter()
    .map(|d| Instant::now() + d)
    .collect::<Vec<_>>()
    .into_iter();
    let mut next_send = sends.next();
    loop {
        let wake = next_send.map_or(deadline, |t| t.min(deadline));
        tokio::select! {
            () = tokio::time::sleep_until(wake) => {
                if Instant::now() >= deadline {
                    return;
                }
                let probe = probe_message(&Uuid::new_v4().to_string());
                if let Err(e) = socket.send_to(probe.as_bytes(), target).await {
                    tracing::debug!(error = %e, "sending the discovery probe failed");
                }
                next_send = sends.next();
            }
            received = socket.recv_from(&mut buf) => {
                let Ok((n, from)) = received else { continue };
                let text = String::from_utf8_lossy(&buf[..n]);
                for device in parse_probe_match(&text, from.ip()) {
                    let _ = out.send(device);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPLY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<SOAP-ENV:Envelope xmlns:SOAP-ENV="http://www.w3.org/2003/05/soap-envelope" xmlns:wsa="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery">
 <SOAP-ENV:Header><wsa:MessageID>uuid:1</wsa:MessageID></SOAP-ENV:Header>
 <SOAP-ENV:Body><d:ProbeMatches><d:ProbeMatch>
  <wsa:EndpointReference><wsa:Address>urn:uuid:2419d68a-2dd2-21b2-a205-001122334455</wsa:Address></wsa:EndpointReference>
  <d:Types>dn:NetworkVideoTransmitter tds:Device</d:Types>
  <d:Scopes>onvif://www.onvif.org/type/video_encoder onvif://www.onvif.org/name/Front_Door onvif://www.onvif.org/hardware/IPC-HFW%201230 onvif://www.onvif.org/location/country/it</d:Scopes>
  <d:XAddrs>http://10.9.9.9:80/onvif/device_service http://192.168.1.64:80/onvif/device_service</d:XAddrs>
  <d:MetadataVersion>1</d:MetadataVersion>
 </d:ProbeMatch></d:ProbeMatches></SOAP-ENV:Body></SOAP-ENV:Envelope>"#;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn parses_a_probe_match() {
        let found = parse_probe_match(REPLY, ip("192.168.1.64"));
        assert_eq!(found.len(), 1);
        let d = &found[0];
        assert_eq!(d.name.as_deref(), Some("Front Door"));
        assert_eq!(d.hardware.as_deref(), Some("IPC-HFW 1230"));
        assert_eq!(d.endpoint, "urn:uuid:2419d68a-2dd2-21b2-a205-001122334455");
        assert_eq!(d.xaddrs.len(), 2);
        assert_eq!(d.title(), "Front Door");
    }

    #[test]
    fn prefers_the_address_that_answered() {
        let d = &parse_probe_match(REPLY, ip("192.168.1.64"))[0];
        assert_eq!(
            d.device_url().unwrap().as_str(),
            "http://192.168.1.64/onvif/device_service"
        );
        assert_eq!(d.host(), "192.168.1.64");
        // Answered from an address that is not listed: first one wins.
        let d = &parse_probe_match(REPLY, ip("172.16.0.1"))[0];
        assert_eq!(d.host(), "10.9.9.9");
    }

    #[test]
    fn title_falls_back_to_hardware_then_host() {
        let bare = REPLY.replace("onvif://www.onvif.org/name/Front_Door", "");
        let d = &parse_probe_match(&bare, ip("192.168.1.64"))[0];
        assert_eq!(d.title(), "IPC-HFW 1230");
        let bare = bare.replace("onvif://www.onvif.org/hardware/IPC-HFW%201230", "");
        let d = &parse_probe_match(&bare, ip("192.168.1.64"))[0];
        assert_eq!(d.title(), "192.168.1.64");
    }

    #[test]
    fn ignores_junk_and_matches_without_addresses() {
        assert!(parse_probe_match("not xml", ip("10.0.0.1")).is_empty());
        assert!(parse_probe_match("<a/>", ip("10.0.0.1")).is_empty());
        let none = REPLY.replace(
            "http://10.9.9.9:80/onvif/device_service http://192.168.1.64:80/onvif/device_service",
            "",
        );
        assert!(parse_probe_match(&none, ip("10.0.0.1")).is_empty());
    }

    #[test]
    fn other_ws_discovery_devices_are_not_cameras() {
        // A Windows PC answering the same probe with its own types and no ONVIF scopes.
        let pc = REPLY
            .replace(
                "dn:NetworkVideoTransmitter tds:Device",
                "wsdp:Device pub:Computer",
            )
            .replace("onvif://www.onvif.org/", "other://scope/");
        assert!(parse_probe_match(&pc, ip("192.168.1.2")).is_empty());
    }

    #[test]
    fn the_probe_is_well_formed() {
        let probe = probe_message("abc");
        let root = Node::parse(&probe).unwrap();
        assert_eq!(root.text_of("MessageID"), Some("uuid:abc"));
        assert_eq!(root.text_of("Types"), Some("dn:NetworkVideoTransmitter"));
    }
}
