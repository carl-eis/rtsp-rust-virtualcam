//! Schema migrations, applied to the raw JSON before it is deserialized.

use serde_json::{Map, Value};

use crate::error::ConfigError;

/// Schema version written by this build.
pub const CURRENT_VERSION: u64 = 1;

/// One step of a schema migration, on the raw JSON object.
pub type Migration = fn(&mut Map<String, Value>) -> Result<(), String>;

/// `MIGRATIONS[i]` upgrades a document from version `i + 1` to `i + 2`.
/// Append a function here (and bump [`CURRENT_VERSION`]) when the schema changes.
pub(crate) const MIGRATIONS: &[Migration] = &[];

const _: () = assert!(MIGRATIONS.len() as u64 + 1 == CURRENT_VERSION);

/// Upgrades `doc` in place to `current` with `migrations` (see [`MIGRATIONS`]) and returns
/// the version it had before.
///
/// A missing `version` (for example a hand-written file) counts as version 1.
pub(crate) fn migrate(
    doc: &mut Value,
    current: u64,
    migrations: &[Migration],
) -> Result<u64, ConfigError> {
    let Some(obj) = doc.as_object_mut() else {
        // Not an object: let deserialization produce the error.
        return Ok(current);
    };
    let found = match obj.get("version") {
        None | Some(Value::Null) => 1,
        Some(v) => v.as_u64().ok_or_else(|| ConfigError::Migration {
            from: 0,
            reason: format!("\"version\" must be a positive integer, found {v}"),
        })?,
    }
    .max(1);

    if found > current {
        return Err(ConfigError::UnsupportedVersion {
            found,
            supported: current,
        });
    }
    for (from, step) in (found..).zip(&migrations[(found - 1) as usize..]) {
        step(obj).map_err(|reason| ConfigError::Migration { from, reason })?;
    }
    obj.insert("version".into(), current.into());
    Ok(found)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn missing_version_is_current() {
        let mut doc = json!({ "streams": [] });
        assert_eq!(migrate(&mut doc, CURRENT_VERSION, MIGRATIONS).unwrap(), 1);
        assert_eq!(doc["version"], CURRENT_VERSION);
    }

    #[test]
    fn newer_version_is_rejected() {
        let mut doc = json!({ "version": CURRENT_VERSION + 1 });
        assert!(matches!(
            migrate(&mut doc, CURRENT_VERSION, MIGRATIONS),
            Err(ConfigError::UnsupportedVersion { .. })
        ));
    }

    #[test]
    fn bad_version_is_rejected() {
        let mut doc = json!({ "version": "one" });
        assert!(matches!(
            migrate(&mut doc, CURRENT_VERSION, MIGRATIONS),
            Err(ConfigError::Migration { .. })
        ));
    }
}
