//! The Add/Edit stream dialog. The form model and its rules are `rtspcam_engine::form`; this
//! only moves values between it and the sheet.

use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rtspcam_core::config::{BRAND_TEMPLATES, Picture, StreamConfig, find_template};
use rtspcam_engine::form::{
    FIT_LABELS, FPS_LABELS, RESOLUTION_LABELS, StreamForm, TRANSPORT_LABELS, describe_errors,
};
use rtspcam_engine::probe::{TEST_TIMEOUT, test_connection};
use rtspcam_pipeline::SourceOptions;
use slint::{ComponentHandle as _, Image, ModelRc, SharedString, Timer, TimerMode, VecModel};

use super::app::{App, with_app};
use super::generated::StreamDialog;
use super::{picture_dialog, preview};

/// Adding a new stream or changing an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Add,
    Edit,
}

/// What happens with the stream when OK is pressed.
pub(crate) type OnDone = Box<dyn FnOnce(&Rc<App>, StreamConfig)>;

pub(crate) struct State {
    /// The stream being edited (or prefilled); keeps the id and fields the form doesn't show.
    base: StreamConfig,
    others: Vec<StreamConfig>,
    picture: Picture,
    on_done: Option<OnDone>,
    /// Counts the seconds of a running connection test.
    test_timer: Option<Timer>,
}

fn strings(items: &[&str]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        items
            .iter()
            .map(|s| SharedString::from(*s))
            .collect::<Vec<_>>(),
    ))
}

/// Shows the dialog for `base`. `others` are the existing streams (for the unique-name rule).
pub(crate) fn open(
    app: &Rc<App>,
    base: StreamConfig,
    mode: Mode,
    others: Vec<StreamConfig>,
    on_done: OnDone,
) {
    let Some(ui) = app.ui() else { return };
    let d = ui.global::<StreamDialog>();
    let form = StreamForm::from_stream(&base);

    d.set_title(
        match mode {
            Mode::Add => "Add stream",
            Mode::Edit => "Edit stream",
        }
        .into(),
    );
    d.set_paste(SharedString::default());
    let mut brands = vec!["Other (enter the path below)".to_owned()];
    brands.extend(BRAND_TEMPLATES.iter().map(|t| t.label()));
    d.set_brands(ModelRc::new(VecModel::from(
        brands
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    )));
    d.set_brand(
        find_template(base.port, &base.path)
            .and_then(|t| BRAND_TEMPLATES.iter().position(|b| b == t))
            .map_or(0, |i| i as i32 + 1),
    );
    d.set_transports(strings(&TRANSPORT_LABELS));
    d.set_resolutions(strings(&RESOLUTION_LABELS));
    d.set_frame_rates(strings(&FPS_LABELS));
    d.set_fits(strings(&FIT_LABELS));
    set_fields(&d, &form);
    d.set_transport(form.transport as i32);
    d.set_resolution(form.resolution as i32);
    d.set_frame_rate(form.fps as i32);
    d.set_fit(form.fit as i32);
    d.set_on_demand(form.on_demand);
    d.set_enabled(form.enabled);
    d.set_show_password(false);
    d.set_error(SharedString::default());
    d.set_testing(false);
    d.set_test_text(SharedString::default());
    d.set_thumbnail(Image::default());
    // A late answer from an earlier dialog's connection test is ignored.
    app.test_generation.fetch_add(1, Ordering::SeqCst);

    *app.stream_dialog.borrow_mut() = Some(State {
        picture: base.picture,
        base,
        others,
        on_done: Some(on_done),
        test_timer: None,
    });
    // Errors for a form nobody has typed into yet would be noise; just keep OK disabled.
    d.set_ok_enabled(validate(app).is_ok());
    d.set_open(true);
}

/// The text fields of `form` into the sheet.
fn set_fields(d: &StreamDialog<'_>, form: &StreamForm) {
    d.set_name(form.name.as_str().into());
    d.set_host(form.host.as_str().into());
    d.set_port(form.port.as_str().into());
    d.set_path(form.path.as_str().into());
    d.set_username(form.username.as_str().into());
    d.set_password(form.password.as_str().into());
}

/// The form as the sheet holds it now.
fn form(app: &App) -> Option<StreamForm> {
    let ui = app.ui()?;
    let d = ui.global::<StreamDialog>();
    let picture = app.stream_dialog.borrow().as_ref()?.picture;
    let index = |i: i32| usize::try_from(i).unwrap_or(0);
    Some(StreamForm {
        name: d.get_name().into(),
        host: d.get_host().into(),
        port: d.get_port().into(),
        path: d.get_path().into(),
        username: d.get_username().into(),
        password: d.get_password().into(),
        transport: index(d.get_transport()),
        resolution: index(d.get_resolution()),
        fps: index(d.get_frame_rate()),
        fit: index(d.get_fit()),
        picture,
        on_demand: d.get_on_demand(),
        enabled: d.get_enabled(),
    })
}

/// The stream the form describes, or the text explaining what is wrong.
fn validate(app: &App) -> Result<StreamConfig, String> {
    let Some(form) = form(app) else {
        return Err(String::new());
    };
    let state = app.stream_dialog.borrow();
    let Some(state) = state.as_ref() else {
        return Err(String::new());
    };
    form.to_stream(&state.base, &state.others)
        .map_err(|errors| describe_errors(&errors))
}

/// Validates and shows the result: OK on or off, and the first problems under the form.
fn revalidate(app: &App) -> Option<StreamConfig> {
    let ui = app.ui()?;
    let d = ui.global::<StreamDialog>();
    match validate(app) {
        Ok(stream) => {
            d.set_error(SharedString::default());
            d.set_ok_enabled(true);
            Some(stream)
        }
        Err(text) => {
            d.set_error(text.into());
            d.set_ok_enabled(false);
            None
        }
    }
}

fn close(app: &App) {
    if let Some(ui) = app.ui() {
        ui.global::<StreamDialog>().set_open(false);
    }
    app.test_generation.fetch_add(1, Ordering::SeqCst);
    app.stream_dialog.borrow_mut().take();
}

/// Wires the sheet's callbacks. Called once.
pub(crate) fn connect(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let d = ui.global::<StreamDialog>();

    let me = Rc::downgrade(app);
    d.on_changed(move || {
        if let Some(app) = me.upgrade() {
            revalidate(&app);
        }
    });

    let me = Rc::downgrade(app);
    d.on_fill(move || {
        let Some(app) = me.upgrade() else { return };
        let (Some(ui), Some(mut form)) = (app.ui(), form(&app)) else {
            return;
        };
        let d = ui.global::<StreamDialog>();
        match form.apply_url(&d.get_paste()) {
            Ok(()) => {
                set_fields(&d, &form);
                d.set_paste(SharedString::default());
                revalidate(&app);
            }
            Err(e) => d.set_error(e.into()),
        }
    });

    let me = Rc::downgrade(app);
    d.on_brand_chosen(move |chosen| {
        let Some(app) = me.upgrade() else { return };
        // Entry 0 is "Other": leave whatever is typed alone.
        let Some(template) = usize::try_from(chosen)
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| BRAND_TEMPLATES.get(i))
        else {
            return;
        };
        let (Some(ui), Some(mut form)) = (app.ui(), form(&app)) else {
            return;
        };
        let d = ui.global::<StreamDialog>();
        form.apply_template(template);
        set_fields(&d, &form);
        revalidate(&app);
        if !template.note.is_empty() {
            d.set_error(template.note.into());
        }
    });

    let me = Rc::downgrade(app);
    d.on_picture(move || {
        let Some(app) = me.upgrade() else { return };
        let Some(current) = app.stream_dialog.borrow().as_ref().map(|s| s.picture) else {
            return;
        };
        picture_dialog::open(
            &app,
            current,
            Box::new(|app, picture| {
                if let Some(state) = app.stream_dialog.borrow_mut().as_mut() {
                    state.picture = picture;
                }
                revalidate(app);
            }),
        );
    });

    let me = Rc::downgrade(app);
    d.on_test(move || {
        if let Some(app) = me.upgrade() {
            start_test(&app);
        }
    });

    let me = Rc::downgrade(app);
    d.on_ok(move || {
        let Some(app) = me.upgrade() else { return };
        let Some(stream) = revalidate(&app) else {
            return;
        };
        let on_done = app
            .stream_dialog
            .borrow_mut()
            .as_mut()
            .and_then(|s| s.on_done.take());
        close(&app);
        if let Some(on_done) = on_done {
            on_done(&app, stream);
        }
    });

    let me = Rc::downgrade(app);
    d.on_cancel(move || {
        if let Some(app) = me.upgrade() {
            close(&app);
        }
    });
}

/// "Test connection": connects once on a worker thread and shows what arrives.
fn start_test(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let d = ui.global::<StreamDialog>();
    let stream = match validate(app) {
        Ok(stream) => stream,
        Err(text) => {
            d.set_error(text.into());
            return;
        }
    };
    let source = match SourceOptions::from_stream(&stream) {
        Ok(source) => source,
        Err(e) => {
            d.set_test_text(e.to_string().into());
            return;
        }
    };
    let Some(runtime) = app.core.runtime() else {
        return;
    };
    d.set_testing(true);
    d.set_test_text("Connecting...".into());
    d.set_thumbnail(Image::default());

    // "Connecting... 3 s" while it runs.
    let timer = Timer::default();
    let started = Instant::now();
    let me = Rc::downgrade(app);
    timer.start(TimerMode::Repeated, Duration::from_millis(200), move || {
        if let Some(ui) = me.upgrade().and_then(|app| app.ui()) {
            let d = ui.global::<StreamDialog>();
            if d.get_testing() {
                let secs = started.elapsed().as_secs();
                d.set_test_text(format!("Connecting... {secs} s").into());
            }
        }
    });
    if let Some(state) = app.stream_dialog.borrow_mut().as_mut() {
        state.test_timer = Some(timer);
    }

    let generation = app.test_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let current = app.test_generation.clone();
    std::thread::spawn(move || {
        let outcome = test_connection(source, &runtime, TEST_TIMEOUT);
        let thumbnail = outcome
            .frame
            .as_deref()
            .and_then(|f| preview::thumbnail(f, (224, 126)));
        let text = outcome.text;
        let _ = slint::invoke_from_event_loop(move || {
            if current.load(Ordering::SeqCst) != generation {
                return; // the dialog was closed or tested again
            }
            with_app(|app| {
                if let Some(state) = app.stream_dialog.borrow_mut().as_mut() {
                    state.test_timer = None;
                }
                let Some(ui) = app.ui() else { return };
                let d = ui.global::<StreamDialog>();
                d.set_testing(false);
                d.set_test_text(text.into());
                d.set_thumbnail(thumbnail.map(Image::from_rgba8).unwrap_or_default());
            });
        });
    });
}
