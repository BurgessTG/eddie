use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use eddie_core::update::Channel;

use super::State;
use crate::app::Ui;
use crate::testing::{frames, shown, tooltip, window};

/// Serves `update.json` for tagged releases over loopback HTTP, and 404
/// for everything else. Returns the base URL.
fn releases(version: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let body = serde_json::json!({
        "version": version, "build": 1, "commit": "abc1234",
        "notes": format!("{base}/tag/v{version}"),
    })
    .to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let found = request.contains(" /latest/download/update.json ");
            let (status, body) = if found {
                ("200 OK", body.as_str())
            } else {
                ("404 Not Found", "")
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    base
}

/// Frames until `key` shows, or 5 seconds.
fn wait_for(rt: &mut rune::Runtime<Ui>, key: &str) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed().as_secs() < 5 {
        frames(rt, 1);
        if shown(rt, key) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

#[test]
fn a_newer_release_shows_in_the_status_bar_with_what_to_do() {
    let mut ui = Ui::headless();
    ui.updates = State::new(Some(releases("99.0.0")));
    let mut rt = window(ui);
    assert!(wait_for(&mut rt, "update"), "the release is offered");
    let words = tooltip(&mut rt, "update").expect("the control explains itself");
    if eddie_core::update::SELF_INSTALLS {
        assert!(words.contains("signed by its publisher"), "{words}");
    } else {
        assert!(words.contains("package manager"), "{words}");
    }
    assert!(
        words.contains("/tag/v99.0.0"),
        "it names the notes: {words}"
    );

    // Turning updates off takes the offer away.
    rt.state.editor.settings.updates = Channel::Off;
    frames(&mut rt, 3);
    assert!(!shown(&rt, "update"));
}

#[test]
fn an_older_release_or_none_offers_nothing() {
    for base in [releases("0.0.1"), "http://127.0.0.1:9".to_owned()] {
        let mut ui = Ui::headless();
        ui.updates = State::new(Some(base.clone()));
        let mut rt = window(ui);
        let started = std::time::Instant::now();
        // The check has answered once nothing is in flight.
        while rt.state.updates.answers.is_some() && started.elapsed().as_secs() < 5 {
            frames(&mut rt, 1);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        frames(&mut rt, 2);
        assert!(
            rt.state.updates.answers.is_none(),
            "{base}: the check answered"
        );
        assert!(!shown(&rt, "update"), "{base}");
        assert!(
            !rt.state.editor.status.contains("update"),
            "{base}: no word"
        );
    }
}

#[test]
fn a_window_that_does_not_look_never_restarts() {
    let mut state = State::new(None);
    state.relaunch = Some("/Applications/Eddie.app".into());
    assert_eq!(state.take_relaunch(), None);
}
