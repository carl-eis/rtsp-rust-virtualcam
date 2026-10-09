//! The "Find cameras" dialog: scans the network for ONVIF cameras, asks the chosen one for its
//! streams and returns a stream ready to be reviewed in the Add dialog.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rtspcam_core::config::StreamConfig;
use rtspcam_onvif::{CameraInfo, Credentials, DiscoveredDevice, OnvifError, Profile};
use tokio::runtime::Handle;
use url::Url;
use winsafe::{co, gui, prelude::*};

use rtspcam_engine::form::stream_from_onvif;

const TIMER_ID: usize = 1;
const SCAN_FOR: Duration = Duration::from_secs(4);

/// The camera whose streams are listed: the name to suggest, and its streams.
type ChosenCamera = (String, Vec<Profile>);

/// What a background job found.
enum Outcome {
    Scan(Result<Vec<DiscoveredDevice>, OnvifError>),
    Streams(Result<CameraInfo, OnvifError>),
}

#[derive(Clone)]
pub(crate) struct DiscoverDialog {
    wnd: gui::WindowModal,
    list: gui::ListView<()>,
    scan: gui::Button,
    address: gui::Edit,
    user: gui::Edit,
    pass: gui::Edit,
    query: gui::Button,
    streams: gui::ComboBox,
    status: gui::Label,
    add: gui::Button,
    cancel: gui::Button,

    others: Rc<Vec<StreamConfig>>,
    runtime: Handle,
    inbox: Arc<Mutex<Option<Outcome>>>,
    devices: Rc<RefCell<Vec<DiscoveredDevice>>>,
    camera: Rc<RefCell<Option<ChosenCamera>>>,
    result: Rc<RefCell<Option<StreamConfig>>>,
}

impl DiscoverDialog {
    /// Shows the dialog. Returns the stream to review and add, or `None` if cancelled.
    pub(crate) fn run(
        parent: &impl GuiParent,
        others: Vec<StreamConfig>,
        runtime: Handle,
    ) -> Option<StreamConfig> {
        let wnd = gui::WindowModal::new(gui::WindowModalOpts {
            title: "Find cameras",
            size: (560, 470),
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
        let button = |text: &str, x: i32, y: i32, w: i32| {
            gui::Button::new(
                &wnd,
                gui::ButtonOpts {
                    text,
                    position: (x, y),
                    width: w,
                    height: 26,
                    ..Default::default()
                },
            )
        };
        let edit = |x: i32, y: i32, w: i32, style: co::ES| {
            gui::Edit::new(
                &wnd,
                gui::EditOpts {
                    text: "",
                    position: (x, y),
                    width: w,
                    height: 22,
                    control_style: co::ES::AUTOHSCROLL | style,
                    ..Default::default()
                },
            )
        };

        let mut y = 12;
        label("ONVIF cameras on your network", 12, y, 360);
        let scan = button("Scan again", 448, y, 100);
        y += 34;
        let list = gui::ListView::<()>::new(
            &wnd,
            gui::ListViewOpts {
                position: (12, y),
                size: (536, 140),
                control_style: co::LVS::REPORT | co::LVS::SINGLESEL | co::LVS::SHOWSELALWAYS,
                control_ex_style: co::LVS_EX::FULLROWSELECT,
                columns: &[("Camera", 300), ("Address", 220)],
                ..Default::default()
            },
        );
        y += 152;
        label("Camera address", 12, y, 110);
        let address = edit(130, y, 418, co::ES::LEFT);
        y += 30;
        label("User name", 12, y, 110);
        let user = edit(130, y, 150, co::ES::LEFT);
        label("Password", 296, y, 70);
        let pass = edit(370, y, 178, co::ES::PASSWORD);
        y += 36;
        let query = button("Get streams", 12, y, 110);
        let status = label("", 130, y - 2, 418);
        y += 38;
        label("Stream", 12, y, 110);
        let streams = gui::ComboBox::new(
            &wnd,
            gui::ComboBoxOpts {
                position: (130, y),
                width: 418,
                items: &[],
                ..Default::default()
            },
        );
        y += 40;
        let add = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "Add...",
                position: (370, y + 8),
                width: 80,
                height: 26,
                control_style: co::BS::DEFPUSHBUTTON,
                ..Default::default()
            },
        );
        let cancel = button("Cancel", 458, y + 8, 90);

        let dlg = Self {
            wnd,
            list,
            scan,
            address,
            user,
            pass,
            query,
            streams,
            status,
            add,
            cancel,
            others: Rc::new(others),
            runtime,
            inbox: Arc::default(),
            devices: Rc::default(),
            camera: Rc::default(),
            result: Rc::default(),
        };
        dlg.events();
        dlg.wnd.show_modal(parent).ok()?;
        dlg.result.take()
    }

    fn say(&self, text: &str) {
        let _ = self.status.hwnd().SetWindowText(text);
    }

    fn events(&self) {
        let me = self.clone();
        self.wnd.on().wm_create(move |_| {
            me.add.hwnd().EnableWindow(false);
            me.streams.hwnd().EnableWindow(false);
            let _ = me.wnd.hwnd().SetTimer(TIMER_ID, 150, None);
            me.start_scan();
            Ok(0)
        });

        let me = self.clone();
        self.wnd.on().wm_timer(TIMER_ID, move || {
            me.poll();
            Ok(())
        });

        let me = self.clone();
        self.scan.on().bn_clicked(move || {
            me.start_scan();
            Ok(())
        });

        let me = self.clone();
        self.list.on().lvn_item_changed(move |_| {
            me.on_device_selected();
            Ok(())
        });

        let me = self.clone();
        self.query.on().bn_clicked(move || {
            me.start_query();
            Ok(())
        });

        let me = self.clone();
        self.streams.on().cbn_sel_change(move || {
            me.add.hwnd().EnableWindow(true);
            Ok(())
        });

        let me = self.clone();
        self.add.on().bn_clicked(move || {
            me.finish();
            Ok(())
        });

        let me = self.clone();
        self.cancel.on().bn_clicked(move || {
            me.wnd.close();
            Ok(())
        });
    }

    fn start_scan(&self) {
        self.scan.hwnd().EnableWindow(false);
        self.say("Scanning...");
        let _ = self.list.items().delete_all();
        self.devices.borrow_mut().clear();
        let inbox = self.inbox.clone();
        self.runtime.spawn(async move {
            let found = rtspcam_onvif::discover(SCAN_FOR).await;
            *inbox.lock().unwrap_or_else(PoisonError::into_inner) = Some(Outcome::Scan(found));
        });
    }

    fn start_query(&self) {
        let Some(url) = self.device_url() else {
            self.say("Enter the camera's address, or pick one from the list.");
            return;
        };
        let user = self.user.text().unwrap_or_default();
        let credentials = (!user.trim().is_empty()).then(|| Credentials {
            username: user.trim().to_owned(),
            password: self.pass.text().unwrap_or_default(),
        });
        self.query.hwnd().EnableWindow(false);
        self.add.hwnd().EnableWindow(false);
        self.say("Asking the camera...");
        let inbox = self.inbox.clone();
        self.runtime.spawn(async move {
            let info = rtspcam_onvif::query_camera(&url, credentials.as_ref()).await;
            *inbox.lock().unwrap_or_else(PoisonError::into_inner) = Some(Outcome::Streams(info));
        });
    }

    /// The device service address from the address box: a full URL, or `host[:port]`.
    fn device_url(&self) -> Option<Url> {
        let text = self.address.text().unwrap_or_default();
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let full = if text.contains("://") {
            text.to_owned()
        } else {
            format!("http://{text}/onvif/device_service")
        };
        Url::parse(&full).ok()
    }

    fn on_device_selected(&self) {
        let Some(item) = self.list.items().iter_selected().next() else {
            return;
        };
        let devices = self.devices.borrow();
        let Some(device) = devices.get(item.index() as usize) else {
            return;
        };
        if let Some(url) = device.device_url() {
            let _ = self.address.set_text(url.as_str());
        }
        // A different camera: forget the streams of the last one.
        *self.camera.borrow_mut() = None;
        self.streams.items().delete_all();
        self.streams.hwnd().EnableWindow(false);
        self.add.hwnd().EnableWindow(false);
        self.say("Enter the camera's login and press Get streams.");
    }

    fn poll(&self) {
        let taken = self
            .inbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match taken {
            Some(Outcome::Scan(result)) => self.show_devices(result),
            Some(Outcome::Streams(result)) => self.show_streams(result),
            None => {}
        }
    }

    fn show_devices(&self, result: Result<Vec<DiscoveredDevice>, OnvifError>) {
        self.scan.hwnd().EnableWindow(true);
        match result {
            Ok(devices) if devices.is_empty() => {
                self.say(
                    "No cameras answered. Type the camera's address below, or check that ONVIF \
                     is on and that Windows Firewall allows RTSP Cam.",
                );
            }
            Ok(devices) => {
                for d in &devices {
                    let _ =
                        self.list
                            .items()
                            .add(&[d.title().as_str(), d.host().as_str()], None, ());
                }
                let n = devices.len();
                self.say(&format!(
                    "Found {n} camera{}. Pick one.",
                    if n == 1 { "" } else { "s" }
                ));
                *self.devices.borrow_mut() = devices;
                let _ = self.list.items().get(0).select(true);
            }
            Err(e) => self.say(&format!("Scan failed: {e}")),
        }
    }

    fn show_streams(&self, result: Result<CameraInfo, OnvifError>) {
        self.query.hwnd().EnableWindow(true);
        match result {
            Ok(info) => {
                let name = info
                    .title()
                    .or_else(|| self.selected_title())
                    .unwrap_or_else(|| "Camera".to_owned());
                self.streams.items().delete_all();
                // Main streams first is how cameras list them; keep their order.
                let labels: Vec<String> = info.profiles.iter().map(Profile::label).collect();
                let _ = self.streams.items().add(&labels);
                self.streams.items().select(Some(0));
                self.streams.hwnd().EnableWindow(true);
                self.add.hwnd().EnableWindow(true);
                self.say(&format!(
                    "{name}: {} streams. Choose one.",
                    info.profiles.len()
                ));
                *self.camera.borrow_mut() = Some((name, info.profiles));
            }
            Err(e) => {
                let hint = e.hint().unwrap_or_default();
                self.say(&format!("{e}. {hint}"));
            }
        }
    }

    fn selected_title(&self) -> Option<String> {
        let item = self.list.items().iter_selected().next()?;
        self.devices
            .borrow()
            .get(item.index() as usize)
            .map(DiscoveredDevice::title)
    }

    fn finish(&self) {
        let camera = self.camera.borrow();
        let Some((name, profiles)) = camera.as_ref() else {
            return;
        };
        let Some(profile) = self
            .streams
            .items()
            .selected_index()
            .and_then(|i| profiles.get(i as usize))
        else {
            return;
        };
        match stream_from_onvif(
            name,
            &profile.rtsp_uri,
            &self.user.text().unwrap_or_default(),
            &self.pass.text().unwrap_or_default(),
            &self.others,
        ) {
            Ok(stream) => {
                *self.result.borrow_mut() = Some(stream);
                self.wnd.close();
            }
            Err(e) => self.say(&format!("The camera's stream address is not usable: {e}")),
        }
    }
}
