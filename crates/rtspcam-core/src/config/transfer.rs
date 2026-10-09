//! Exporting streams to a file and importing them from one.
//!
//! An export is an ordinary config file with only the streams: no settings, no passwords. Import
//! adds the file's streams to the current config under new ids, renaming any whose name is
//! taken, and skips streams that would not pass validation.

use serde_json::Value;
use uuid::Uuid;

use super::migrate::migrate;
use super::{Config, StreamConfig};
use crate::error::ConfigError;

/// Why a file could not be exported or imported as a whole.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("this is not a valid RTSP Cam file: {0}")]
    Invalid(String),
    #[error("the file has no streams")]
    Empty,
    #[error(transparent)]
    Config(#[from] ConfigError),
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Names of the streams that were added (after any renaming).
    pub added: Vec<String>,
    /// Streams that were added under a different name because theirs was taken: (old, new).
    pub renamed: Vec<(String, String)>,
    /// Streams that were left out, with the reason.
    pub skipped: Vec<(String, String)>,
}

impl ImportReport {
    /// A few lines for a message box.
    pub fn summary(&self) -> String {
        let mut text = match self.added.len() {
            0 => "No streams were imported.".to_owned(),
            1 => "Imported 1 stream.".to_owned(),
            n => format!("Imported {n} streams."),
        };
        if !self.renamed.is_empty() {
            text.push_str("\r\n\r\nRenamed because the name was taken:");
            for (old, new) in &self.renamed {
                text.push_str(&format!("\r\n  {old} -> {new}"));
            }
        }
        if !self.skipped.is_empty() {
            text.push_str("\r\n\r\nSkipped:");
            for (name, why) in &self.skipped {
                text.push_str(&format!("\r\n  {name}: {why}"));
            }
        }
        if !self.added.is_empty() {
            text.push_str(
                "\r\n\r\nPasswords are not part of a file; enter them again for each stream.",
            );
        }
        text
    }
}

/// The text of an export file for `streams`: passwords left out, everything else kept.
pub fn export_streams(streams: &[StreamConfig]) -> Result<String, ExportError> {
    let config = Config {
        streams: streams
            .iter()
            .map(|s| StreamConfig {
                password: None,
                ..s.clone()
            })
            .collect(),
        ..Config::default()
    };
    let mut text = serde_json::to_string_pretty(&config)
        .map_err(|e| ExportError::Config(ConfigError::Serialize(e)))?;
    text.push('\n');
    Ok(text)
}

/// Adds the streams in `text` (an export file, or any `config.json`) to `config`.
///
/// Settings in the file are ignored. A file password in plain text (written by hand) is kept and
/// gets encrypted when the config is saved; an encrypted one from another user or PC is dropped,
/// because it could never be decrypted here.
pub fn import_streams(config: &mut Config, text: &str) -> Result<ImportReport, ExportError> {
    let mut doc: Value =
        serde_json::from_str(text).map_err(|e| ExportError::Invalid(e.to_string()))?;
    if !doc.is_object() {
        return Err(ExportError::Invalid("expected a JSON object".to_owned()));
    }
    migrate(&mut doc)?;
    let incoming: Config =
        serde_json::from_value(doc).map_err(|e| ExportError::Invalid(e.to_string()))?;
    if incoming.streams.is_empty() {
        return Err(ExportError::Empty);
    }

    let mut report = ImportReport::default();
    for mut stream in incoming.streams {
        let original = stream.name.trim().to_owned();
        stream.id = Uuid::new_v4();
        if stream
            .password
            .as_ref()
            .is_some_and(|p| p.expose().is_none())
        {
            stream.password = None;
        }
        stream.name = unique_stream_name(&config.streams, &original);

        let issues = stream.validate();
        if let Some(first) = issues.first() {
            let label = if original.is_empty() {
                stream.host.clone()
            } else {
                original
            };
            report.skipped.push((label, first.problem.to_string()));
            continue;
        }
        if stream.name != original {
            report.renamed.push((original, stream.name.clone()));
        }
        report.added.push(stream.name.clone());
        config.streams.push(stream);
    }
    Ok(report)
}

/// `name`, or `name (2)`, `name (3)`, ... if another stream (compared ignoring case) already
/// has it.
pub fn unique_stream_name(streams: &[StreamConfig], name: &str) -> String {
    let taken = |candidate: &str| {
        let key = candidate.to_lowercase();
        streams.iter().any(|s| s.name.trim().to_lowercase() == key)
    };
    if name.is_empty() || !taken(name) {
        return name.to_owned();
    }
    (2..)
        .map(|n| format!("{name} ({n})"))
        .find(|candidate| !taken(candidate))
        .expect("an unused number exists")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::Picture;

    fn stream(name: &str, host: &str) -> StreamConfig {
        StreamConfig::new(name, host)
    }

    #[test]
    fn export_leaves_passwords_out() {
        crate::secret::test_store::install();
        let mut s = stream("Front", "10.0.0.2");
        s.username = "admin".into();
        s.password = Some(crate::Secret::new("hunter2"));
        let text = export_streams(&[s]).unwrap();
        assert!(!text.contains("hunter2") && !text.contains("keyfile"));
        assert!(text.contains("\"username\": \"admin\""));
    }

    #[test]
    fn export_then_import_round_trips_with_new_ids() {
        let mut a = stream("Front", "10.0.0.2");
        a.picture = Picture {
            show_name: true,
            ..Picture::default()
        };
        a.port = 8554;
        let text = export_streams(std::slice::from_ref(&a)).unwrap();

        let mut config = Config::default();
        let report = import_streams(&mut config, &text).unwrap();
        assert_eq!(report.added, ["Front"]);
        let b = &config.streams[0];
        assert_ne!(b.id, a.id);
        assert_eq!(
            StreamConfig {
                id: a.id,
                ..b.clone()
            },
            a
        );
    }

    #[test]
    fn taken_names_get_a_number() {
        let mut config = Config::default();
        config.streams.push(stream("Front", "10.0.0.1"));
        config.streams.push(stream("Front (2)", "10.0.0.1"));
        let text = export_streams(&[stream("front", "10.0.0.9")]).unwrap();
        let report = import_streams(&mut config, &text).unwrap();
        assert_eq!(
            report.renamed,
            [("front".to_owned(), "front (3)".to_owned())]
        );
        assert_eq!(config.streams.len(), 3);
        assert!(config.validate().is_empty());
    }

    #[test]
    fn invalid_streams_are_skipped_not_fatal() {
        let text = json!({
            "streams": [
                { "name": "Good", "host": "10.0.0.2" },
                { "name": "Bad", "host": "not a host" },
                { "name": "", "host": "10.0.0.3" }
            ]
        })
        .to_string();
        let mut config = Config::default();
        let report = import_streams(&mut config, &text).unwrap();
        assert_eq!(report.added, ["Good"]);
        assert_eq!(report.skipped.len(), 2);
        assert!(report.skipped[0].1.contains("not a host"));
        assert!(report.summary().contains("Skipped"));
    }

    #[test]
    fn settings_in_the_file_are_ignored() {
        let text = json!({
            "settings": { "minimize_to_tray": true },
            "streams": [{ "name": "A", "host": "10.0.0.2" }]
        })
        .to_string();
        let mut config = Config::default();
        import_streams(&mut config, &text).unwrap();
        assert!(!config.settings.minimize_to_tray);
    }

    #[test]
    fn bad_files_are_rejected_with_a_message() {
        let mut config = Config::default();
        assert!(matches!(
            import_streams(&mut config, "not json"),
            Err(ExportError::Invalid(_))
        ));
        assert!(matches!(
            import_streams(&mut config, "[1]"),
            Err(ExportError::Invalid(_))
        ));
        assert!(matches!(
            import_streams(&mut config, "{}"),
            Err(ExportError::Empty)
        ));
        assert!(config.streams.is_empty());
    }

    #[test]
    fn a_crop_that_hides_everything_is_skipped() {
        let text = json!({
            "streams": [{ "name": "A", "host": "10.0.0.2",
                "picture": { "crop": { "left": 60, "right": 60 } } }]
        })
        .to_string();
        let mut config = Config::default();
        let report = import_streams(&mut config, &text).unwrap();
        assert!(report.added.is_empty() && report.skipped.len() == 1);
    }
}
