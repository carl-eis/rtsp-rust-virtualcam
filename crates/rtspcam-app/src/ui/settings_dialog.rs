//! The Settings dialog.

use std::cell::RefCell;
use std::rc::Rc;

use rtspcam_core::config::AppSettings;
use rtspcam_core::{LogLevel, constants::APP_DISPLAY_NAME};
use winsafe::{co, gui, prelude::*};

const LEVELS: [(&str, LogLevel); 5] = [
    ("Error", LogLevel::Error),
    ("Warning", LogLevel::Warn),
    ("Info", LogLevel::Info),
    ("Debug", LogLevel::Debug),
    ("Trace", LogLevel::Trace),
];

#[derive(Clone)]
pub(crate) struct SettingsDialog {
    wnd: gui::WindowModal,
    tray: gui::CheckBox,
    autostart: gui::CheckBox,
    level: gui::ComboBox,
    ok: gui::Button,
    cancel: gui::Button,
    result: Rc<RefCell<Option<AppSettings>>>,
    current: Rc<AppSettings>,
}

impl SettingsDialog {
    /// Shows the dialog; returns the new settings if OK was pressed.
    pub(crate) fn run(parent: &impl GuiParent, current: &AppSettings) -> Option<AppSettings> {
        let wnd = gui::WindowModal::new(gui::WindowModalOpts {
            title: &format!("{APP_DISPLAY_NAME} settings"),
            size: (340, 190),
            ..Default::default()
        });
        let check = |text: &str, y: i32, on: bool| {
            gui::CheckBox::new(
                &wnd,
                gui::CheckBoxOpts {
                    text,
                    position: (16, y),
                    size: (300, 20),
                    check_state: if on {
                        co::BST::CHECKED
                    } else {
                        co::BST::UNCHECKED
                    },
                    ..Default::default()
                },
            )
        };
        let tray = check("Minimize to tray", 16, current.minimize_to_tray);
        let autostart = check("Start with Windows", 44, current.start_with_windows);
        let _ = gui::Label::new(
            &wnd,
            gui::LabelOpts {
                text: "Log level",
                position: (16, 80),
                size: (80, 20),
                ..Default::default()
            },
        );
        let names: Vec<&str> = LEVELS.iter().map(|(n, _)| *n).collect();
        let selected = LEVELS
            .iter()
            .position(|(_, l)| *l == current.log_level)
            .unwrap_or(2);
        let level = gui::ComboBox::new(
            &wnd,
            gui::ComboBoxOpts {
                position: (100, 76),
                width: 120,
                items: &names,
                selected_item: Some(selected as u32),
                ..Default::default()
            },
        );
        let ok = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "OK",
                position: (150, 126),
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
                position: (238, 126),
                width: 80,
                height: 26,
                ..Default::default()
            },
        );
        let dlg = Self {
            wnd,
            tray,
            autostart,
            level,
            ok,
            cancel,
            result: Rc::default(),
            current: Rc::new(current.clone()),
        };
        dlg.events();
        dlg.wnd.show_modal(parent).ok()?;
        dlg.result.take()
    }

    fn events(&self) {
        let me = self.clone();
        self.ok.on().bn_clicked(move || {
            let mut s = (*me.current).clone();
            s.minimize_to_tray = me.tray.is_checked();
            s.start_with_windows = me.autostart.is_checked();
            let index = me.level.items().selected_index().unwrap_or(2) as usize;
            s.log_level = LEVELS.get(index).map_or(s.log_level, |(_, l)| *l);
            *me.result.borrow_mut() = Some(s);
            me.wnd.close();
            Ok(())
        });
        let me = self.clone();
        self.cancel.on().bn_clicked(move || {
            me.wnd.close();
            Ok(())
        });
    }
}
