//! Per-camera picture adjustments: rotate/flip, crop, text overlay and what to show when the
//! stream drops.

use serde::{Deserialize, Serialize};

/// Largest share of a side that may be cropped away, in percent. Keeps something visible.
pub const MAX_CROP_PERCENT: u8 = 80;

/// Clockwise rotation, applied after the crop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Rotation {
    #[default]
    #[serde(rename = "0")]
    None,
    #[serde(rename = "90")]
    Cw90,
    #[serde(rename = "180")]
    Cw180,
    #[serde(rename = "270")]
    Cw270,
}

impl Rotation {
    pub const ALL: [Self; 4] = [Self::None, Self::Cw90, Self::Cw180, Self::Cw270];

    pub fn degrees(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Cw90 => 90,
            Self::Cw180 => 180,
            Self::Cw270 => 270,
        }
    }
}

/// Edges to cut off the source picture, each as a percentage of that side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Crop {
    pub left: u8,
    pub top: u8,
    pub right: u8,
    pub bottom: u8,
}

impl Crop {
    pub fn is_none(&self) -> bool {
        *self == Self::default()
    }

    /// Whether the cut-offs leave a usable picture.
    pub fn is_valid(&self) -> bool {
        let ok = |a: u8, b: u8| a <= MAX_CROP_PERCENT && b <= MAX_CROP_PERCENT && a + b <= 90;
        ok(self.left, self.right) && ok(self.top, self.bottom)
    }
}

/// What the virtual camera shows when the RTSP stream is down.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnDisconnect {
    /// Apps see a "No signal" picture.
    #[default]
    NoSignal,
    /// Apps keep seeing the last picture.
    FreezeLastFrame,
}

/// How a stream's picture is adjusted before it is scaled to the camera's output size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Picture {
    pub rotate: Rotation,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
    pub crop: Crop,
    /// Draw the stream's name in a corner of the picture.
    pub show_name: bool,
    /// Draw the current date and time in a corner of the picture.
    pub show_time: bool,
    pub on_disconnect: OnDisconnect,
}

impl Picture {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether frames need changing at all (everything except [`on_disconnect`](Self::on_disconnect)).
    pub fn changes_frames(&self) -> bool {
        self.rotate != Rotation::None
            || self.flip_horizontal
            || self.flip_vertical
            || !self.crop.is_none()
            || self.show_name
            || self.show_time
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_change_nothing() {
        let p = Picture::default();
        assert!(p.is_default() && !p.changes_frames());
        assert!(p.crop.is_valid());
    }

    #[test]
    fn serde_names() {
        let p: Picture = serde_json::from_value(json!({
            "rotate": "90",
            "crop": { "left": 10 },
            "on_disconnect": "freeze_last_frame",
            "flip_horizontal": true
        }))
        .unwrap();
        assert_eq!(p.rotate, Rotation::Cw90);
        assert_eq!(p.crop.left, 10);
        assert_eq!(p.on_disconnect, OnDisconnect::FreezeLastFrame);
        assert!(p.changes_frames());
        let out = serde_json::to_value(p).unwrap();
        assert_eq!(out["rotate"], "90");
    }

    #[test]
    fn crop_limits() {
        assert!(
            Crop {
                left: 40,
                right: 40,
                ..Crop::default()
            }
            .is_valid()
        );
        assert!(
            !Crop {
                left: 50,
                right: 50,
                ..Crop::default()
            }
            .is_valid()
        );
        assert!(
            !Crop {
                top: 81,
                ..Crop::default()
            }
            .is_valid()
        );
    }
}
