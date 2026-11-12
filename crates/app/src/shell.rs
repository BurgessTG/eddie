//! The bars around the editor: the menu bar on top, the status bar below.

use std::path::Path;

use eddie_core::Action;
use eddie_core::shortcuts;
use openreel_engine::format_timecode;
use rune::{Color, Cursor, Cx, El, Titlebar, el, row, spacer, text};

use crate::app::Ui;
use crate::icons::{self, Tool};
use crate::menus::Menu;
use crate::theme::{self, Variant, c};

/// The menu bar's height: 30-point controls inside 8 points of padding.
pub const BAR_H: f32 = 46.0;

/// The menu bar as the window's titlebar, where the system lets an app draw
/// one: macOS puts its traffic lights on the bar's 28-point line.
pub fn titlebar() -> Titlebar {
    Titlebar::new(BAR_H).traffic_lights(16.0, 15.0)
}

pub fn menu_bar(ui: &Ui, cx: &Cx<Ui>) -> El {
    let c = c();
    // Where the window's own controls sit over the bar (the macOS traffic
    // lights; nothing on Linux or in full screen), the bar starts past them.
    let chrome = cx.chrome_insets();
    let pad = |inset: f32| if inset > 0.0 { inset + 12.0 } else { 14.0 };
    // The mark, the name and the menus share a 28-point line at the top of
    // the bar; the 30-point settings button beside Export sets its height.
    let mut left = row()
        .key("menu-bar-left")
        .h(28.0)
        .gap(8.0)
        .center_y()
        .self_start()
        .child(theme::mark(22.0))
        .child(
            text("Eddie")
                .font(theme::semibold(15.0))
                .color(c.foreground)
                .single_line(),
        )
        .child(el().w(2.0));
    for menu in Menu::ALL {
        let open = ui.menu == Some(menu);
        left = left.child(
            row()
                .key(menu.anchor())
                .px(12.0)
                .py(5.5)
                .center()
                .radius(theme::R_MD)
                .fill(if open {
                    c.pressed
                } else {
                    Color::WHITE.alpha(10.0 / 255.0)
                })
                .hover(c.hover)
                .press(c.pressed)
                .cursor(Cursor::Pointer)
                .help(menu.help())
                .child(
                    text(menu.label())
                        .font(theme::BODY)
                        .color(c.foreground)
                        .single_line(),
                )
                .on_press(move |ui: &mut Ui| {
                    ui.menu = if ui.menu == Some(menu) {
                        None
                    } else {
                        Some(menu)
                    };
                }),
        );
    }
    let bar = row()
        .key("menu-bar")
        .w_full()
        .pl(pad(chrome.leading))
        .pr(pad(chrome.trailing))
        .py(8.0)
        .gap(8.0)
        .center_y()
        .fill(c.surface)
        .border_sides([0.0, 0.0, 1.0, 0.0], c.border)
        .child(left)
        .child(spacer())
        .child(
            theme::icon_button(
                "open-settings",
                icons::tool(Tool::Settings, c.foreground),
                &shortcuts::tip(
                    "Settings: playback, speech and format",
                    Action::OpenSettings,
                ),
                ui.show_settings,
                true,
            )
            .on_press(|ui: &mut Ui| ui.act(Action::OpenSettings)),
        )
        .child(
            theme::button(
                "export",
                "Export",
                Variant::Primary,
                &shortcuts::tip("Export the timeline as an MP4", Action::Export),
                true,
            )
            .h(27.0)
            .mb(1.0)
            .on_press(|ui: &mut Ui| ui.act(Action::Export)),
        );
    // As the titlebar, its empty parts move the window.
    if chrome.height > 0.0 {
        bar.window_drag()
    } else {
        bar
    }
}

/// Green when playback shows at least 80% of the pictures it should, amber
/// from 50%, red below.
fn rate_health(shown: f64, target: f64) -> Color {
    let c = c();
    if shown >= target * 0.8 {
        c.success
    } else if shown >= target * 0.5 {
        c.warning
    } else {
        c.destructive
    }
}

/// Open `dir` in the desktop's file manager without blocking the UI.
fn open_folder(dir: &Path) -> std::io::Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let mut child = std::process::Command::new(opener).arg(dir).spawn()?;
    // Reap the launcher so it does not linger as a zombie.
    std::thread::spawn(move || child.wait());
    Ok(())
}

fn dot(radius: f32, color: Color) -> El {
    el().size(radius * 2.0, radius * 2.0)
        .radius(radius)
        .fill(color)
}

pub fn status_bar(ui: &Ui) -> El {
    let c = c();
    let editor = &ui.editor;
    let just_exported = editor
        .exported
        .as_ref()
        .filter(|(_, message)| *message == editor.status);
    // The message truncates into what the readouts leave, so a long error
    // never runs under them.
    let status = text(editor.status.clone())
        .key("status")
        .font(theme::sized(12.0))
        .color(if just_exported.is_some() {
            c.foreground
        } else {
            c.muted_fg
        })
        .single_line()
        .truncate()
        .grow(1.0)
        .min_w(0.0)
        .help(editor.status.clone());
    let mut bar = row()
        .key("status-bar")
        .w_full()
        .px(14.0)
        .py(5.0)
        .gap(8.0)
        .min_h(38.0)
        .center_y()
        .fill(c.surface)
        .border_sides([1.0, 0.0, 0.0, 0.0], c.border)
        .child(lamp(ui))
        .child(status)
        .child(el().w(4.0));
    if let Some(progress) = editor.export_progress {
        return bar
            .child(
                text("Exporting")
                    .font(theme::sized(12.0))
                    .color(c.muted_fg)
                    .single_line(),
            )
            .child(export_progress(progress.clamp(0.0, 1.0) as f32));
    }
    // Left to right: rate, length, UI frame time, memory.
    let segments = [
        (
            format!("{:.0} fps", editor.timeline.fps),
            "Project frame rate",
        ),
        (
            format_timecode(editor.timeline.end(), editor.timeline.fps),
            "Timeline length",
        ),
        (format!("{:.1} ms", ui.frame_ms), "UI frame time"),
        (format!("{} MB", ui.rss_mb), "Memory in use"),
    ];
    for (index, (value, tip)) in segments.into_iter().enumerate() {
        if index > 0 {
            bar = bar.child(
                text("·")
                    .font(theme::sized(11.0))
                    .color(c.subtle_fg)
                    .single_line(),
            );
        }
        bar = bar.child(
            text(value)
                .font(theme::mono(11.0))
                .color(c.subtle_fg)
                .single_line()
                .help(tip),
        );
    }
    if let Some((path, _)) = just_exported {
        bar = bar.child(el().w(4.0)).child(open_folder_button(path));
    }
    bar
}

/// A spinner while the editor works, the playback-rate dot while it
/// plays, a grey dot otherwise.
fn lamp(ui: &Ui) -> El {
    let c = c();
    let editor = &ui.editor;
    let busy = editor.importing() || editor.exporting() || editor.curating();
    let lamp = row().key("status-lamp").size(12.0, 12.0).center();
    if busy {
        lamp.child(
            el().size(14.0, 14.0)
                .shape(icons::spinner(14.0, ui.turn(), c.muted_fg)),
        )
        .help("Working")
    } else if let Some((shown, target)) = editor.playback_rate() {
        lamp.child(dot(4.0, rate_health(shown as f64, target)))
            .help(format!(
                "Playback: {shown} of {target:.0} pictures per second"
            ))
    } else {
        lamp.child(dot(3.0, c.subtle_fg))
    }
}

/// The export's progress bar, `progress` from 0 to 1.
fn export_progress(progress: f32) -> El {
    let c = c();
    row()
        .key("export-progress")
        .size(180.0, 16.0)
        .radius(8.0)
        .fill(c.input)
        .clip_rounded(8.0)
        .help("Export progress")
        .child(
            el().absolute(0.0, 0.0)
                .size((180.0 * progress).max(16.0), 16.0)
                .radius(8.0)
                .fill(c.brand),
        )
        .child(
            row().size(180.0, 16.0).center().child(
                text(format!("{:.0}%", progress * 100.0))
                    .font(theme::sized(11.0))
                    .color(c.foreground)
                    .single_line(),
            ),
        )
}

/// Shows the folder of the file an export just wrote.
fn open_folder_button(path: &Path) -> El {
    let dir = path.parent().map(Path::to_path_buf);
    theme::button(
        "open-export-folder",
        "Open folder",
        Variant::Secondary,
        &format!("Show the exported file's folder\n{}", path.display()),
        true,
    )
    .on_press(move |ui: &mut Ui| {
        let Some(dir) = &dir else {
            return;
        };
        if let Err(error) = open_folder(dir) {
            ui.editor.status = format!("Could not open the folder: {error}");
        }
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::app::Ui;
    use crate::testing::{frames, shown, tooltip, window};

    #[test]
    fn a_finished_export_offers_its_folder_until_the_status_moves_on() {
        let mut ui = Ui::headless();
        ui.editor.status = "Exported montage.mp4".to_owned();
        ui.editor.exported = Some((
            PathBuf::from("/videos/cut/montage.mp4"),
            ui.editor.status.clone(),
        ));
        let mut rt = window(ui);
        assert!(shown(&rt, "open-export-folder"));
        assert!(
            tooltip(&mut rt, "open-export-folder")
                .is_some_and(|words| words.contains("/videos/cut/montage.mp4")),
            "the button names the file it opens the folder of"
        );

        rt.state.editor.status = "Split clip.".to_owned();
        frames(&mut rt, 2);
        assert!(
            !shown(&rt, "open-export-folder"),
            "stale exports offer nothing"
        );
    }

    /// macOS draws its traffic lights over the bar's left end.
    #[test]
    fn the_bar_starts_past_the_windows_own_controls() {
        let mut rt = window(Ui::headless());
        let bar = |rt: &rune::Runtime<Ui>| rt.presented("menu-bar").unwrap().0;
        let left = |rt: &rune::Runtime<Ui>| rt.presented("menu-bar-left").unwrap().0[0];
        assert_eq!(bar(&rt)[3], super::BAR_H, "the titlebar is the bar");
        assert_eq!(left(&rt), 14.0);

        let mut lights = rune::ChromeInsets::default();
        lights.leading = 76.0;
        lights.height = super::BAR_H;
        rt.set_chrome_insets(lights);
        // The bar slides to its new place for about half a second.
        frames(&mut rt, 60);
        assert!(left(&rt) >= 84.0, "the mark is under the traffic lights");
        assert_eq!(bar(&rt)[3], super::BAR_H);

        // Full screen hides them.
        rt.set_chrome_insets(rune::ChromeInsets::default());
        frames(&mut rt, 60);
        assert_eq!(left(&rt), 14.0);
    }
}
