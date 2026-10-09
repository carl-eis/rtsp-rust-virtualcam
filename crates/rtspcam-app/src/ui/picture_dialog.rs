//! The "Picture" dialog: rotate, flip, crop, text overlay and what to do when the stream drops.

use std::rc::Rc;

use rtspcam_core::config::{Crop, MAX_CROP_PERCENT, OnDisconnect, Picture, Rotation};
use rtspcam_engine::form::{DISCONNECT_LABELS, ROTATION_LABELS, parse_percent};
use slint::{ComponentHandle as _, ModelRc, SharedString, VecModel};

use super::app::App;
use super::generated::PictureDialog;

/// What happens with the settings when OK is pressed.
pub(crate) type OnOk = Box<dyn FnOnce(&Rc<App>, Picture)>;

pub(crate) struct State {
    on_ok: Option<OnOk>,
}

fn labels(items: &[&str]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        items
            .iter()
            .map(|s| SharedString::from(*s))
            .collect::<Vec<_>>(),
    ))
}

/// Shows the dialog with `current` filled in.
pub(crate) fn open(app: &Rc<App>, current: Picture, on_ok: OnOk) {
    let Some(ui) = app.ui() else { return };
    let d = ui.global::<PictureDialog>();
    d.set_rotations(labels(&ROTATION_LABELS));
    d.set_rotation(
        Rotation::ALL
            .iter()
            .position(|r| *r == current.rotate)
            .unwrap_or(0) as i32,
    );
    d.set_flip_horizontal(current.flip_horizontal);
    d.set_flip_vertical(current.flip_vertical);
    d.set_max_crop(i32::from(MAX_CROP_PERCENT));
    let c = current.crop;
    d.set_crop_left(c.left.to_string().into());
    d.set_crop_top(c.top.to_string().into());
    d.set_crop_right(c.right.to_string().into());
    d.set_crop_bottom(c.bottom.to_string().into());
    d.set_show_name(current.show_name);
    d.set_show_time(current.show_time);
    d.set_disconnects(labels(&DISCONNECT_LABELS));
    d.set_disconnect(match current.on_disconnect {
        OnDisconnect::NoSignal => 0,
        OnDisconnect::FreezeLastFrame => 1,
    });
    d.set_error(SharedString::default());
    *app.picture_dialog.borrow_mut() = Some(State { on_ok: Some(on_ok) });
    d.set_open(true);
}

/// The picture the sheet describes, or what is wrong with it.
fn read(d: &PictureDialog<'_>) -> Result<Picture, String> {
    let side = |text: SharedString, name: &str| {
        parse_percent(&text)
            .ok_or_else(|| format!("{name} crop must be a number from 0 to {MAX_CROP_PERCENT}."))
    };
    let crop = Crop {
        left: side(d.get_crop_left(), "Left")?,
        top: side(d.get_crop_top(), "Top")?,
        right: side(d.get_crop_right(), "Right")?,
        bottom: side(d.get_crop_bottom(), "Bottom")?,
    };
    if !crop.is_valid() {
        return Err(
            "The crop must leave part of the picture: at most 90% in total \
                    across the width, and the same across the height."
                .to_owned(),
        );
    }
    Ok(Picture {
        rotate: usize::try_from(d.get_rotation())
            .ok()
            .and_then(|i| Rotation::ALL.get(i))
            .copied()
            .unwrap_or_default(),
        flip_horizontal: d.get_flip_horizontal(),
        flip_vertical: d.get_flip_vertical(),
        crop,
        show_name: d.get_show_name(),
        show_time: d.get_show_time(),
        on_disconnect: if d.get_disconnect() == 1 {
            OnDisconnect::FreezeLastFrame
        } else {
            OnDisconnect::NoSignal
        },
    })
}

/// Wires the sheet's callbacks. Called once.
pub(crate) fn connect(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let d = ui.global::<PictureDialog>();

    let me = Rc::downgrade(app);
    d.on_ok(move || {
        let Some(app) = me.upgrade() else { return };
        let Some(ui) = app.ui() else { return };
        let d = ui.global::<PictureDialog>();
        match read(&d) {
            Ok(picture) => {
                d.set_open(false);
                let on_ok = app
                    .picture_dialog
                    .borrow_mut()
                    .take()
                    .and_then(|mut s| s.on_ok.take());
                if let Some(on_ok) = on_ok {
                    on_ok(&app, picture);
                }
            }
            Err(message) => d.set_error(message.into()),
        }
    });

    let me = Rc::downgrade(app);
    d.on_cancel(move || {
        if let Some(app) = me.upgrade() {
            if let Some(ui) = app.ui() {
                ui.global::<PictureDialog>().set_open(false);
            }
            app.picture_dialog.borrow_mut().take();
        }
    });
}
