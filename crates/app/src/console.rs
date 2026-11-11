//! The JS console under the timeline: what scripts printed, and a line to
//! run JavaScript against the editor. `Editor::run_console` runs it and
//! keeps the output and the history.

use rune::{Cursor, El, Key, Phase, ScrollSpec, TextEdit, col, el, row, text};
use rune_kit::{ScrollBar, ScrollBarStyle};

use crate::app::Ui;
use crate::theme::{self, Variant, c};

/// The console with its output full: 148, its top border included.
pub const DEFAULT_H: f32 = 148.0;
const MIN_H: f32 = 90.0;
const MAX_H: f32 = 340.0;
const PAD_X: f32 = 14.0;
const INPUT_KEY: &str = "console-input";

pub struct State {
    pub input: TextEdit,
    /// The panel's height, its top border included.
    pub height: f32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            input: TextEdit::new(""),
            height: DEFAULT_H,
        }
    }
}

impl State {
    /// The input line has the keyboard, so plain keys are text.
    pub fn typing(&self) -> bool {
        self.input.focused
    }
}

/// Once per frame: a hidden console holds no keys.
pub fn sync(ui: &mut Ui) {
    if !ui.show_console {
        ui.console.input.focused = false;
    }
}

/// Enter: run the line and empty it, keeping the keyboard for the next.
fn run(ui: &mut Ui) {
    let code = ui.console.input.text.clone();
    ui.console.input.clear();
    ui.editor.run_console(code);
}

/// Up or Down: put an earlier or later line in the input, caret at its end.
fn recall(ui: &mut Ui, older: bool) {
    if let Some(line) = ui.editor.recall_console(older) {
        let end = line.len();
        ui.console.input.replace_text(line);
        ui.console.input.set_caret(end);
    }
}

pub fn panel(ui: &Ui) -> El {
    let c = c();
    el().key("console")
        .w_full()
        .h(ui.console.height)
        .fill(c.surface)
        .child(
            col()
                .w_full()
                .h_full()
                .px(PAD_X)
                .py(8.0)
                .child(header())
                .child(el().h(3.0))
                .child(output(ui))
                .child(el().h(5.0))
                .child(prompt(ui)),
        )
        .child(el().absolute(0.0, 0.0).w_full().h(1.0).fill(c.border))
        .child(
            el().key("console-resize")
                .absolute(0.0, -3.0)
                .w_full()
                .h(7.0)
                .cursor(Cursor::ResizeRow)
                .help("Drag to resize the console")
                .on_drag(|ui: &mut Ui, drag, _| {
                    if drag.phase == Phase::Down {
                        drag.data = ui.console.height;
                    } else {
                        ui.console.height = (drag.data - drag.dy).round().clamp(MIN_H, MAX_H);
                    }
                }),
        )
}

fn header() -> El {
    row()
        .w_full()
        .h(28.0)
        .center_y()
        .child(theme::panel_title("Console"))
        .child(rune::spacer())
        .child(
            theme::button(
                "console-clear",
                "Clear",
                Variant::Ghost,
                "Clear console output",
                true,
            )
            .on_press(|ui: &mut Ui| ui.editor.console_output.clear()),
        )
}

/// What the scripts printed, newest at the bottom, where it stays.
fn output(ui: &Ui) -> El {
    let c = c();
    col()
        .key("console-output")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: true })
        .child(
            col()
                .w_full()
                .gap(6.0)
                .children(ui.editor.console_output.iter().map(|line| {
                    text(line.clone())
                        .font(theme::MONO.lh(14.0))
                        .color(c.foreground)
                        .wrap()
                        .selectable(true)
                        .selection_color(theme::selection())
                })),
        )
        .scroll_bar_styled(ScrollBarStyle::native(c.border_strong, c.subtle_fg).inside())
}

/// The `>` line: Enter runs it, Up and Down recall history, Esc lets go
/// of the keyboard.
fn prompt(ui: &Ui) -> El {
    let c = c();
    let input = rune::text_input(&ui.console.input, theme::MONO.lh(14.0))
        .selection_color(theme::selection())
        .key(INPUT_KEY)
        .single_line()
        .grow(1.0)
        .min_w(0.0)
        .h(19.0)
        .px(4.0)
        .py(2.5)
        .radius(4.0)
        .fill(c.input)
        .color(c.foreground)
        .placeholder("JavaScript. help() lists the editor API.", c.muted_fg)
        .focus_within_ring(1.0, -1.0, c.brand)
        .label("Console input")
        .help("Run JavaScript against the editor. Enter runs. Up and Down recall history.")
        .on_edit(rune::edit_access(|ui: &mut Ui| &mut ui.console.input))
        .on_submit(|ui: &mut Ui, _| run(ui))
        .on_input_key(|ui: &mut Ui, event, tx| match event.key {
            Key::Up => {
                recall(ui, true);
                true
            }
            Key::Down => {
                recall(ui, false);
                true
            }
            Key::Escape => {
                tx.blur();
                true
            }
            _ => false,
        });
    row()
        .w_full()
        .h(28.0)
        // Centres the 19-high input on whole pixels.
        .pt(1.0)
        .gap(7.0)
        .center_y()
        .child(
            text(">")
                .font(theme::MONO.lh(14.0))
                .color(c.foreground)
                .single_line(),
        )
        .child(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{click, press, window};

    fn console() -> rune::Runtime<Ui> {
        let mut ui = Ui::headless();
        ui.show_console = true;
        window(ui)
    }

    #[test]
    fn enter_runs_the_line_and_keeps_the_keyboard() {
        let mut rt = console();
        click(&mut rt, INPUT_KEY);
        assert!(rt.state.console.input.focused, "a click takes the keyboard");
        assert!(rt.state.typing(), "plain keys are text, not shortcuts");
        rt.paste("1 + 1");
        press(&mut rt, Key::Enter);
        let output = &rt.state.editor.console_output;
        assert_eq!(&output[output.len() - 2..], ["> 1 + 1", "2"]);
        assert_eq!(rt.state.console.input.text, "");
        assert!(rt.state.console.input.focused);
    }

    #[test]
    fn up_and_down_walk_the_history() {
        let mut rt = console();
        click(&mut rt, INPUT_KEY);
        for line in ["1", "2"] {
            rt.paste(line);
            press(&mut rt, Key::Enter);
        }
        press(&mut rt, Key::Up);
        assert_eq!(rt.state.console.input.text, "2");
        press(&mut rt, Key::Up);
        press(&mut rt, Key::Up);
        assert_eq!(rt.state.console.input.text, "1", "Up stops at the oldest");
        press(&mut rt, Key::Down);
        press(&mut rt, Key::Down);
        assert_eq!(rt.state.console.input.text, "", "Down past the newest");
        press(&mut rt, Key::Escape);
        assert!(!rt.state.console.input.focused);
    }

    #[test]
    fn clear_empties_the_output() {
        let mut rt = console();
        rt.state.editor.run_console("1".to_owned());
        click(&mut rt, "console-clear");
        assert!(rt.state.editor.console_output.is_empty());
    }

    #[test]
    fn a_hidden_console_gives_the_keys_back() {
        let mut rt = console();
        click(&mut rt, INPUT_KEY);
        rt.state.show_console = false;
        crate::testing::frames(&mut rt, 2);
        assert!(!rt.state.typing());
    }
}
