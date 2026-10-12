use cpal::traits::{DeviceTrait as _, HostTrait as _};

use crate::error::AudioError;

/// The device id that follows the system's default device (input or output).
pub const DEFAULT_DEVICE: &str = "default";

const INPUT_PREFIX: &str = "input:";
const OUTPUT_PREFIX: &str = "output:";

/// What a device does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// Records: a microphone, line in, or a virtual cable's output.
    Input,
    /// Plays. Where [`loopback_supported`] is true it can also be recorded ("what the PC
    /// plays").
    Output,
}

/// One audio device, as shown in a picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable across restarts: `input:` or `output:` followed by the OS's own device id. Two
    /// devices with the same name still get different ids.
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    /// The system's current default for its kind.
    pub is_default: bool,
}

/// Whether output devices can be recorded (WASAPI loopback, as TheEpicAudioStreamer does).
/// Elsewhere what the PC plays is recorded through the inputs: PipeWire and PulseAudio list
/// "Monitor of ..." sources as inputs already.
pub fn loopback_supported() -> bool {
    cpal::default_host().id().name() == "WASAPI"
}

/// The input devices, then the output devices, each sorted by name. Slow (it asks the OS about
/// every device); call it from a worker thread.
pub fn devices() -> Result<Vec<DeviceInfo>, AudioError> {
    let host = cpal::default_host();
    let default_id = |device: Option<cpal::Device>| device.and_then(|d| d.id().ok());
    let default_input = default_id(host.default_input_device());
    let default_output = default_id(host.default_output_device());

    let mut list = Vec::new();
    for (kind, devices, prefix, default) in [
        (
            DeviceKind::Input,
            host.input_devices().map_err(AudioError::from)?,
            INPUT_PREFIX,
            &default_input,
        ),
        (
            DeviceKind::Output,
            host.output_devices().map_err(AudioError::from)?,
            OUTPUT_PREFIX,
            &default_output,
        ),
    ] {
        let mut of_kind: Vec<DeviceInfo> = devices
            .filter_map(|device| {
                let id = device.id().ok()?;
                let name = device
                    .description()
                    .map(|d| d.name().to_owned())
                    .unwrap_or_else(|_| id.to_string());
                Some(DeviceInfo {
                    is_default: default.as_ref() == Some(&id),
                    id: format!("{prefix}{id}"),
                    name,
                    kind,
                })
            })
            .collect();
        of_kind.sort_by_key(|d| d.name.to_lowercase());
        list.extend(of_kind);
    }
    Ok(list)
}

/// Finds the device to record from: an input, or (for loopback) an output. Returns the device
/// and whether it is an output.
pub(crate) fn find_source(id: &str) -> Result<(cpal::Device, bool), AudioError> {
    let host = cpal::default_host();
    if id == DEFAULT_DEVICE {
        let device = host
            .default_input_device()
            .ok_or(AudioError::NoDefault("input"))?;
        return Ok((device, false));
    }
    if let Some(rest) = id.strip_prefix(INPUT_PREFIX) {
        return Ok((find(&host, rest, id)?, false));
    }
    if let Some(rest) = id.strip_prefix(OUTPUT_PREFIX) {
        return Ok((find(&host, rest, id)?, true));
    }
    Err(AudioError::NotFound(id.to_owned()))
}

/// Finds the device to play on.
pub(crate) fn find_output(id: &str) -> Result<cpal::Device, AudioError> {
    let host = cpal::default_host();
    if id == DEFAULT_DEVICE {
        return host
            .default_output_device()
            .ok_or(AudioError::NoDefault("output"));
    }
    match id.strip_prefix(OUTPUT_PREFIX) {
        Some(rest) => find(&host, rest, id),
        None => Err(AudioError::NotFound(id.to_owned())),
    }
}

fn find(host: &cpal::Host, os_id: &str, id: &str) -> Result<cpal::Device, AudioError> {
    let not_found = || AudioError::NotFound(id.to_owned());
    let parsed: cpal::DeviceId = os_id.parse().map_err(|_| not_found())?;
    host.device_by_id(&parsed).ok_or_else(not_found)
}

/// The device's name, for logs and the status line.
pub(crate) fn name_of(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "audio device".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_ids_are_not_found() {
        assert!(matches!(find_source("bogus"), Err(AudioError::NotFound(_))));
        assert!(matches!(
            find_output("input:whatever"),
            Err(AudioError::NotFound(_))
        ));
    }
}
