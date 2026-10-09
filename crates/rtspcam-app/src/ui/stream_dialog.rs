//! The Add/Edit stream dialog.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rtspcam_core::config::{Field, StreamConfig};
use rtspcam_pipeline::{Frame, Pipeline, PipelineOptions, SourceOptions, StreamState};
use tokio::runtime::Handle;
use winsafe::{self as w, co, gui, prelude::*};

use super::preview::PreviewPane;
use crate::form::{
    FIT_LABELS, FPS_LABELS, FormError, RESOLUTION_LABELS, StreamForm, TRANSPORT_LABELS,
};

const LABEL_W: i32 = 120;
const FIELD_X: i32 = 136;
const FIELD_W: i32 = 300;
const ROW: i32 = 28;
const TIMER_ID: usize = 1;
/// How long "Test connection" waits for a first picture.
const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// What a connection test found.
#[derive(Default)]
struct TestOutcome {
    done: bool,
    text: String,
    frame: Option<Arc<Frame>>,
}

#[derive(Clone)]
pub(crate) struct StreamDialog {
    wnd: gui::WindowModal,
    paste: gui::Edit,
    paste_btn: gui::Button,
    name: gui::Edit,
    host: gui::Edit,
    port: gui::Edit,
    path: gui::Edit,
    user: gui::Edit,
    pass: gui::Edit,
    show_pass: gui::CheckBox,
    transport: gui::ComboBox,
    resolution: gui::ComboBox,
    fps: gui::ComboBox,
    fit: gui::ComboBox,
    on_demand: gui::CheckBox,
    enabled: gui::CheckBox,
    error: gui::Label,
    test_btn: gui::Button,
    test_text: gui::Label,
    thumbnail: PreviewPane,
    ok: gui::Button,
    cancel: gui::Button,

    base: Rc<StreamConfig>,
    others: Rc<Vec<StreamConfig>>,
    runtime: Handle,
    test: Arc<Mutex<Option<TestOutcome>>>,
    test_started: Rc<RefCell<Option<Instant>>>,
    result: Rc<RefCell<Option<StreamConfig>>>,
}

fn label(parent: &gui::WindowModal, text: &str, y: i32) -> gui::Label {
    gui::Label::new(
        parent,
        gui::LabelOpts {
            text,
            position: (12, y + 4),
            size: (LABEL_W, 20),
            ..Default::default()
        },
    )
}

fn edit(parent: &gui::WindowModal, y: i32, text: &str, style: co::ES) -> gui::Edit {
    gui::Edit::new(
        parent,
        gui::EditOpts {
            text,
            position: (FIELD_X, y),
            width: FIELD_W,
            height: 22,
            control_style: co::ES::AUTOHSCROLL | style,
            ..Default::default()
        },
    )
}

fn combo(parent: &gui::WindowModal, y: i32, items: &[&str], selected: usize) -> gui::ComboBox {
    gui::ComboBox::new(
        parent,
        gui::ComboBoxOpts {
            position: (FIELD_X, y),
            width: 160,
            items,
            selected_item: Some(selected as u32),
            ..Default::default()
        },
    )
}

impl StreamDialog {
    fn error_text(&self, text: &str) {
        let _ = self.error.hwnd().SetWindowText(text);
    }

    fn test_label(&self, text: &str) {
        let _ = self.test_text.hwnd().SetWindowText(text);
    }

    /// Shows the dialog over `parent`. `existing` edits that stream; `None` adds a new one.
    /// Returns the stream to save, or `None` if cancelled.
    pub(crate) fn run(
        parent: &impl GuiParent,
        existing: Option<&StreamConfig>,
        others: Vec<StreamConfig>,
        runtime: Handle,
    ) -> Option<StreamConfig> {
        let base = existing.cloned().unwrap_or_default();
        let form = StreamForm::from_stream(&base);
        let title = if existing.is_some() {
            "Edit stream"
        } else {
            "Add stream"
        };
        let wnd = gui::WindowModal::new(gui::WindowModalOpts {
            title,
            size: (456, 600),
            ..Default::default()
        });

        let mut y = 12;
        label(&wnd, "Paste full URL", y);
        let paste = gui::Edit::new(
            &wnd,
            gui::EditOpts {
                text: "",
                position: (FIELD_X, y),
                width: FIELD_W - 64,
                height: 22,
                ..Default::default()
            },
        );
        let paste_btn = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "Fill",
                position: (FIELD_X + FIELD_W - 58, y - 2),
                width: 58,
                height: 26,
                ..Default::default()
            },
        );
        y += ROW + 8;
        label(&wnd, "Name", y);
        let name = edit(&wnd, y, &form.name, co::ES::LEFT);
        y += ROW;
        label(&wnd, "Protocol", y);
        let _protocol = gui::ComboBox::new(
            &wnd,
            gui::ComboBoxOpts {
                position: (FIELD_X, y),
                width: 160,
                items: &["RTSP"],
                selected_item: Some(0),
                // Only one protocol for now; the control keeps the layout stable for later.
                window_style: co::WS::CHILD | co::WS::VISIBLE | co::WS::DISABLED,
                ..Default::default()
            },
        );
        y += ROW;
        label(&wnd, "IP address / host", y);
        let host = edit(&wnd, y, &form.host, co::ES::LEFT);
        y += ROW;
        label(&wnd, "Port", y);
        let port = gui::Edit::new(
            &wnd,
            gui::EditOpts {
                text: &form.port,
                position: (FIELD_X, y),
                width: 80,
                height: 22,
                control_style: co::ES::AUTOHSCROLL | co::ES::NUMBER,
                ..Default::default()
            },
        );
        y += ROW;
        label(&wnd, "Path", y);
        let path = edit(&wnd, y, &form.path, co::ES::LEFT);
        y += ROW;
        label(&wnd, "User name", y);
        let user = edit(&wnd, y, &form.username, co::ES::LEFT);
        y += ROW;
        label(&wnd, "Password", y);
        let pass = edit(&wnd, y, &form.password, co::ES::PASSWORD);
        y += ROW;
        let show_pass = gui::CheckBox::new(
            &wnd,
            gui::CheckBoxOpts {
                text: "Show password",
                position: (FIELD_X, y),
                size: (200, 20),
                ..Default::default()
            },
        );
        y += ROW;
        label(&wnd, "Transport", y);
        let transport = combo(&wnd, y, &TRANSPORT_LABELS, form.transport);
        y += ROW;
        label(&wnd, "Resolution", y);
        let resolution = combo(&wnd, y, &RESOLUTION_LABELS, form.resolution);
        y += ROW;
        label(&wnd, "Frame rate", y);
        let fps = combo(&wnd, y, &FPS_LABELS, form.fps);
        y += ROW;
        label(&wnd, "Fit", y);
        let fit = combo(&wnd, y, &FIT_LABELS, form.fit);
        y += ROW;
        let on_demand = gui::CheckBox::new(
            &wnd,
            gui::CheckBoxOpts {
                text: "On demand (connect only when used)",
                position: (FIELD_X, y),
                size: (300, 20),
                check_state: if form.on_demand {
                    co::BST::CHECKED
                } else {
                    co::BST::UNCHECKED
                },
                ..Default::default()
            },
        );
        y += ROW - 4;
        let enabled = gui::CheckBox::new(
            &wnd,
            gui::CheckBoxOpts {
                text: "Enabled",
                position: (FIELD_X, y),
                size: (200, 20),
                check_state: if form.enabled {
                    co::BST::CHECKED
                } else {
                    co::BST::UNCHECKED
                },
                ..Default::default()
            },
        );
        y += ROW + 4;
        let test_btn = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "Test connection",
                position: (12, y),
                width: 110,
                height: 26,
                ..Default::default()
            },
        );
        let test_text = gui::Label::new(
            &wnd,
            gui::LabelOpts {
                text: "",
                position: (130, y + 2),
                size: (190, 80),
                ..Default::default()
            },
        );
        let thumbnail = PreviewPane::new(&wnd, (326, y), (112, 63));
        y += 76;
        let error = gui::Label::new(
            &wnd,
            gui::LabelOpts {
                text: "",
                position: (12, y),
                size: (426, 36),
                ..Default::default()
            },
        );
        y += 40;
        let ok = gui::Button::new(
            &wnd,
            gui::ButtonOpts {
                text: "OK",
                position: (270, y),
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
                position: (358, y),
                width: 80,
                height: 26,
                ..Default::default()
            },
        );

        let dlg = Self {
            wnd,
            paste,
            paste_btn,
            name,
            host,
            port,
            path,
            user,
            pass,
            show_pass,
            transport,
            resolution,
            fps,
            fit,
            on_demand,
            enabled,
            error,
            test_btn,
            test_text,
            thumbnail,
            ok,
            cancel,
            base: Rc::new(base),
            others: Rc::new(others),
            runtime,
            test: Arc::default(),
            test_started: Rc::default(),
            result: Rc::default(),
        };
        dlg.events();
        if dlg.wnd.show_modal(parent).is_err() {
            return None;
        }
        dlg.result.take()
    }

    /// The form as the controls hold it now.
    fn form(&self) -> StreamForm {
        let index = |c: &gui::ComboBox| c.items().selected_index().unwrap_or(0) as usize;
        StreamForm {
            name: txt(&self.name),
            host: txt(&self.host),
            port: txt(&self.port),
            path: txt(&self.path),
            username: txt(&self.user),
            password: txt(&self.pass),
            transport: index(&self.transport),
            resolution: index(&self.resolution),
            fps: index(&self.fps),
            fit: index(&self.fit),
            on_demand: self.on_demand.is_checked(),
            enabled: self.enabled.is_checked(),
        }
    }

    fn set_form(&self, f: &StreamForm) {
        let _ = self.name.set_text(&f.name);
        let _ = self.host.set_text(&f.host);
        let _ = self.port.set_text(&f.port);
        let _ = self.path.set_text(&f.path);
        let _ = self.user.set_text(&f.username);
        let _ = self.pass.set_text(&f.password);
    }

    /// Validates the form: enables OK and shows the first problem next to the fields.
    fn revalidate(&self) -> Option<StreamConfig> {
        match self.form().to_stream(&self.base, &self.others) {
            Ok(stream) => {
                self.error_text("");
                self.ok.hwnd().EnableWindow(true);
                Some(stream)
            }
            Err(errors) => {
                self.error_text(&describe(&errors));
                self.ok.hwnd().EnableWindow(false);
                None
            }
        }
    }

    fn events(&self) {
        let me = self.clone();
        self.wnd.on().wm_init_dialog(move |_| {
            // Validation messages for an empty form would be noise; just keep OK disabled.
            me.ok.hwnd().EnableWindow(false);
            let _ = me.wnd.hwnd().SetTimer(TIMER_ID, 200, None);
            me.revalidate_quiet();
            Ok(true)
        });

        for e in [
            &self.name, &self.host, &self.port, &self.path, &self.user, &self.pass,
        ] {
            let me = self.clone();
            e.on().en_change(move || {
                me.revalidate_loud();
                Ok(())
            });
        }
        for c in [&self.transport, &self.resolution, &self.fps, &self.fit] {
            let me = self.clone();
            c.on().cbn_sel_change(move || {
                me.revalidate_loud();
                Ok(())
            });
        }
        for c in [&self.on_demand, &self.enabled] {
            let me = self.clone();
            c.on().bn_clicked(move || {
                me.revalidate_loud();
                Ok(())
            });
        }

        let me = self.clone();
        self.show_pass.on().bn_clicked(move || {
            // EM_SETPASSWORDCHAR: 0 shows the text, '*' hides it.
            let ch = if me.show_pass.is_checked() {
                0
            } else {
                '*' as usize
            };
            // SAFETY: EM_SETPASSWORDCHAR takes a character in wParam and ignores lParam.
            unsafe {
                me.pass.hwnd().SendMessage(w::msg::Wm {
                    msg_id: co::WM::from_raw(0x00CC),
                    wparam: ch,
                    lparam: 0,
                });
            }
            me.pass.hwnd().InvalidateRect(None, true).ok();
            Ok(())
        });

        let me = self.clone();
        self.paste_btn.on().bn_clicked(move || {
            let mut form = me.form();
            match form.apply_url(&txt(&me.paste)) {
                Ok(()) => {
                    me.set_form(&form);
                    let _ = me.paste.set_text("");
                    me.revalidate_loud();
                }
                Err(e) => me.error_text(&e),
            }
            Ok(())
        });

        let me = self.clone();
        self.test_btn.on().bn_clicked(move || {
            me.start_test();
            Ok(())
        });

        let me = self.clone();
        self.wnd.on().wm_timer(TIMER_ID, move || {
            me.poll_test();
            Ok(())
        });

        let me = self.clone();
        self.ok.on().bn_clicked(move || {
            if let Some(stream) = me.revalidate() {
                *me.result.borrow_mut() = Some(stream);
                me.wnd.close();
            }
            Ok(())
        });

        let me = self.clone();
        self.cancel.on().bn_clicked(move || {
            me.wnd.close();
            Ok(())
        });
    }

    /// Enables or disables OK without showing errors (the empty form of a new stream).
    fn revalidate_quiet(&self) {
        let ok = self.form().to_stream(&self.base, &self.others).is_ok();
        self.ok.hwnd().EnableWindow(ok);
    }

    fn revalidate_loud(&self) {
        self.revalidate();
    }

    fn start_test(&self) {
        let stream = match self.form().to_stream(&self.base, &self.others) {
            Ok(s) => s,
            Err(errors) => {
                self.error_text(&describe(&errors));
                return;
            }
        };
        let source = match SourceOptions::from_stream(&stream) {
            Ok(s) => s,
            Err(e) => {
                self.test_label(&e.to_string());
                return;
            }
        };
        self.test_btn.hwnd().EnableWindow(false);
        self.test_label("Connecting...");
        self.thumbnail.show(None, "");
        *self.test_started.borrow_mut() = Some(Instant::now());
        *self.test.lock().unwrap_or_else(PoisonError::into_inner) = None;

        let outcome = self.test.clone();
        let runtime = self.runtime.clone();
        std::thread::spawn(move || {
            let result = run_test(source, &runtime);
            *outcome.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
        });
    }

    fn poll_test(&self) {
        let taken = self
            .test
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(outcome) = taken else {
            if let Some(started) = *self.test_started.borrow() {
                let secs = started.elapsed().as_secs();
                self.test_label(&format!("Connecting... {secs} s"));
            }
            return;
        };
        if outcome.done {
            *self.test_started.borrow_mut() = None;
            self.test_btn.hwnd().EnableWindow(true);
        }
        self.test_label(&outcome.text);
        self.thumbnail.show(outcome.frame.as_ref(), "");
    }
}

/// Connects once and reports what the camera sends, or why it could not.
fn run_test(source: SourceOptions, runtime: &Handle) -> TestOutcome {
    let pipeline = Pipeline::start("test connection", PipelineOptions::new(source), runtime);
    let status = pipeline.status();
    let started = Instant::now();
    let outcome = loop {
        let state = status.borrow().clone();
        match state {
            StreamState::Streaming(stats) => {
                if let Some(frame) = pipeline.frames().latest() {
                    break TestOutcome {
                        done: true,
                        text: format!(
                            "Connected.\n{} {}x{}, {:.0} fps\n({})",
                            stats.codec, stats.width, stats.height, stats.fps, stats.decoder
                        ),
                        frame: Some(frame),
                    };
                }
            }
            StreamState::Retrying { error, .. } => {
                let hint = error.kind().hint().unwrap_or_default();
                break TestOutcome {
                    done: true,
                    text: format!("Failed: {error}\n{hint}"),
                    frame: None,
                };
            }
            _ => {}
        }
        if started.elapsed() > TEST_TIMEOUT {
            break TestOutcome {
                done: true,
                text: "No picture arrived in time. Check the address, or whether the camera \
                       sends key frames rarely."
                    .to_owned(),
                frame: None,
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    pipeline.stop();
    outcome
}

fn describe(errors: &[FormError]) -> String {
    errors
        .iter()
        .map(|e| format!("{}: {}", field_name(e.field), e.message))
        .take(2)
        .collect::<Vec<_>>()
        .join("\n")
}

fn field_name(f: Field) -> &'static str {
    match f {
        Field::Name => "Name",
        Field::Host => "Address",
        Field::Port => "Port",
        Field::Path => "Path",
        Field::Password => "Password",
        Field::Protocol => "Protocol",
        Field::Output => "Output",
        Field::Id => "Id",
    }
}

fn txt(edit: &gui::Edit) -> String {
    edit.text().unwrap_or_default()
}
