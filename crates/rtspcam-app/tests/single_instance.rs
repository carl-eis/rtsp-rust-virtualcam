//! A second launch finds the first and asks it to show its window.

use std::sync::mpsc;
use std::time::Duration;

use rtspcam_app::single_instance::{Instance, acquire};

#[test]
fn second_instance_signals_the_first() {
    let Instance::First(mut first) = acquire().expect("acquire") else {
        panic!("another RTSP Cam is running; quit it before running this test");
    };
    let (tx, rx) = mpsc::channel();
    first.on_show(move || {
        let _ = tx.send(());
    });

    assert!(matches!(
        acquire().expect("acquire"),
        Instance::AlreadyRunning
    ));
    rx.recv_timeout(Duration::from_secs(5))
        .expect("the first instance was told to show itself");

    drop(first);
    // Once the first is gone the next launch is the first again.
    assert!(matches!(acquire().expect("acquire"), Instance::First(_)));
}
