//! Transitions on the monitor: the clip before plays on past its out point
//! as the program picture, and the clip after comes from a second decoder
//! (or a still while paused) as the incoming picture.

use openreel_engine::{Transition, TransitionKind};

use super::*;

/// Red is source [0, 1), blue [1, 2). Both clips show the red second, the
/// second one straight after the first, with a 0.4 s mix between them
/// (0.8 to 1.2). Past the cut the program picture is the first clip played
/// on into the file's blue; the incoming picture is red throughout.
fn mixed(app: &mut Editor, path: &str) -> ClipId {
    app.timeline
        .add_media_clip("Out", ClipKind::Video, 0, 0.0, 1.0, 0.0, path);
    let next = app
        .timeline
        .add_media_clip("In", ClipKind::Video, 0, 1.0, 1.0, 0.0, path);
    app.timeline.clip_mut(next).unwrap().transition =
        Some(Transition::new(TransitionKind::Mix, 0.4));
    next
}

fn red(picture: Rgb) -> bool {
    picture.r() > 240 && picture.b() < 10
}

fn blue(picture: Rgb) -> bool {
    picture.b() > 240 && picture.r() < 10
}

#[test]
fn paused_in_a_transition_the_monitor_holds_both_pictures() {
    let fixture = Fixture::new();
    let (mut app, monitor) = watched_editor();
    add_asset(&mut app, &fixture.path);
    let next = mixed(&mut app, &fixture.path);
    app.seek(1.1);
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.transition_shown().is_none() {
        assert!(Instant::now() < deadline, "no transition: {}", app.status);
        app.poll_frames();
        app.sync_playback();
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(app.transition_shown().unwrap().to, next);
    let program = monitor.take();
    let incoming = monitor.take_incoming();
    assert!(blue(*program.last().unwrap()), "{program:?}");
    assert!(red(*incoming.last().unwrap()), "{incoming:?}");

    // Past the transition the next clip is the program picture alone.
    app.seek(1.3);
    let after = await_picture(&mut app, &monitor);
    assert!(red(after), "{after:?}");
    assert!(app.transition_shown().is_none());

    // The clip before now plays the file's last second, blue, to its end:
    // past it the transition holds the file's last picture.
    let out = app.timeline.clips[0].id;
    app.timeline.clip_mut(out).unwrap().source_in = 1.0;
    app.seek(1.15);
    monitor.take();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut held = false;
    while app.transition_shown().is_none() || !held {
        assert!(Instant::now() < deadline, "no held picture: {}", app.status);
        app.poll_frames();
        app.sync_playback();
        held |= monitor.take().last().copied().is_some_and(blue);
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn playing_through_a_transition_shows_both_clips_and_the_next_takes_over_after_it() {
    let fixture = Fixture::new();
    let (mut app, monitor) = watched_editor();
    add_asset(&mut app, &fixture.path);
    let next = mixed(&mut app, &fixture.path);
    app.seek(0.5);
    app.last_now = Some(0.0);
    app.set_playing(true);
    let mut now = 0.0;
    let (mut blue_past_the_cut, mut incoming_seen) = (false, false);
    while app.playing && app.playhead < 1.5 {
        // Wait for both pictures, then give the decoders a moment to show
        // the one for this time: the clock is held only for first pictures.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut settled = None;
        loop {
            app.poll_frames();
            app.sync_playback();
            if app.waiting_for_video() {
                assert!(
                    Instant::now() < deadline,
                    "stuck at {}: {}",
                    app.playhead,
                    app.status
                );
            } else if settled.get_or_insert_with(Instant::now).elapsed() > Duration::from_millis(80)
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let at = app.playhead;
        let program = monitor.take();
        let incoming = monitor.take_incoming();
        if (0.8..1.2).contains(&at) {
            let span = app.transition_shown();
            assert_eq!(span.map(|span| span.to), Some(next), "at {at}");
        } else {
            assert!(app.transition_shown().is_none(), "at {at}");
        }
        if (1.05..1.2).contains(&at) && program.last().copied().is_some_and(blue) {
            blue_past_the_cut = true;
        }
        if at >= 1.25 {
            assert!(program.iter().all(|p| red(*p)), "at {at}: {program:?}");
        }
        assert!(incoming.iter().all(|p| red(*p)), "at {at}: {incoming:?}");
        incoming_seen |= !incoming.is_empty();
        now += 0.05;
        app.advance_playback(now);
    }
    assert!(app.playhead >= 1.5, "stopped at {}", app.playhead);
    assert!(
        blue_past_the_cut,
        "the clip before plays on past its out point"
    );
    assert!(incoming_seen, "the next clip's pictures arrived");
    app.set_playing(false);
}
