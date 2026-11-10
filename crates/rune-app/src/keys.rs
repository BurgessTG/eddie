//! The Keyboard shortcuts window: every binding in `eddie_core::shortcuts`,
//! in two columns grouped like the menus.

use eddie_core::shortcuts::{BINDINGS, GROUPS, keycaps};
use rune::{Cursor, El, Shape, col, el, row, text};

use crate::app::Ui;
use crate::theme::{self, c};

const COLUMN_W: f32 = 304.0;
const COLUMN_GAP: f32 = 32.0;
/// Space between the window's border and its columns.
const PAD: f32 = 6.0;
const TITLE_H: f32 = 40.0;
const ROW_H: f32 = 28.0;
const ROW_GAP: f32 = 6.0;

fn close(ui: &mut Ui) {
    ui.show_shortcuts = false;
}

/// The window, centred. The editor behind is inert; a press outside, Esc,
/// F1 or the close button closes it.
pub fn overlay(ui: &Ui, overlays: &mut Vec<El>) {
    if !ui.show_shortcuts {
        return;
    }
    let c = c();
    let cross = Shape::new()
        .view_box(28.0, 28.0)
        .stroke(vec![[9.5, 9.5], [18.5, 18.5]], 1.1, c.foreground)
        .stroke(vec![[18.5, 9.5], [9.5, 18.5]], 1.1, c.foreground);
    let title = row()
        .w_full()
        .h(TITLE_H)
        .center()
        .fill(c.hover)
        // The window's corners, inside its border.
        .corners([theme::R_LG - 1.0, theme::R_LG - 1.0, 0.0, 0.0])
        // Padding under centring: one pixel lower, where egui sets it.
        .child(
            el().pt(2.0).child(
                text("Keyboard shortcuts")
                    .font(theme::semibold(17.0))
                    .color(c.foreground)
                    .single_line(),
            ),
        )
        .child(
            row()
                .key("shortcuts-close")
                .absolute(2.0 * COLUMN_W + COLUMN_GAP + 2.0 * PAD - 34.0, 6.0)
                .size(28.0, 28.0)
                .radius(theme::R_MD)
                .label("Close")
                .help("Close keyboard shortcuts (Esc)")
                .cursor(Cursor::Pointer)
                .hover(c.pressed)
                .press(theme::hex("#4D4D57"))
                // A title bar's cross: thinner than a panel's and not muted.
                .child(el().size(28.0, 28.0).shape(cross))
                .on_press(close),
        );
    let columns = row()
        .gap(COLUMN_GAP)
        .px(PAD)
        .pt(10.0)
        .pb(PAD)
        // Two menus to a column, in the table's order.
        .children(GROUPS.chunks(2).map(|menus| {
            let mut column = col().w(COLUMN_W);
            for (index, name) in menus.iter().enumerate() {
                if index > 0 {
                    column = column.child(el().h(18.0));
                }
                column = column.child(group(name));
            }
            column
        }));
    let card = col()
        .key("shortcuts")
        // The border draws over the padding.
        .padding(1.0)
        .fill(c.card)
        .border(1.0, c.border)
        .radius(theme::R_LG)
        .shadow(8.0, 24.0, rune::Color::BLACK.alpha(140.0 / 255.0))
        .label("Keyboard shortcuts")
        .modal(true, close)
        .child(title)
        .child(theme::divider())
        .child(columns);
    overlays.push(
        row()
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .center()
            .child(card),
    );
}

/// One menu's bindings under its name.
fn group(name: &'static str) -> El {
    let c = c();
    col()
        .w_full()
        .gap(ROW_GAP)
        .child(
            row().h(20.0).pb(2.0).center_y().child(
                text(name)
                    .font(theme::SEMIBOLD)
                    .color(c.foreground)
                    .single_line(),
            ),
        )
        .children(
            BINDINGS
                .iter()
                .filter(|binding| binding.group == name)
                .map(|binding| {
                    row()
                        .w_full()
                        .h(ROW_H)
                        .center_y()
                        .child(
                            el().pt(2.0).child(
                                text(binding.label)
                                    .font(theme::BODY)
                                    .color(c.muted_fg)
                                    .single_line(),
                            ),
                        )
                        .child(rune::spacer())
                        .child(
                            row()
                                .gap(4.0)
                                .children(keycaps(binding).into_iter().map(keycap)),
                        )
                }),
        )
}

/// One chord as a keycap chip.
fn keycap(chord: String) -> El {
    let c = c();
    row()
        .h(ROW_H)
        .px(7.0)
        .center()
        .fill(c.secondary)
        .border(1.0, c.border_strong)
        .radius(theme::R_SM)
        .child(
            text(chord)
                .font(theme::sized(12.0))
                .color(c.foreground)
                .single_line(),
        )
}

#[cfg(test)]
mod tests {
    use rune::Key;

    use crate::app::Ui;
    use crate::testing::{click, press, shown, tooltip, window};

    #[test]
    fn f1_opens_it_and_f1_escape_or_the_cross_close_it() {
        let mut rt = window(Ui::headless());
        assert!(!shown(&rt, "shortcuts"));
        press(&mut rt, Key::Function(1));
        assert!(shown(&rt, "shortcuts"));
        press(&mut rt, Key::Function(1));
        assert!(!shown(&rt, "shortcuts"), "F1 again closes it");

        press(&mut rt, Key::Function(1));
        press(&mut rt, Key::Escape);
        assert!(!shown(&rt, "shortcuts"), "Esc closes it");

        press(&mut rt, Key::Function(1));
        assert!(
            tooltip(&mut rt, "shortcuts-close").is_some_and(|words| !words.is_empty()),
            "the close button has no tooltip"
        );
        click(&mut rt, "shortcuts-close");
        assert!(!shown(&rt, "shortcuts"), "the cross closes it");
    }

    #[test]
    fn the_editor_takes_no_shortcut_while_the_list_is_open() {
        let mut rt = window(Ui::headless());
        let magnet = rt.state.editor.viewport.magnet;
        press(&mut rt, Key::Function(1));
        press(&mut rt, Key::Text("m".into()));
        assert_eq!(
            rt.state.editor.viewport.magnet, magnet,
            "M under the window"
        );
        press(&mut rt, Key::Escape);
        press(&mut rt, Key::Text("m".into()));
        assert_ne!(
            rt.state.editor.viewport.magnet, magnet,
            "M once it is closed"
        );
    }
}
