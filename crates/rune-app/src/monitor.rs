//! The centre of the window: the Program monitor with its transport, or
//! the Gallery. The monitor shows the decoded picture under the playhead, a
//! waveform for audio-only moments, and titles placed as export draws them.

use eddie_core::Action;
use eddie_core::shortcuts;
use openreel_engine::{Clip, ClipKind, format_timecode};
use rune::{Color, Cursor, Cx, El, Phase, Shape, Span, SurfaceFit, col, el, rich, row, text};

use crate::app::Ui;
use crate::gallery;
use crate::icons::{self, Transport};
use crate::theme::{self, c};

/// The transport under the monitor: 4 gap, 14 scrubber, 44 bar, 10 spare.
const TRANSPORT_H: f32 = 68.0;
const BAR_W: f32 = 520.0;
/// Narrower than this, the length readout would run into the controls.
const LENGTH_MIN_W: f32 = 470.0;

pub fn central(ui: &Ui, cx: &mut Cx<Ui>) -> El {
    let c = c();
    let gallery = ui.editor.curator.open;
    let tabs = theme::segmented(
        "central-tabs",
        vec![
            (
                "tab-program".to_owned(),
                "Program",
                "Show the timeline preview and playback controls",
                !gallery,
            ),
            (
                "tab-gallery".to_owned(),
                "Gallery",
                "Pause playback and browse curated clips",
                gallery,
            ),
        ],
        |index, segment| {
            if index == 0 {
                segment.on_press(|ui: &mut Ui| ui.editor.curator.open = false)
            } else {
                segment.on_press(|ui: &mut Ui| ui.editor.open_curator_gallery())
            }
        },
    );
    let panel = col()
        .key("central")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .px(16.0)
        .py(12.0)
        .fill(c.background)
        .child(row().child(tabs))
        .child(el().h(10.0));
    if gallery {
        return panel.child(gallery::view(ui, cx));
    }
    let area = cx.layout_size("monitor-area");
    let mut stage = el().key("monitor-area").w_full().grow(1.0).min_h(0.0);
    if let Some([w, h]) = area {
        stage = stage.child(frame(ui, w, h));
    }
    let bar_w = area.map_or(BAR_W, |[w, _]| w.min(BAR_W));
    panel.child(stage).child(el().h(4.0)).child(
        col()
            .w_full()
            .h(TRANSPORT_H)
            .items_center()
            .child(scrubber(ui, bar_w - 16.0))
            .child(transport_bar(ui, bar_w)),
    )
}

/// The export canvas fitted in the stage, so pillarboxing and titles
/// preview as exported.
fn frame(ui: &Ui, area_w: f32, area_h: f32) -> El {
    let c = c();
    let editor = &ui.editor;
    let canvas = editor.canvas();
    let aspect = canvas.0 as f32 / canvas.1.max(1) as f32;
    let w = (area_w - 24.0).min((area_h - 24.0) * aspect).max(0.0);
    let h = w / aspect;
    let mut frame = el()
        .key("monitor")
        .absolute((area_w - w) / 2.0, (area_h - h) / 2.0)
        .size(w, h)
        .fill(Color::BLACK)
        .radius(theme::R_LG)
        .clip_rounded(theme::R_LG);
    let message = |message: &str| {
        row().absolute(0.0, 0.0).size(w, h).center().child(
            text(message.to_owned())
                .font(theme::sized(14.0))
                .color(c.muted_fg)
                .single_line(),
        )
    };
    let timeline = &editor.timeline;
    let playhead = editor.playhead;
    let picture = ui
        .monitor_size
        .get()
        .filter(|_| editor.preview_is_current());
    match (timeline.active_video(playhead), picture) {
        (Some(clip), Some((pw, ph))) => {
            let scale = (w / pw as f32).min(h / ph as f32);
            let (pw, ph) = (pw as f32 * scale, ph as f32 * scale);
            frame = frame.child(
                el().key("monitor-picture")
                    .absolute((w - pw) / 2.0, (h - ph) / 2.0)
                    .size(pw, ph)
                    .pixels_fit(&ui.monitor, SurfaceFit::Fill)
                    .opacity(clip.opacity.clamp(0.0, 1.0)),
            );
        }
        (Some(_), None) => frame = frame.child(message("Loading frame…")),
        (None, _) => {
            let audio = timeline
                .clips
                .iter()
                .filter(|clip| clip.kind == ClipKind::Audio && clip.contains(playhead))
                .min_by_key(|clip| clip.track);
            let peaks = audio
                .and_then(|clip| clip.source.as_ref())
                .and_then(|source| editor.asset(source))
                .map(|asset| (asset.peaks.as_slice(), asset.info.duration));
            match (audio, peaks) {
                (Some(clip), Some((peaks, duration))) => {
                    frame = frame.child(
                        el().absolute(0.0, 0.0)
                            .size(w, h)
                            .shape(waveform(w, h, clip, peaks, duration, playhead)),
                    );
                }
                _ => {
                    frame = frame.child(message(if !timeline.clips.is_empty() {
                        "No clip at playhead"
                    } else if editor.assets.is_empty() {
                        "Import media to start"
                    } else {
                        "Drag media onto the timeline"
                    }));
                }
            }
        }
    }
    if let Some(title) = timeline.active_title(playhead) {
        frame = frame.child(title_overlay(title, playhead, w, h, canvas.1));
    }
    frame
}

/// An audio-only moment: the clip's waveform with a progress line.
fn waveform(w: f32, h: f32, clip: &Clip, peaks: &[f32], duration: f64, playhead: f64) -> Shape {
    let mut shape = Shape::new().view_box(w, h);
    if peaks.is_empty() || duration <= 0.0 || clip.duration <= 0.0 {
        return shape;
    }
    let bars = rune::color::hex("#4C9A68").expect("a literal colour");
    let peaks_per_sec = peaks.len() as f64 / duration;
    let mid = h / 2.0;
    let max_amp = h * 0.32;
    let (left, right) = (24.0, w - 24.0);
    let (top, bottom) = (30.0, h - 30.0);
    let inner_w = right - left;
    if inner_w <= 0.0 {
        return shape;
    }
    let mut x = left;
    while x < right {
        let frac = (x - left) / inner_w;
        let source_time = clip.source_in + f64::from(frac) * clip.duration;
        if let Some(peak) = peaks.get((source_time * peaks_per_sec) as usize) {
            let amp = (peak * max_amp).max(1.0);
            shape = shape.line(
                [x, mid - amp],
                [x, mid + amp],
                2.0,
                rune::LineCap::Butt,
                bars,
            );
        }
        x += 4.0;
    }
    let progress = ((playhead - clip.start) / clip.duration).clamp(0.0, 1.0) as f32;
    let px = left + inner_w * progress;
    shape.line(
        [px, top],
        [px, bottom],
        1.5,
        rune::LineCap::Butt,
        c().destructive,
    )
}

/// A title or caption from export's own table, scaled from canvas pixels
/// to the monitor so it previews at its exported size and place.
fn title_overlay(title: &Clip, playhead: f64, w: f32, h: f32, canvas_h: u32) -> El {
    let alpha = title.opacity.clamp(0.0, 1.0);
    let look = openreel_media::title_look(title.title_style);
    let scale = h / canvas_h.max(1) as f32;
    let rgb = |(r, g, b): (u8, u8, u8), alpha: f32| {
        rune::color::hex(&format!("#{r:02X}{g:02X}{b:02X}"))
            .expect("a colour from bytes")
            .alpha(alpha)
    };
    let color = rgb(look.color, alpha);
    let font = theme::sized((look.font_size as f32 * scale).max(1.0));
    // Export draws one line with single spaces between words.
    let active = look.highlight.and(title.active_word(playhead));
    let (line, lit) = match (look.highlight, active) {
        (Some(highlight), Some(active)) => {
            let mut line = String::new();
            let mut lit = None;
            for (index, word) in title.name.split_whitespace().enumerate() {
                if index > 0 {
                    line.push(' ');
                }
                if index == active {
                    lit = Some((line.len(), line.len() + word.len(), rgb(highlight, alpha)));
                }
                line.push_str(word);
            }
            (line, lit)
        }
        _ => (title.name.clone(), None),
    };
    let label = |color: Color, lit: Option<(usize, usize, Color)>| {
        let spans = lit
            .map(|(start, end, lit)| vec![Span::new(start, end).color(lit)])
            .unwrap_or_default();
        rich(line.clone(), spans)
            .font(font)
            .color(color)
            .single_line()
    };
    let border = look.border as f32 * scale;
    let mut block = el().key("monitor-title").padding(border);
    match look.box_alpha {
        Some(box_alpha) => block = block.fill(Color::BLACK.alpha(box_alpha * alpha)),
        None => {
            // No box: a black outline from four offset copies.
            let outline = Color::BLACK.alpha(0.8 * alpha);
            for (x, y) in [
                (0.0, border),
                (2.0 * border, border),
                (border, 0.0),
                (border, 2.0 * border),
            ] {
                block = block.child(label(outline, None).absolute(x, y));
            }
        }
    }
    let id = title.id;
    let block = block
        .child(label(color, lit))
        .cursor(Cursor::Text)
        .help("Double-click to edit this text")
        .on_double_press(move |ui: &mut Ui| {
            ui.editor.selected = Some(id);
            ui.show_inspector = true;
            ui.focus_title = true;
        });
    let top = h - look.bottom as f32 * scale - border;
    match look.left {
        Some(left) => block.absolute(left as f32 * scale - border, top),
        None => row().absolute(0.0, top).w(w).justify_center().child(block),
    }
}

/// The thin whole-edit position track above the transport. Click or drag
/// to move the playhead; while held it previews like a ruler scrub.
fn scrubber(ui: &Ui, width: f32) -> El {
    let c = c();
    let width = width.max(0.0);
    let end = ui.editor.timeline.end();
    let active = ui.editor.transport_scrubbing || ui.scrub_hot;
    let fraction = if end > 0.0 {
        (ui.editor.playhead / end).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let track_h = if active { 5.0 } else { 3.0 };
    let x = width * fraction;
    let mut track = el()
        .size(width, 14.0)
        .child(
            el().absolute(0.0, (14.0 - track_h) / 2.0)
                .size(width, track_h)
                .radius(3.0)
                .fill(c.secondary),
        )
        .child(
            el().absolute(0.0, (14.0 - track_h) / 2.0)
                .size(x, track_h)
                .radius(3.0)
                .fill(if active { c.foreground } else { c.muted_fg }),
        );
    if active {
        track = track.child(
            el().absolute(x - 6.0, 1.0)
                .size(12.0, 12.0)
                .radius(6.0)
                .fill(c.foreground),
        );
    }
    let scrub = track
        .key("transport-scrub")
        .cursor(Cursor::Pointer)
        .on_hover_change(|ui: &mut Ui, hovered, _| ui.scrub_hot = hovered)
        .on_drag(|ui: &mut Ui, drag, _| {
            if drag.phase == Phase::Up {
                // Settle at full preview quality where the pointer let go.
                ui.editor.transport_scrubbing = false;
                ui.editor.drop_preview();
                return;
            }
            ui.editor.transport_scrubbing = true;
            let end = ui.editor.timeline.end();
            if end > 0.0 && drag.w > 0.0 {
                let fraction = f64::from((drag.x / drag.w).clamp(0.0, 1.0));
                ui.editor.playhead = ui.editor.timeline.snap(fraction * end);
            }
        });
    if ui.editor.transport_scrubbing {
        // No tooltip over the readout while dragging.
        scrub
    } else {
        scrub.help("Click or drag to move the playhead")
    }
}

/// Timecode in mono 13 with whole leading `00:` groups dimmed; seconds and
/// frames always stay bright.
fn timecode(timecode: &str, bright: Color) -> El {
    let mut dim = 0;
    while timecode[dim..].starts_with("00:") && dim + 3 + 5 <= timecode.len() {
        dim += 3;
    }
    rich(
        timecode.to_owned(),
        vec![Span::new(0, dim).color(c().subtle_fg)],
    )
    .font(theme::mono(13.0))
    .color(bright)
    .single_line()
}

fn playhead_readout(ui: &Ui) -> El {
    let c = c();
    if ui.jump_open {
        return rune::text_input(&ui.jump, theme::mono(13.0))
            .selection_color(theme::selection())
            .key("jump-box")
            .single_line()
            .w(96.0)
            .px(6.0)
            .py(3.0)
            .radius(theme::R_SM)
            .fill(c.input)
            .border(1.0, c.brand)
            .color(c.foreground)
            .help("Type a time and press Enter: 1:05, 3.2s, 120f, 50% or clip:7@end")
            .on_edit(|ui: &mut Ui| &mut ui.jump)
            .on_submit(|ui: &mut Ui, tx| {
                ui.submit_jump();
                tx.blur();
            });
    }
    timecode(
        &format_timecode(ui.editor.playhead, ui.editor.timeline.fps),
        c.foreground,
    )
    .key("playhead-readout")
    .cursor(Cursor::Text)
    .help("Playhead position. Click to jump to a time: 1:05, 3.2s, 120f, 50% or clip:7@end")
    .on_press(|ui: &mut Ui| ui.open_jump())
}

fn transport_button(icon: Transport, label: &str, action: Action) -> El {
    let c = c();
    let primary = matches!(icon, Transport::Play | Transport::Pause);
    let key = format!("transport-{action:?}");
    let help = shortcuts::tip(label, action);
    let button = row()
        .key(key)
        .size(38.0, 32.0)
        .center()
        .cursor(Cursor::Pointer)
        .help(help)
        .on_press(move |ui: &mut Ui| ui.act(action));
    if primary {
        // Play and Pause sit on a filled disc; the others are bare glyphs.
        let hover = rune::color::hex("#E4E4E7").expect("a literal colour");
        let pressed = rune::color::hex("#D4D4D8").expect("a literal colour");
        button.child(
            el().size(30.0, 30.0)
                .radius(15.0)
                .fill(c.primary)
                .hover(hover)
                .press(pressed)
                .child(
                    el().absolute(-4.0, -1.0)
                        .size(38.0, 32.0)
                        .shape(icons::transport(icon, c.primary_fg)),
                ),
        )
    } else {
        button.hover_group().radius(theme::R_MD).child(
            el().size(38.0, 32.0)
                .shape(icons::transport(icon, c.foreground))
                .group_opacity(0.62, 1.0),
        )
    }
}

fn transport_bar(ui: &Ui, bar_w: f32) -> El {
    let c = c();
    let show_length = bar_w >= LENGTH_MIN_W;
    let controls_w = 38.0 * 5.0 + 2.0 * 4.0;
    let play = if ui.editor.playing {
        (Transport::Pause, "Pause")
    } else {
        (Transport::Play, "Play")
    };
    let mut controls = row().gap(2.0).center_y();
    for (icon, label, action) in [
        (Transport::ToStart, "Go to start", Action::GoStart),
        (Transport::StepBack, "Back one frame", Action::StepBack),
        (play.0, play.1, Action::PlayPause),
        (
            Transport::StepForward,
            "Forward one frame",
            Action::StepForward,
        ),
        (Transport::ToEnd, "Go to end", Action::GoEnd),
    ] {
        controls = controls.child(transport_button(icon, label, action));
    }
    let bar = row()
        .key("transport")
        .w(bar_w)
        .h(44.0)
        .px(14.0)
        .center_y()
        .fill(c.surface)
        .border(1.0, c.border)
        .radius(theme::R_LG);
    if !show_length {
        return bar
            .child(row().grow(1.0).min_w(0.0).child(playhead_readout(ui)))
            .child(controls);
    }
    // Equal sides keep the controls centred whatever the readouts measure.
    let side = ((bar_w - 28.0 - controls_w) / 2.0).max(0.0);
    bar.child(row().w(side).center_y().child(playhead_readout(ui)))
        .child(controls)
        .child(
            row().w(side).center_y().justify_end().child(
                timecode(
                    &format_timecode(ui.editor.timeline.end(), ui.editor.timeline.fps),
                    c.muted_fg,
                )
                .help("Timeline length"),
            ),
        )
}
