//! Messages and yes/no questions, shown as a sheet over the window.

use std::rc::Rc;

use slint::ComponentHandle as _;

use super::app::App;
use super::generated::MessageDialog;

/// Shows `text` with an OK button; `then` runs when it is closed.
pub(crate) fn inform(app: &Rc<App>, title: &str, text: &str) {
    show(app, title, text, false, |_| {});
}

/// Asks a yes/no question; `then` gets the answer (Escape counts as no).
pub(crate) fn ask(app: &Rc<App>, title: &str, text: &str, then: impl FnOnce(bool) + 'static) {
    show(app, title, text, true, then);
}

fn show(app: &Rc<App>, title: &str, text: &str, question: bool, then: impl FnOnce(bool) + 'static) {
    let Some(ui) = app.ui() else { return };
    let dialog = ui.global::<MessageDialog>();
    dialog.set_title(title.into());
    dialog.set_text(text.into());
    dialog.set_question(question);
    *app.message_reply.borrow_mut() = Some(Box::new(then));
    dialog.set_open(true);
}

/// Wires the sheet's buttons. Called once.
pub(crate) fn connect(app: &Rc<App>) {
    let Some(ui) = app.ui() else { return };
    let me = Rc::downgrade(app);
    ui.global::<MessageDialog>().on_answer(move |yes| {
        let Some(app) = me.upgrade() else { return };
        if let Some(ui) = app.ui() {
            ui.global::<MessageDialog>().set_open(false);
        }
        let reply = app.message_reply.borrow_mut().take();
        if let Some(reply) = reply {
            reply(yes);
        }
    });
}
