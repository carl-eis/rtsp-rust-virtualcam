//! A second launch finds the first and asks it to show its window.

use std::sync::mpsc;
use std::time::Duration;

use rtspcam_platform::{Instance, single_instance};

#[test]
fn second_instance_signals_the_first() {
    // Not the app's name, so a running RTSP Cam doesn't get in the way.
    let name = format!("RtspCamTest{}", std::process::id());
    let check = single_instance(&name);
    let Instance::First(mut first) = check.acquire().expect("acquire") else {
        panic!("the test name is already taken");
    };
    let (tx, rx) = mpsc::channel();
    first.on_show(Box::new(move || {
        let _ = tx.send(());
    }));

    assert!(matches!(
        check.acquire().expect("acquire"),
        Instance::AlreadyRunning
    ));
    rx.recv_timeout(Duration::from_secs(5))
        .expect("the first instance was told to show itself");

    drop(first);
    // Once the first is gone the next launch is the first again.
    assert!(matches!(
        check.acquire().expect("acquire"),
        Instance::First(_)
    ));
}
