//! The Add/Edit stream form as plain data: what the user typed, checked against the same rules
//! as `config.json`, without any window code so it can be tested.

use rtspcam_core::config::{BrandTemplate, Field, Problem, StreamConfig, parse_stream_url};
use rtspcam_core::{Config, FitMode, OutputFormat, Picture, Protocol, Secret, Transport};

/// Resolutions offered in the form, as combo box entries (same order as
/// [`OutputFormat::RESOLUTIONS`]).
pub const RESOLUTION_LABELS: [&str; 3] = ["1920 x 1080", "1280 x 720", "640 x 480"];
/// Frame rates offered in the form (same order as [`OutputFormat::FRAME_RATES`]).
pub const FPS_LABELS: [&str; 2] = ["30 fps", "15 fps"];
pub const TRANSPORT_LABELS: [&str; 2] = ["TCP", "UDP"];
pub const FIT_LABELS: [&str; 3] = ["Letterbox", "Crop", "Stretch"];
/// Same order as [`rtspcam_core::config::Rotation::ALL`].
pub const ROTATION_LABELS: [&str; 4] = [
    "None",
    "90 degrees clockwise",
    "180 degrees",
    "270 degrees clockwise",
];
pub const DISCONNECT_LABELS: [&str; 2] = ["Show \"No signal\"", "Keep showing the last picture"];

/// A crop edge typed by the user: a whole number from 0 to the allowed maximum.
pub fn parse_percent(text: &str) -> Option<u8> {
    let value: u8 = text.trim().parse().ok()?;
    (value <= rtspcam_core::config::MAX_CROP_PERCENT).then_some(value)
}

/// What the form controls hold. Choice fields are indexes into the label lists above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamForm {
    pub name: String,
    pub host: String,
    pub port: String,
    pub path: String,
    pub username: String,
    pub password: String,
    pub transport: usize,
    pub resolution: usize,
    pub fps: usize,
    pub fit: usize,
    /// Edited in its own dialog; the form only carries it.
    pub picture: Picture,
    pub on_demand: bool,
    pub enabled: bool,
}

/// A new stream for an address a camera reported over ONVIF. The name is made unique among
/// `existing`; credentials typed by the user win over any inside the address. The error is ready
/// to show the user.
pub fn stream_from_onvif(
    name: &str,
    rtsp_uri: &str,
    username: &str,
    password: &str,
    existing: &[StreamConfig],
) -> Result<StreamConfig, String> {
    let url = parse_stream_url(rtsp_uri.trim()).map_err(|e| e.to_string())?;
    let mut s = StreamConfig::new(
        rtspcam_core::config::unique_stream_name(existing, name.trim()),
        url.host,
    );
    s.port = url.port;
    s.path = url.path;
    let username = username.trim();
    if username.is_empty() {
        s.username = url.username;
        s.password = url.password.map(Secret::new);
    } else {
        s.username = username.to_owned();
        s.password = (!password.is_empty()).then(|| Secret::new(password));
    }
    Ok(s)
}

/// An error to show next to a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormError {
    pub field: Field,
    pub message: String,
}

impl Default for StreamForm {
    fn default() -> Self {
        Self::from_stream(&StreamConfig::default())
    }
}

impl StreamForm {
    pub fn from_stream(s: &StreamConfig) -> Self {
        let resolution = OutputFormat::RESOLUTIONS
            .iter()
            .position(|&r| r == (s.output.width, s.output.height))
            .unwrap_or(1);
        let fps = OutputFormat::FRAME_RATES
            .iter()
            .position(|&f| f == s.output.fps)
            .unwrap_or(0);
        Self {
            name: s.name.clone(),
            host: s.host.clone(),
            port: s.port.to_string(),
            path: s.path.clone(),
            username: s.username.clone(),
            password: s
                .password
                .as_ref()
                .and_then(Secret::expose)
                .unwrap_or_default()
                .to_owned(),
            transport: match s.transport {
                Transport::Tcp => 0,
                Transport::Udp => 1,
            },
            resolution,
            fps,
            fit: match s.fit_mode {
                FitMode::Letterbox => 0,
                FitMode::Crop => 1,
                FitMode::Stretch => 2,
            },
            picture: s.picture,
            on_demand: s.on_demand,
            enabled: s.enabled,
        }
    }

    /// Fills host, port, path and credentials from a pasted `rtsp://user:pass@host:port/path`.
    /// The error is ready to show the user.
    pub fn apply_url(&mut self, pasted: &str) -> Result<(), String> {
        let url = parse_stream_url(pasted.trim()).map_err(|e| e.to_string())?;
        self.host = url.host;
        self.port = url.port.to_string();
        self.path = url.path;
        self.username = url.username;
        if let Some(password) = url.password {
            self.password = password;
        }
        Ok(())
    }

    /// Fills the port and path from a brand template. The host, name and login stay as typed.
    pub fn apply_template(&mut self, template: &BrandTemplate) {
        self.port = template.port.to_string();
        self.path = template.path.to_owned();
    }

    /// Builds the stream this form describes, starting from `base` (so the id and any fields
    /// the form does not show are kept). `others` are the other streams, for the unique-name
    /// rule. Returns every problem found, not just the first.
    pub fn to_stream(
        &self,
        base: &StreamConfig,
        others: &[StreamConfig],
    ) -> Result<StreamConfig, Vec<FormError>> {
        let mut errors = Vec::new();
        let mut s = base.clone();
        s.name = self.name.trim().to_owned();
        s.protocol = Protocol::Rtsp;
        s.host = self.host.trim().to_owned();
        s.path = self.path.trim().to_owned();
        s.username = self.username.trim().to_owned();
        s.password = (!self.password.is_empty()).then(|| Secret::new(self.password.clone()));
        s.transport = if self.transport == 1 {
            Transport::Udp
        } else {
            Transport::Tcp
        };
        let (width, height) = OutputFormat::RESOLUTIONS
            .get(self.resolution)
            .copied()
            .unwrap_or((1280, 720));
        s.output.width = width;
        s.output.height = height;
        s.output.fps = OutputFormat::FRAME_RATES
            .get(self.fps)
            .copied()
            .unwrap_or(30);
        s.fit_mode = [FitMode::Letterbox, FitMode::Crop, FitMode::Stretch]
            .get(self.fit)
            .copied()
            .unwrap_or_default();
        s.picture = self.picture;
        s.on_demand = self.on_demand;
        s.enabled = self.enabled;

        let port_ok = match self.port.trim().parse::<u16>() {
            Ok(p) if p > 0 => {
                s.port = p;
                true
            }
            _ => {
                errors.push(FormError {
                    field: Field::Port,
                    message: Problem::InvalidPort.to_string(),
                });
                false
            }
        };

        let mut config = Config {
            streams: others.iter().filter(|o| o.id != s.id).cloned().collect(),
            ..Config::default()
        };
        config.streams.push(s.clone());
        errors.extend(
            config
                .validate()
                .into_iter()
                // Problems with the other streams are not this form's business.
                .filter(|i| i.stream == s.id)
                // An unparsable port was reported above.
                .filter(|i| port_ok || i.field != Field::Port)
                .map(|i| FormError {
                    field: i.field,
                    message: i.problem.to_string(),
                }),
        );
        if errors.is_empty() {
            Ok(s)
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled() -> StreamForm {
        StreamForm {
            name: "Front Door".into(),
            host: "192.168.1.50".into(),
            ..StreamForm::default()
        }
    }

    #[test]
    fn minimal_form_builds_a_default_stream() {
        let base = StreamConfig::default();
        let s = filled().to_stream(&base, &[]).unwrap();
        assert_eq!(s.id, base.id);
        assert_eq!(s.url(), "rtsp://192.168.1.50:554");
        assert_eq!(
            (s.output.width, s.output.height, s.output.fps),
            (1280, 720, 30)
        );
        assert!(s.password.is_none());
    }

    #[test]
    fn existing_stream_round_trips_through_the_form() {
        let mut s = StreamConfig::new("Garage", "cam.local");
        s.port = 8554;
        s.path = "/live".into();
        s.username = "admin".into();
        s.password = Some(Secret::new("pw"));
        s.transport = Transport::Udp;
        s.output.width = 1920;
        s.output.height = 1080;
        s.output.fps = 15;
        s.fit_mode = FitMode::Crop;
        s.on_demand = false;
        let back = StreamForm::from_stream(&s).to_stream(&s, &[]).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn all_problems_are_reported_with_their_field() {
        let form = StreamForm {
            name: " ".into(),
            host: "not a host".into(),
            port: "70000".into(),
            ..StreamForm::default()
        };
        let errors = form.to_stream(&StreamConfig::default(), &[]).unwrap_err();
        let fields: Vec<_> = errors.iter().map(|e| e.field).collect();
        assert!(fields.contains(&Field::Name), "{errors:?}");
        assert!(fields.contains(&Field::Host), "{errors:?}");
        assert!(fields.contains(&Field::Port), "{errors:?}");
        assert_eq!(fields.iter().filter(|f| **f == Field::Port).count(), 1);
    }

    #[test]
    fn names_must_be_unique_but_a_stream_may_keep_its_own() {
        let other = StreamConfig::new("front door", "10.0.0.1");
        let mine = StreamConfig::new("Front Door", "10.0.0.2");
        let form = StreamForm::from_stream(&mine);
        let errors = form
            .to_stream(&mine, std::slice::from_ref(&other))
            .unwrap_err();
        assert!(errors.iter().any(|e| e.field == Field::Name), "{errors:?}");
        // Editing a stream does not clash with its own saved copy.
        assert!(form.to_stream(&mine, std::slice::from_ref(&mine)).is_ok());
    }

    #[test]
    fn pasting_a_url_fills_the_fields() {
        let mut form = filled();
        form.apply_url("rtsp://admin:p%40ss@10.0.0.5:8554/Streaming/Channels/101?x=1")
            .unwrap();
        assert_eq!(form.host, "10.0.0.5");
        assert_eq!(form.port, "8554");
        assert_eq!(form.path, "/Streaming/Channels/101?x=1");
        assert_eq!(form.username, "admin");
        assert_eq!(form.password, "p@ss");
        assert_eq!(form.name, "Front Door", "a URL has no name");
    }

    #[test]
    fn a_template_sets_port_and_path_only() {
        let mut form = StreamForm {
            username: "admin".into(),
            ..filled()
        };
        let template = rtspcam_core::config::BRAND_TEMPLATES
            .iter()
            .find(|t| t.brand == "Hikvision" && t.stream == "sub stream")
            .unwrap();
        form.apply_template(template);
        assert_eq!(form.port, "554");
        assert_eq!(form.path, "/Streaming/Channels/102");
        assert_eq!(
            (form.host.as_str(), form.username.as_str()),
            ("192.168.1.50", "admin")
        );
        let s = form.to_stream(&StreamConfig::default(), &[]).unwrap();
        assert_eq!(s.url(), "rtsp://192.168.1.50:554/Streaming/Channels/102");
    }

    #[test]
    fn onvif_address_becomes_a_stream() {
        let existing = [StreamConfig::new("Tapo C210", "10.0.0.9")];
        let s = stream_from_onvif(
            "Tapo C210",
            "rtsp://192.168.8.78:554/stream1",
            " admin ",
            "pw",
            &existing,
        )
        .unwrap();
        assert_eq!(s.name, "Tapo C210 (2)");
        assert_eq!(s.url(), "rtsp://192.168.8.78:554/stream1");
        assert_eq!(s.credentials(), Some(("admin", "pw")));
        // The address' own login is used when none is typed.
        let s = stream_from_onvif("Cam", "rtsp://u:p%40@h:8554/a?x=1", "", "", &[]).unwrap();
        assert_eq!(s.credentials(), Some(("u", "p@")));
        assert_eq!(s.path, "/a?x=1");
        assert!(stream_from_onvif("Cam", "http://h/a", "", "", &[]).is_err());
    }

    #[test]
    fn crop_edges_are_whole_percentages() {
        assert_eq!(parse_percent(" 25 "), Some(25));
        assert_eq!(parse_percent("0"), Some(0));
        assert_eq!(parse_percent("80"), Some(80));
        for bad in ["", "81", "-1", "2.5", "abc", "300"] {
            assert_eq!(parse_percent(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn picture_settings_survive_the_form() {
        let mut s = StreamConfig::new("Door", "10.0.0.2");
        s.picture.rotate = rtspcam_core::config::Rotation::Cw90;
        s.picture.show_name = true;
        s.picture.on_disconnect = rtspcam_core::config::OnDisconnect::FreezeLastFrame;
        let back = StreamForm::from_stream(&s).to_stream(&s, &[]).unwrap();
        assert_eq!(back.picture, s.picture);
        // A crop that hides everything is reported against the picture field.
        let mut form = StreamForm::from_stream(&s);
        form.picture.crop.left = 80;
        form.picture.crop.right = 80;
        let errors = form.to_stream(&s, &[]).unwrap_err();
        assert!(
            errors.iter().any(|e| e.field == Field::Picture),
            "{errors:?}"
        );
    }

    #[test]
    fn pasting_other_schemes_is_rejected_clearly() {
        let mut form = filled();
        let err = form.apply_url("http://10.0.0.5/video").unwrap_err();
        assert!(err.contains("rtsp"), "{err}");
        assert_eq!(form.host, "192.168.1.50", "form untouched");
    }
}
