//! The "Picture" dialog: rotate, flip, crop, text overlay and what to do when the stream drops.

use std::cell::RefCell;
use std::rc::Rc;

use rtspcam_core::config::{Crop, MAX_CROP_PERCENT, OnDisconnect, Picture, Rotation};
use winsafe::{co, gui, prelude::*};

use crate::form::{DISCONNECT_LABELS, ROTATION_LABELS, parse_percent};

#[derive(Clone)]
pub(crate) struct PictureDialog {
    wnd: gui::WindowModal,
    rotate: gui::ComboBox,
    flip_h: gui::CheckBox,
    flip_v: gui::CheckBox,
    crop: [gui::Edit; 4],
    show_name: gui::CheckBox,
    show_time: gui::CheckBox,
    disconnect: gui::ComboBox,
    error: gui::Label,
    ok: gui::Button,
    cancel: gui::Button,
    result: Rc<RefCell<Option<Picture>>>,
}

impl PictureDialog {
    /// Shows the dialog; returns the new settings if OK was pressed.
    pub(crate) fn run(parent: &impl GuiParent, current: &Picture) -> Option<Picture> {
        let wnd = gui::WindowModal::new(gui::WindowModalOpts {
            title: "Picture",
            size: (400, 372),
            ..Default::default()
        });
        let label = |text: &str, x: i32, y: i32, w: i32| {
            gui::Label::new(
                &wnd,
                gui::LabelOpts {
                    text,
                    position: (x, y + 4),
                    size: (w, 20),
                    ..Default::default()
                },
            )
        };
        let check = |text: &str, x: i32, y: i32, on: bool| {
            gui::CheckBox::new(
                &wnd,
                gui::CheckBoxOpts {
                    text,
                    position: (x, y),
                    size: (180, 20),
                    check_state: if on {
                        co::BST::CHECKED
                    } else {
                        co::BST::UNCHECKED
                    },
                    ..Default::default()
                },
            )
        };
        let combo = |y: i32, items: &[&str], selected: usize| {
            gui::ComboBox::new(
                &wnd,
                gui::ComboBoxOpts {
                    position: (150, y),
                    width: 230,
                    items,
                    selected_item: Some(selected as u32),
                    ..Default::default()
                },
            )
        };

        let mut y = 14;
        label("Rotate", 16, y, 120);
        let rotation = Rotation::ALL
            .iter()
            .position(|r| *r == current.rotate)
            .unwrap_or(0);
        let rotate = combo(y, &ROTATION_LABELS, rotation);
        y += 34;
        let flip_h = check("Flip horizontally", 150, y, current.flip_horizontal);
        y += 26;
        let flip_v = check("Flip vertically", 150, y, current.flip_vertical);
        y += 38;

        label(
            &format!("Crop (percent of each side, 0 to {MAX_CROP_PERCENT})"),
            16,
            y,
            360,
        );
        y += 28;
        let c = current.crop;
        let mut crop = Vec::new();
        for (i, (name, value)) in [
            ("Left", c.left),
            ("Top", c.top),
            ("Right", c.right),
            ("Bottom", c.bottom),
        ]
        .into_iter()
        .enumerate()
        {
            let x = 16 + i as i32 * 92;
            label(name, x, y, 60);
            crop.push(gui::Edit::new(
                &wnd,
                gui::EditOpts {
                    text: &value.to_string(),
                    position: (x, y + 24),
                    width: 70,
                    height: 22,
                    control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
                    ..Default::default()
                },
            ));
        }
        let crop: [gui::Edit; 4] = crop.try_into().ok()?;
        y += 62;

        let show_name = check("Show the stream name", 16, y, current.show_name);
        y += 26;
        let show_time = check("Show date and time", 16, y, current.show_time);
        y += 40;

        label("When the stream drops", 16, y, 130);
        let selected = match current.on_disconnect {
            OnDisconnect::NoSignal => 0,
            OnDisconnect::FreezeLastFrame => 1,
        };
        let disconnect = combo(y, &DISCONNECT_LABELS, selected);
        y += 40;

        let error = gui::Label::new(
            &wnd,
            gui::LabelOpts {
                text: "",
                position: (16, y),
                size: (366, 20),
                ..Default::default()
            },
        );
        y += 26;
        let ok = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "OK",
                position: (212, y),
                width: 80,
                height: 26,
                control_style: co::BS::DEFPUSHBUTTON,
                ..Default::default()
            },
        );
        let cancel = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "Cancel",
                position: (300, y),
                width: 80,
                height: 26,
                ..Default::default()
            },
        );

        let dlg = Self {
            wnd,
            rotate,
            flip_h,
            flip_v,
            crop,
            show_name,
            show_time,
            disconnect,
            error,
            ok,
            cancel,
            result: Rc::default(),
        };
        dlg.events();
        dlg.wnd.show_modal(parent).ok()?;
        dlg.result.take()
    }

    /// The picture the controls describe, or what is wrong with them.
    fn read(&self) -> Result<Picture, String> {
        let side = |edit: &gui::Edit, name: &str| {
            parse_percent(&edit.text().unwrap_or_default()).ok_or_else(|| {
                format!("{name} crop must be a number from 0 to {MAX_CROP_PERCENT}.")
            })
        };
        let crop = Crop {
            left: side(&self.crop[0], "Left")?,
            top: side(&self.crop[1], "Top")?,
            right: side(&self.crop[2], "Right")?,
            bottom: side(&self.crop[3], "Bottom")?,
        };
        if !crop.is_valid() {
            return Err(
                "The crop must leave part of the picture: at most 90% in total \
                        across the width, and the same across the height."
                    .to_owned(),
            );
        }
        Ok(Picture {
            rotate: Rotation::ALL
                .get(self.rotate.items().selected_index().unwrap_or(0) as usize)
                .copied()
                .unwrap_or_default(),
            flip_horizontal: self.flip_h.is_checked(),
            flip_vertical: self.flip_v.is_checked(),
            crop,
            show_name: self.show_name.is_checked(),
            show_time: self.show_time.is_checked(),
            on_disconnect: if self.disconnect.items().selected_index() == Some(1) {
                OnDisconnect::FreezeLastFrame
            } else {
                OnDisconnect::NoSignal
            },
        })
    }

    fn events(&self) {
        let me = self.clone();
        self.ok.on().bn_clicked(move || {
            match me.read() {
                Ok(picture) => {
                    *me.result.borrow_mut() = Some(picture);
                    me.wnd.close();
                }
                Err(message) => {
                    let _ = me.error.hwnd().SetWindowText(&message);
                }
            }
            Ok(())
        });
        let me = self.clone();
        self.cancel.on().bn_clicked(move || {
            me.wnd.close();
            Ok(())
        });
    }
}
