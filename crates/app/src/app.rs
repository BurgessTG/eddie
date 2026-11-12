//! The window's state and its per-frame work around the editor: the editor
//! pump, keys, actions, thumbnails and the root view.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use eddie_core::shortcuts::{self, Chord, KeyName};
use eddie_core::{Action, Editor, PreviewSink, Wake};
use openreel_media::RawFrame;
use rune::{AppHandle, Cx, El, Key, KeyEvent, Pixels, TextEdit, Tx, col, row};

use crate::dialogs::{Discard, PendingFileDialog};
use crate::menus::Menu;
use crate::theme::c;
use crate::{
    console, details, dialogs, gallery, keys, media, menus, monitor, settings, shell, timeline,
};

/// The program monitor's picture. The editor copies each frame in; Rune
/// uploads it and wakes the window.
struct MonitorSink {
    pixels: Pixels,
    size: Rc<Cell<Option<(u32, u32)>>>,
}

impl PreviewSink for MonitorSink {
    fn show(&mut self, frame: &RawFrame) {
        self.pixels
            .set(frame.width, frame.height, frame.rgba.clone());
        self.size.set(Some((frame.width, frame.height)));
    }

    fn clear(&mut self) {
        self.size.set(None);
    }
}

/// One filmstrip still of a bin asset: its source time and picture.
pub struct Thumb {
    pub time: f64,
    pub pixels: Pixels,
    pub width: u32,
    pub height: u32,
}

pub struct Ui {
    pub editor: Editor,
    /// Shows, focuses and wakes the window; `None` in tests.
    handle: Option<AppHandle<Ui>>,
    pub monitor: Pixels,
    /// The size of the picture the monitor holds, while it holds one.
    pub monitor_size: Rc<Cell<Option<(u32, u32)>>>,
    /// Filmstrip pictures by media path, taken from each finished import.
    pub thumbs: HashMap<String, Vec<Thumb>>,

    pub menu: Option<Menu>,
    pub show_media: bool,
    pub show_inspector: bool,
    pub show_console: bool,
    pub show_settings: bool,
    pub show_shortcuts: bool,
    /// The Settings dialog's typed fields and open list.
    pub settings: settings::State,
    /// The console's input line and height.
    pub console: console::State,
    /// The playhead readout's jump box and whether it is showing.
    pub jump: TextEdit,
    pub jump_open: bool,
    /// Details should focus its title text on the next frame.
    pub focus_title: bool,
    /// The transport scrubber is under the pointer or held.
    pub scrub_hot: bool,

    /// The media bin's name filter.
    pub search: TextEdit,
    pub media_w: f32,
    pub bin_drag: Option<media::BinDrag>,
    pub bin_menu: Option<media::BinMenu>,
    /// The Details panel's fields, open list and width.
    pub details: details::State,
    /// The Gallery's search box, open list and card pictures.
    pub gallery: gallery::State,
    /// Where the timeline's ruler and lanes were last laid out, in window
    /// points; `None` until they have been.
    pub timeline_rect: Option<[f32; 4]>,
    /// The pointer over the timeline's lanes.
    pub lanes: timeline::LaneInput,

    pub pending_discard: Option<Discard>,
    /// The open file dialog, answered on a worker thread.
    pub file_dialog: Option<PendingFileDialog>,
    /// Set once the user chose to close without (or after) saving.
    quit: bool,
    /// A pointer gesture that edits the timeline is held, so undo waits.
    pub pointer_held: bool,
    /// When the window last took a key or pointer event, and the answer
    /// the last poll saw.
    last_input: fn() -> Option<Instant>,
    input_seen: Option<Instant>,

    /// The window clock at the last poll, for the spinners.
    clock: f64,
    pub frame_ms: f32,
    /// How long the last view build took; the frame time includes it.
    view_ms: Cell<f32>,
    pub rss_mb: u64,
    frames: u64,
}

impl Ui {
    /// `AppSpec` wants an initialiser; `App::init` replaces it.
    pub fn unopened() -> Self {
        unreachable!("the window state is built by App::init")
    }

    /// The window around the user's editor: settings, saved curations and
    /// the files named on the command line.
    pub fn open(handle: AppHandle<Ui>, imports: Vec<PathBuf>) -> Self {
        let wake = handle.clone();
        let mut ui = Self::around(Some(handle), rune::last_input, move |sink| {
            Editor::open(Wake::new(move || wake.wake()), sink, imports)
        });
        if headless_run() {
            ui.settle();
        }
        ui
    }

    /// For tests: the same window around an editor without the user's
    /// settings, saved curations or an audio device.
    #[cfg(test)]
    pub fn headless() -> Self {
        Self::around(None, crate::testing::last_input, |sink| {
            Editor::headless_with(Wake::new(|| {}), sink)
        })
    }

    fn around(
        handle: Option<AppHandle<Ui>>,
        last_input: fn() -> Option<Instant>,
        editor: impl FnOnce(Box<dyn PreviewSink>) -> Editor,
    ) -> Self {
        let monitor = Pixels::new();
        let monitor_size = Rc::new(Cell::new(None));
        let wake = handle.clone();
        let gallery = gallery::State::new(move || {
            if let Some(handle) = &wake {
                handle.wake();
            }
        });
        let editor = editor(Box::new(MonitorSink {
            pixels: monitor.clone(),
            size: Rc::clone(&monitor_size),
        }));
        Self {
            editor,
            handle,
            monitor,
            monitor_size,
            thumbs: HashMap::new(),
            menu: None,
            show_media: true,
            show_inspector: true,
            show_console: false,
            show_settings: false,
            show_shortcuts: false,
            settings: settings::State::default(),
            console: console::State::default(),
            jump: TextEdit::new(""),
            jump_open: false,
            focus_title: false,
            scrub_hot: false,
            search: TextEdit::new(""),
            media_w: media::DEFAULT_W,
            bin_drag: None,
            bin_menu: None,
            details: details::State::default(),
            gallery,
            timeline_rect: None,
            lanes: timeline::LaneInput::default(),
            pending_discard: None,
            file_dialog: None,
            quit: false,
            pointer_held: false,
            last_input,
            input_seen: None,
            clock: 0.0,
            frame_ms: 0.0,
            view_ms: Cell::new(0.0),
            rss_mb: read_rss_mb(),
            frames: 0,
        }
    }

    /// A headless render has one frame: run the editor until the files it
    /// was opened with are imported and the monitor shows its picture.
    fn settle(&mut self) {
        let started = Instant::now();
        let mut quiet = 0;
        while started.elapsed().as_secs() < 120 && quiet < 20 {
            self.pump(started.elapsed().as_secs_f64());
            let waiting = self.editor.importing()
                || (self
                    .editor
                    .timeline
                    .active_video(self.editor.playhead)
                    .is_some()
                    && !self.editor.preview_is_current());
            quiet = if waiting { 0 } else { quiet + 1 };
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// One editor frame, in the order the editor expects: finished work in,
    /// playback out, then history.
    fn pump(&mut self, now: f64) -> bool {
        let events = self.editor.begin_frame(now);
        self.take_thumbnails();
        self.poll_file_dialog();
        timeline::sync(self);
        // A held slider is one edit, as a held clip is.
        self.pointer_held |= self.details.slider_held;
        self.editor.sync_playback();
        let typing = self.typing();
        self.editor.end_frame(self.pointer_held, typing);
        events.exported
    }

    /// A text field has the keyboard, so plain keys are text.
    pub fn typing(&self) -> bool {
        (self.jump_open && self.jump.focused)
            || self.search.focused
            || self.details.typing()
            || self.gallery.typing()
            || self.settings.typing()
            || (self.show_console && self.console.typing())
    }

    /// Where a spinner's arc is in its turn, 0 to 1 once a second.
    pub fn turn(&self) -> f32 {
        self.clock.fract() as f32
    }

    /// Something is spinning, so frames must keep coming.
    fn busy(&self) -> bool {
        self.editor.importing()
            || self.editor.exporting()
            || self.editor.curating()
            || (self.editor.curator.open && self.gallery.busy())
    }

    /// Swap the playhead readout for a box holding it, selected, so typing
    /// replaces it.
    pub fn open_jump(&mut self) {
        let readout =
            openreel_engine::format_timecode(self.editor.playhead, self.editor.timeline.fps);
        let end = readout.len();
        self.jump.replace_text(readout);
        self.jump.set_selection(0, end);
        self.jump.focused = true;
        self.jump_open = true;
    }

    /// Enter in the jump box: seek to the typed time, or say why not.
    pub fn submit_jump(&mut self) {
        self.jump_open = false;
        self.jump.focused = false;
        let spec = self.jump.text.trim().to_owned();
        if spec.is_empty() {
            return;
        }
        match self.editor.timeline.time_index().time(&spec) {
            Ok(time) => {
                let time = time.clamp(0.0, self.editor.timeline.end());
                self.editor.seek(time);
            }
            Err(error) => self.editor.status = format!("Can't jump: {error}."),
        }
    }

    /// Wake the window from another thread's finished work.
    pub fn waker(&self) -> impl Fn() + Send + 'static {
        let handle = self.handle.clone();
        move || {
            if let Some(handle) = &handle {
                handle.wake();
            }
        }
    }

    /// Move finished imports' filmstrip stills into pictures, and drop the
    /// pictures of media no longer in the bin.
    fn take_thumbnails(&mut self) {
        for asset in &mut self.editor.assets {
            if asset.thumbs.is_empty() {
                continue;
            }
            let thumbs = std::mem::take(&mut asset.thumbs)
                .into_iter()
                .map(|(time, frame)| {
                    let pixels = Pixels::new();
                    let (width, height) = (frame.width, frame.height);
                    pixels.set(width, height, frame.rgba);
                    Thumb {
                        time,
                        pixels,
                        width,
                        height,
                    }
                })
                .collect();
            self.thumbs.insert(asset.info.path.clone(), thumbs);
        }
        if self.thumbs.len() > self.editor.assets.len() {
            let assets = &self.editor.assets;
            self.thumbs
                .retain(|path, _| assets.iter().any(|asset| asset.info.path == *path));
        }
    }

    pub fn thumbs(&self, path: &str) -> &[Thumb] {
        self.thumbs.get(path).map_or(&[], Vec::as_slice)
    }

    /// What a key, menu item or button asked for. The window opens the
    /// dialogs and panels; the editor does the rest.
    pub fn act(&mut self, action: Action) {
        match action {
            Action::Import => self.pick_import(),
            Action::Export => self.pick_export(),
            Action::NewProject => self.request_discard(Discard::New),
            Action::OpenProject => self.request_discard(Discard::Open),
            Action::SaveProject => self.save_project(false, None),
            Action::SaveProjectAs => self.save_project(true, None),
            Action::LocateMedia => self.pick_missing_media(),
            Action::OpenSettings if self.show_settings => settings::close(self),
            Action::OpenSettings => {
                self.show_shortcuts = false;
                self.show_settings = true;
            }
            Action::ShowShortcuts => {
                settings::close(self);
                self.show_shortcuts = !self.show_shortcuts;
            }
            // Esc closes what is open before it deselects.
            Action::Deselect if self.jump_open => {
                self.jump_open = false;
                self.jump.focused = false;
            }
            Action::Deselect if self.menu.is_some() => self.menu = None,
            Action::Deselect if self.details.select.is_some() => self.details.select = None,
            Action::Deselect if self.gallery.popup.is_some() => self.gallery.popup = None,
            Action::Deselect if self.bin_menu.is_some() => self.bin_menu = None,
            Action::Deselect if self.settings.providers_open => {
                self.settings.providers_open = false;
            }
            Action::Deselect if self.settings.models.is_some() => self.settings.models = None,
            Action::Deselect if self.show_shortcuts => self.show_shortcuts = false,
            Action::Deselect if self.show_settings => settings::close(self),
            _ => {
                self.editor.perform(action);
            }
        }
    }

    /// Leave once the frame in progress ends.
    pub fn close(&mut self) {
        self.quit = true;
    }
}

/// Rune is rendering to a file or a script, not opening a window.
fn headless_run() -> bool {
    const FLAGS: [&str; 7] = [
        "--snap",
        "--frames",
        "--first-pixels",
        "--tree",
        "--trace",
        "--strip",
        "--script",
    ];
    std::env::args().any(|arg| FLAGS.contains(&arg.as_str()))
}

/// Whether Rune's `focus` only marks the window as wanting attention, which
/// it does on Wayland. On X11 it restores a minimized window and takes the
/// keyboard, and a finished export must not pull the user out of what they
/// are typing elsewhere; there the status bar alone reports it. The test is
/// winit's own choice of Wayland over X11.
fn asks_for_attention_only() -> bool {
    let set = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    cfg!(target_os = "linux") && (set("WAYLAND_DISPLAY") || set("WAYLAND_SOCKET"))
}

pub fn poll(ui: &mut Ui, clock: f64, tx: &mut Tx) -> bool {
    let started = Instant::now();
    ui.clock = clock;
    if ui.jump_open && !ui.jump.focused {
        // Clicking elsewhere leaves the readout as it was.
        ui.jump_open = false;
    }
    details::sync(ui);
    gallery::sync(ui);
    settings::sync(ui);
    console::sync(ui);
    if ui.pump(clock)
        && asks_for_attention_only()
        && let Some(handle) = &ui.handle
    {
        // A finished export asks to be noticed.
        handle.focus();
    }
    if ui.quit {
        tx.quit();
    }
    // Rune polls before it runs the frame's handlers, so what a key or the
    // pointer is about to change gets its poll on the next frame: playback
    // starts there, a dropped clip lands there, typed text is taken there.
    let input = (ui.last_input)();
    if input != ui.input_seen {
        ui.input_seen = input;
        tx.wake_after(std::time::Duration::ZERO);
    }
    if let Some(after) = ui.editor.repaint_after() {
        tx.wake_after(after);
    }
    if ui.busy() {
        tx.wake_after(std::time::Duration::from_millis(33));
    }
    let ms = started.elapsed().as_secs_f32() * 1000.0 + ui.view_ms.get();
    ui.frame_ms = if ui.frame_ms == 0.0 {
        ms
    } else {
        ui.frame_ms * 0.9 + ms * 0.1
    };
    ui.frames += 1;
    if ui.frames.is_multiple_of(60) {
        ui.rss_mb = read_rss_mb();
    }
    true
}

/// The table's name for a pressed key with its modifiers.
fn chord(event: &KeyEvent) -> Option<Chord> {
    let key = match &event.key {
        Key::Text(text) => {
            let mut chars = text.chars();
            let first = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            match first {
                ' ' => KeyName::Space,
                other => KeyName::Char(other.to_ascii_lowercase()),
            }
        }
        Key::Command(key) => KeyName::Char(*key),
        Key::Left => KeyName::Left,
        Key::Right => KeyName::Right,
        Key::Up => KeyName::Up,
        Key::Down => KeyName::Down,
        Key::Home => KeyName::Home,
        Key::End => KeyName::End,
        Key::Delete => KeyName::Delete,
        Key::Backspace => KeyName::Backspace,
        Key::Escape => KeyName::Escape,
        Key::Function(number) => KeyName::Function(*number),
        _ => return None,
    };
    Some(Chord {
        command: event.mods.cmd,
        shift: event.mods.shift,
        key,
    })
}

pub fn on_key(ui: &mut Ui, event: &KeyEvent, tx: &mut Tx) -> bool {
    if ui.pending_discard.is_some() {
        // The "Save changes?" prompt owns the keyboard.
        match event.key {
            Key::Enter => ui.answer_discard(true),
            Key::Escape => ui.pending_discard = None,
            _ => {}
        }
        return true;
    }
    let Some(chord) = chord(event) else {
        return false;
    };
    let Some(action) = shortcuts::action_for(chord, ui.typing(), tx.key_repeat()) else {
        return false;
    };
    // Under a dialog the editor is inert to keys as it is to the pointer.
    let dialog = ui.show_settings || ui.show_shortcuts;
    let closes = matches!(
        action,
        Action::Deselect | Action::OpenSettings | Action::ShowShortcuts
    );
    if !dialog || closes {
        ui.act(action);
    }
    true
}

/// Whether the window may close, or macOS quit Eddie (Cmd+Q, the Dock): with
/// unsaved edits it stays open and shows "Save changes?", whose answer closes it.
pub fn should_close(ui: &mut Ui, _: &mut Tx) -> bool {
    if ui.quit || !ui.editor.has_unsaved_changes() {
        return true;
    }
    ui.pending_discard = Some(Discard::Close);
    false
}

/// The window closed, and Eddie leaves with it.
pub fn on_close(_: &mut Ui, tx: &mut Tx) {
    tx.quit();
}

pub fn view(ui: &Ui, cx: &mut Cx<Ui>) -> El {
    let started = Instant::now();
    let c = c();
    cx.help_look(crate::theme::tooltip());
    let mut overlays = Vec::new();
    let mut middle = row().w_full().grow(1.0).min_h(0.0);
    if ui.show_media {
        middle = middle.child(media::panel(ui));
    }
    middle = middle.child(monitor::central(ui, cx));
    if ui.show_inspector {
        middle = middle.child(details::panel(ui));
    }
    let body = col()
        .w_full()
        .h_full()
        .fill(c.background)
        // Files dropped anywhere on the window are imported.
        .on_drop_files(|ui: &mut Ui, files, _| ui.editor.spawn_import(files.to_vec()))
        .child(shell::menu_bar(ui, cx))
        .child(middle)
        .child(timeline::panel(ui, cx));
    let body = if ui.show_console {
        body.child(console::panel(ui))
    } else {
        body
    }
    .child(shell::status_bar(ui));
    media::overlays(ui, &mut overlays);
    timeline::overlays(ui, &mut overlays);
    details::overlays(ui, cx, &mut overlays);
    gallery::overlays(ui, cx, &mut overlays);
    menus::overlay(ui, &mut overlays);
    settings::overlays(ui, &mut overlays);
    keys::overlay(ui, &mut overlays);
    dialogs::unsaved(ui, &mut overlays);
    ui.view_ms.set(started.elapsed().as_secs_f32() * 1000.0);
    rune_kit::root(body, overlays)
}

/// Resident memory in MiB for the status bar; 0 where it cannot be read.
#[cfg(not(target_os = "macos"))]
fn read_rss_mb() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|statm| statm.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map_or(0, |pages| pages * 4096 / (1024 * 1024))
}

#[cfg(target_os = "macos")]
fn read_rss_mb() -> u64 {
    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u64) -> i32;
    }
    // `struct rusage_info_v0`: a 16-byte UUID, then ten 64-bit counts, of
    // which the seventh is `ri_resident_size`.
    let mut usage = [0u64; 12];
    // SAFETY: flavor 0 (RUSAGE_INFO_V0) fills exactly those 96 bytes.
    let read = unsafe { proc_pid_rusage(std::process::id() as i32, 0, usage.as_mut_ptr()) } == 0;
    if read { usage[8] >> 20 } else { 0 }
}

#[cfg(test)]
mod tests;
