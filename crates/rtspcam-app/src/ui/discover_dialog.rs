//! The "Find cameras" dialog: scans the network for ONVIF cameras, asks the chosen one for its
//! streams and hands back a stream to review in the Add dialog.

use std::rc::Rc;

use rtspcam_core::config::StreamConfig;
use rtspcam_engine::discovery::{SCAN_FOR, device_url, found_text};
use rtspcam_engine::form::stream_from_onvif;
use rtspcam_onvif::{CameraInfo, Credentials, DiscoveredDevice, OnvifError, Profile};
use slint::{ComponentHandle as _, ModelRc, SharedString, StandardListViewItem, VecModel};

use super::app::{App, with_app};
use super::generated::DiscoverDialog;

/// What happens with the stream the user picked.
pub(crate) type OnFound = Box<dyn FnOnce(&Rc<App>, StreamConfig)>;

pub(crate) struct State {
    others: Vec<StreamConfig>,
    devices: Vec<DiscoveredDevice>,
    /// The camera whose streams are listed: the name to suggest, and its streams.
    camera: Option<(String, Vec<Profile>)>,
    on_found: Option<OnFound>,
}

/// Shows the dialog and starts a scan.
pub(crate) fn open(app: &Rc<App>, others: Vec<StreamConfig>, on_found: OnFound) {
    let Some(ui) = app.ui() else { return };
    let d = ui.global::<DiscoverDialog>();
    d.set_devices(ModelRc::default());
    d.set_device(-1);
    d.set_address(SharedString::default());
    d.set_username(SharedString::default());
    d.set_password(SharedString::default());
    d.set_streams(ModelRc::default());
    d.set_stream(0);
    d.set_add_enabled(false);
    d.set_querying(false);
    *app.discover_dialog.borrow_mut() = Some(State {
        others,
        devices: Vec::new(),
        camera: None,
        on_found: Some(on_found),
    });
    d.set_open(true);
    start_scan(app);
}

fn start_scan(app: &Rc<App>) {
    let (Some(ui), Some(runtime)) = (app.ui(), app.core.runtime()) else {
        return;
    };
    let d = ui.global::<DiscoverDialog>();
    d.set_scanning(true);
    d.set_status("Scanning...".into());
    d.set_devices(ModelRc::default());
    if let Some(state) = app.discover_dialog.borrow_mut().as_mut() {
        state.devices.clear();
    }
    runtime.spawn(async move {
        let found = rtspcam_onvif::discover(SCAN_FOR).await;
        let _ = slint::invoke_from_event_loop(move || with_app(|app| show_devices(app, found)));
    });
}

fn show_devices(app: &Rc<App>, result: Result<Vec<DiscoveredDevice>, OnvifError>) {
    let Some(ui) = app.ui() else { return };
    let d = ui.global::<DiscoverDialog>();
    if !d.get_open() {
        return;
    }
    d.set_scanning(false);
    match result {
        Ok(devices) => {
            d.set_status(found_text(devices.len()).into());
            let rows: Vec<ModelRc<StandardListViewItem>> = devices
                .iter()
                .map(|dev| {
                    ModelRc::new(VecModel::from(vec![
                        StandardListViewItem::from(dev.title().as_str()),
                        StandardListViewItem::from(dev.host().as_str()),
                    ]))
                })
                .collect();
            let any = !rows.is_empty();
            d.set_devices(ModelRc::new(VecModel::from(rows)));
            if let Some(state) = app.discover_dialog.borrow_mut().as_mut() {
                state.devices = devices;
            }
            if any {
                d.set_device(0);
                device_selected(app, 0);
            }
        }
        Err(e) => d.set_status(format!("Scan failed: {e}").into()),
    }
}

fn device_selected(app: &Rc<App>, row: i32) {
    let Some(ui) = app.ui() else { return };
    let d = ui.global::<DiscoverDialog>();
    let mut state = app.discover_dialog.borrow_mut();
    let Some(state) = state.as_mut() else { return };
    let Some(device) = usize::try_from(row).ok().and_then(|i| state.devices.get(i)) else {
        return;
    };
    if let Some(url) = device.device_url() {
        d.set_address(url.as_str().into());
    }
    // A different camera: forget the streams of the last one.
    state.camera = None;
    d.set_streams(ModelRc::default());
    d.set_add_enabled(false);
    d.set_status("Enter the camera's login and press Get streams.".into());
}

fn start_query(app: &Rc<App>) {
    let (Some(ui), Some(runtime)) = (app.ui(), app.core.runtime()) else {
        return;
    };
    let d = ui.global::<DiscoverDialog>();
    let Some(url) = device_url(&d.get_address()) else {
        d.set_status("Enter the camera's address, or pick one from the list.".into());
        return;
    };
    let user = d.get_username().trim().to_owned();
    let credentials = (!user.is_empty()).then(|| Credentials {
        username: user,
        password: d.get_password().into(),
    });
    d.set_querying(true);
    d.set_add_enabled(false);
    d.set_status("Asking the camera...".into());
    runtime.spawn(async move {
        let info = rtspcam_onvif::query_camera(&url, credentials.as_ref()).await;
        let _ = slint::invoke_from_event_loop(move || with_app(|app| show_streams(app, info)));
    });
}

fn show_streams(app: &Rc<App>, result: Result<CameraInfo, OnvifError>) {
    let Some(ui) = app.ui() else { return };
    let d = ui.global::<DiscoverDialog>();
    if !d.get_open() {
        return;
    }
    d.set_querying(false);
    let mut state = app.discover_dialog.borrow_mut();
    let Some(state) = state.as_mut() else { return };
    match result {
        Ok(info) => {
            let selected_title = usize::try_from(d.get_device())
                .ok()
                .and_then(|i| state.devices.get(i))
                .map(DiscoveredDevice::title);
            let name = info
                .title()
                .or(selected_title)
                .unwrap_or_else(|| "Camera".to_owned());
            // Main streams first is how cameras list them; keep their order.
            let labels: Vec<SharedString> = info
                .profiles
                .iter()
                .map(|p| SharedString::from(p.label()))
                .collect();
            d.set_streams(ModelRc::new(VecModel::from(labels)));
            d.set_stream(0);
            d.set_add_enabled(!info.profiles.is_empty());
            d.set_status(format!("{name}: {} streams. Choose one.", info.profiles.len()).into());
            state.camera = Some((name, info.profiles));
        }
        Err(e) => {
            let hint = e.hint().unwrap_or_default();
            d.set_status(format!("{e}. {hint}").into());
        }
    }
}

fn finish(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let d = ui.global::<DiscoverDialog>();
    let found = {
        let state = app.discover_dialog.borrow();
        let Some(state) = state.as_ref() else { return };
        let Some((name, profiles)) = state.camera.as_ref() else {
            return;
        };
        let Some(profile) = usize::try_from(d.get_stream())
            .ok()
            .and_then(|i| profiles.get(i))
        else {
            return;
        };
        stream_from_onvif(
            name,
            &profile.rtsp_uri,
            &d.get_username(),
            &d.get_password(),
            &state.others,
        )
    };
    match found {
        Ok(stream) => {
            let on_found = app
                .discover_dialog
                .borrow_mut()
                .take()
                .and_then(|mut s| s.on_found.take());
            d.set_open(false);
            if let Some(on_found) = on_found {
                on_found(app, stream);
            }
        }
        Err(e) => d.set_status(format!("The camera's stream address is not usable: {e}").into()),
    }
}

/// Wires the sheet's callbacks. Called once.
pub(crate) fn connect(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let d = ui.global::<DiscoverDialog>();

    let me = Rc::downgrade(app);
    d.on_scan(move || {
        if let Some(app) = me.upgrade() {
            start_scan(&app);
        }
    });
    let me = Rc::downgrade(app);
    d.on_device_selected(move |row| {
        if let Some(app) = me.upgrade() {
            device_selected(&app, row);
        }
    });
    let me = Rc::downgrade(app);
    d.on_query(move || {
        if let Some(app) = me.upgrade() {
            start_query(&app);
        }
    });
    let me = Rc::downgrade(app);
    d.on_add(move || {
        if let Some(app) = me.upgrade() {
            finish(&app);
        }
    });
    let me = Rc::downgrade(app);
    d.on_cancel(move || {
        if let Some(app) = me.upgrade() {
            if let Some(ui) = app.ui() {
                ui.global::<DiscoverDialog>().set_open(false);
            }
            app.discover_dialog.borrow_mut().take();
        }
    });
}
