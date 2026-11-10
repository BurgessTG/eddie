//! Eddie's look in Rune: the shadcn tokens, button variants and the mark.

use std::sync::LazyLock;

use rune::{Color, Cursor, El, HelpLook, Shadow, Shape, TextStyle, el, row, text};

pub fn hex(code: &str) -> Color {
    rune::color::hex(code).expect("a literal colour")
}

/// sRGB bytes with straight alpha.
fn rgba(code: &str, alpha: u8) -> Color {
    hex(code).alpha(f32::from(alpha) / 255.0)
}

pub struct Tokens {
    /// The stage: program monitor, gallery and lanes.
    pub background: Color,
    /// Docked panels and bars.
    pub surface: Color,
    pub card: Color,
    pub secondary: Color,
    pub border: Color,
    pub border_strong: Color,
    pub input: Color,
    pub foreground: Color,
    pub primary: Color,
    pub primary_fg: Color,
    pub muted_fg: Color,
    pub subtle_fg: Color,
    /// Outlines of things in flight: a dragged tile, the lane it lands on.
    pub ring: Color,
    /// Selection only.
    pub brand: Color,
    pub destructive: Color,
    pub destructive_fg: Color,
    pub success: Color,
    pub warning: Color,
    pub hover: Color,
    pub pressed: Color,
    pub clip_video: Color,
    pub clip_audio: Color,
    pub clip_title: Color,
    pub logo_clip: Color,
    pub logo_playhead: Color,
}

static TOKENS: LazyLock<Tokens> = LazyLock::new(|| Tokens {
    background: hex("#09090B"),
    surface: hex("#101013"),
    card: hex("#18181B"),
    secondary: hex("#27272A"),
    border: hex("#27272A"),
    border_strong: hex("#3F3F46"),
    input: hex("#1C1C21"),
    foreground: hex("#FAFAFA"),
    primary: hex("#FAFAFA"),
    primary_fg: hex("#18181B"),
    muted_fg: hex("#A1A1AA"),
    subtle_fg: hex("#71717A"),
    ring: hex("#A1A1AA"),
    brand: hex("#3B82F6"),
    destructive: hex("#EF4444"),
    destructive_fg: hex("#FCA5A5"),
    success: hex("#22C55E"),
    warning: hex("#F59E0B"),
    hover: hex("#323239"),
    pressed: hex("#42424B"),
    clip_video: hex("#2E4A6B"),
    clip_audio: hex("#2C543C"),
    clip_title: hex("#6B5318"),
    logo_clip: hex("#FFEEEF"),
    logo_playhead: hex("#8B5CF6"),
});

pub fn c() -> &'static Tokens {
    &TOKENS
}

/// Behind whatever is selected: chosen text in a field, the current entry
/// of a list.
pub fn selection() -> Color {
    rgba("#3B82F6", 70)
}

/// How every tooltip looks: a card with the popup shadow, just under its
/// element.
pub fn tooltip() -> HelpLook {
    let c = c();
    HelpLook::system(true)
        .fill(c.card)
        .ink(c.foreground)
        .ring(1.0, c.border)
        .radius(R_MD)
        .padding(7.0, 7.0)
        .font(BODY.lh(16.0))
        .max_width(Some(500.0))
        .gap(4.0)
        .shadows([Shadow {
            y: 4.0,
            blur: 16.0,
            color: Color::BLACK.alpha(120.0 / 255.0),
            ..Shadow::default()
        }])
}

/// A field or select on a card: white at 4% over it.
pub fn field_fill() -> Color {
    hex("#212124")
}

pub const R_SM: f32 = 6.0;
pub const R_MD: f32 = 8.0;
pub const R_LG: f32 = 10.0;

/// Body text: Inter 13.
pub const BODY: TextStyle = TextStyle::new(13.0, 400.0);
pub const SMALL: TextStyle = TextStyle::new(11.5, 400.0);
pub const SEMIBOLD: TextStyle = TextStyle::new(13.0, 600.0);
pub const MONO: TextStyle = TextStyle::new(12.0, 400.0).mono();

pub fn sized(size: f32) -> TextStyle {
    TextStyle::new(size, 400.0)
}

pub fn semibold(size: f32) -> TextStyle {
    TextStyle::new(size, 600.0)
}

/// Mono text on a whole-pixel grid: the 0.6 em advance is tracked to the
/// nearest pixel, so timecodes and counters keep their width as digits change.
pub fn mono(size: f32) -> TextStyle {
    let advance = size * 0.6;
    TextStyle::new(size, 400.0)
        .mono()
        .tracking(advance.round() - advance)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// One per area: the area's main action.
    Primary,
    /// Everyday actions: filled, borderless.
    Secondary,
    Ghost,
    Destructive,
}

struct Look {
    fg: Color,
    rest: Color,
    hover: Color,
    pressed: Color,
}

fn look(variant: Variant) -> Look {
    let c = c();
    match variant {
        Variant::Primary => Look {
            fg: c.primary_fg,
            rest: c.primary,
            hover: hex("#E4E4E7"),
            pressed: hex("#D4D4D8"),
        },
        Variant::Secondary => Look {
            fg: c.foreground,
            rest: c.secondary,
            hover: c.hover,
            pressed: c.pressed,
        },
        Variant::Ghost => Look {
            fg: c.foreground,
            rest: Color::TRANSPARENT,
            hover: c.secondary,
            pressed: c.hover,
        },
        Variant::Destructive => Look {
            fg: c.destructive_fg,
            rest: rgba("#EF4444", 28),
            hover: rgba("#EF4444", 60),
            pressed: rgba("#EF4444", 95),
        },
    }
}

/// A text button: borderless at rest, with the variant's hover and pressed
/// fills. `help` is its tooltip; the caller adds `on_press`. A disabled
/// button keeps its tooltip and dims its label.
pub fn button(key: &str, label: &str, variant: Variant, help: &str, enabled: bool) -> El {
    let look = look(variant);
    let label = text(label.to_owned())
        .font(BODY)
        .color(if enabled {
            look.fg
        } else {
            look.fg.alpha(0.45)
        })
        .single_line();
    let frame = row()
        .key(key.to_owned())
        .px(12.0)
        .py(5.5)
        .center()
        .radius(R_MD)
        .fill(look.rest)
        .help(help.to_owned())
        .child(label);
    if enabled {
        frame
            .hover(look.hover)
            .press(look.pressed)
            .cursor(Cursor::Pointer)
    } else {
        frame.disabled(true)
    }
}

/// A 30×30 button around a hand-drawn icon (`icons::tool`, drawn in the
/// foreground colour): ghost at rest with a muted glyph, filled when
/// `active` (a toggle that is on). The glyph brightens under the pointer.
pub fn icon_button(key: &str, icon: Shape, help: &str, active: bool, enabled: bool) -> El {
    glyph_button(
        key,
        el().size(30.0, 30.0).shape(icon),
        help,
        active,
        enabled,
    )
}

/// An icon button whose glyph is a letter.
pub fn letter_button(key: &str, letter: &str, help: &str, active: bool, enabled: bool) -> El {
    let glyph = row().size(30.0, 30.0).center().child(
        text(letter.to_owned())
            .font(sized(16.0))
            .color(c().foreground)
            .single_line(),
    );
    glyph_button(key, glyph, help, active, enabled)
}

fn glyph_button(key: &str, glyph: El, help: &str, active: bool, enabled: bool) -> El {
    let c = c();
    // The glyph is drawn bright; resting buttons show it at MUTED_FG's
    // level, which on these greys is 62% of FOREGROUND.
    let glyph = if !enabled {
        glyph.opacity(0.3)
    } else if active {
        glyph
    } else {
        glyph.group_opacity(0.62, 1.0)
    };
    let frame = row()
        .key(key.to_owned())
        .size(30.0, 30.0)
        .radius(R_MD)
        .fill(if active {
            c.secondary
        } else {
            Color::TRANSPARENT
        })
        .help(help.to_owned())
        .child(glyph);
    if enabled {
        frame
            .hover_group()
            .hover(c.secondary)
            .press(hex("#404049"))
            .cursor(Cursor::Pointer)
    } else {
        frame.disabled(true)
    }
}

/// shadcn Tabs: a sunken track with the active segment raised. Each option
/// is `(key, label, tooltip, active)`; the caller adds `on_press` to each
/// returned segment through `wire`.
pub fn segmented(
    key: &str,
    options: Vec<(String, &str, &str, bool)>,
    wire: impl Fn(usize, El) -> El,
) -> El {
    let c = c();
    let mut track = row()
        .key(key.to_owned())
        .padding(3.0)
        .radius(R_MD)
        .fill(c.input);
    for (index, (segment, label, help, active)) in options.into_iter().enumerate() {
        let item = row()
            .key(segment)
            .px(14.0)
            .py(6.0)
            .center()
            .radius(R_SM)
            .fill(if active {
                c.secondary
            } else {
                Color::TRANSPARENT
            })
            .hover(if active {
                c.secondary
            } else {
                Color {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 8.0 / 255.0,
                }
            })
            .cursor(Cursor::Pointer)
            .help(help.to_owned())
            // An unselected segment's label lights up under the pointer.
            .hover_group()
            .child(
                text(label.to_owned())
                    .font(BODY)
                    .color(if active { c.foreground } else { c.muted_fg })
                    .group_color(c.foreground)
                    .single_line(),
            );
        track = track.child(wire(index, item));
    }
    track
}

/// How tall fields, closed lists, list rows and checkboxes are.
pub const FIELD_H: f32 = 28.0;

/// A round switch with its label. The caller adds `on_press`.
pub fn checkbox(key: &str, label: &str, help: &str, on: bool) -> El {
    let c = c();
    let mark = Shape::new().view_box(14.0, 14.0);
    let mark = if on {
        mark.stroke(
            vec![[3.4, 8.2], [6.0, 10.7], [10.6, 5.3]],
            1.3,
            c.foreground,
        )
    } else {
        mark
    };
    row()
        .key(key.to_owned())
        .h(FIELD_H)
        .gap(4.0)
        .center_y()
        .role("checkbox")
        .checked(on)
        .focusable(true)
        .label(label.to_owned())
        .cursor(Cursor::Pointer)
        .help(help.to_owned())
        .child(
            el().size(14.0, 14.0)
                .child(
                    el().absolute(0.0, 0.0)
                        .size(14.0, 14.0)
                        .radius(7.0)
                        .fill(c.secondary),
                )
                .child(el().absolute(0.0, 0.0).size(14.0, 14.0).shape(mark)),
        )
        .child(
            text(label.to_owned())
                .font(BODY)
                .color(c.foreground)
                .single_line(),
        )
}

/// A closed list: its choice and a down arrow, over `fill` at rest. The
/// caller sets its width and, when `enabled`, adds `on_press`.
pub fn select(key: &str, choice: &str, help: &str, fill: Color, open: bool, enabled: bool) -> El {
    let c = c();
    let field = row()
        .key(key.to_owned())
        .h(FIELD_H)
        .pl(12.0)
        .pr(14.0)
        .gap(6.0)
        .center_y()
        .radius(R_MD)
        .fill(if open { c.hover } else { fill })
        .role("combobox")
        .expanded(open)
        .label(choice.to_owned())
        .help(help.to_owned())
        .child(
            text(choice.to_owned())
                .font(BODY)
                .color(c.foreground)
                .single_line()
                .truncate(),
        )
        .child(rune::spacer())
        .child(
            el().size(10.0, 8.0).shape(
                Shape::new()
                    .view_box(10.0, 8.0)
                    .fill(vec![[0.25, 1.3], [9.75, 1.3], [5.0, 7.3]], c.foreground),
            ),
        );
    if enabled {
        field
            .hover(if open { c.hover } else { c.secondary })
            .press(c.hover)
            .focusable(true)
            .cursor(Cursor::Pointer)
    } else {
        field.disabled(true).opacity(0.5)
    }
}

/// One choice in an open list. The current one is marked as selection is
/// everywhere else: a brand tint behind brand text. The caller adds
/// `on_press`.
pub fn option(key: String, label: &str, help: &str, current: bool) -> El {
    let c = c();
    let option = row()
        .key(key)
        .w_full()
        .h(FIELD_H)
        .px(12.0)
        .center_y()
        .radius(R_MD)
        .cursor(Cursor::Pointer)
        .role("option")
        .label(label.to_owned())
        .help(help.to_owned())
        .child(
            text(label.to_owned())
                .font(BODY)
                .color(if current { c.brand } else { c.foreground })
                .single_line(),
        );
    if current {
        option.fill(selection())
    } else {
        option.hover(c.hover).press(c.pressed)
    }
}

/// The frame of an open list or popup: a card with the popup shadow.
pub fn popup(key: &str) -> El {
    let c = c();
    rune::col()
        .key(key.to_owned())
        // A margin of 6 inside a border of 1.
        .padding(7.0)
        .fill(c.card)
        .border(1.0, c.border)
        .radius(R_MD)
        .shadow(4.0, 16.0, Color::BLACK.alpha(120.0 / 255.0))
}

/// A 28×28 close button: an X that brightens over a hover fill.
pub fn close_button(key: &str, help: &str) -> El {
    let c = c();
    let cross = Shape::new()
        .view_box(28.0, 28.0)
        .stroke(vec![[9.0, 9.0], [19.0, 19.0]], 1.6, c.foreground)
        .stroke(vec![[19.0, 9.0], [9.0, 19.0]], 1.6, c.foreground);
    row()
        .key(key.to_owned())
        .size(28.0, 28.0)
        .radius(R_MD)
        .label("Close")
        .help(help.to_owned())
        .cursor(Cursor::Pointer)
        .hover_group()
        .hover(c.secondary)
        .press(hex("#404049"))
        // MUTED_FG's level at rest, as the icon buttons.
        .child(el().size(28.0, 28.0).shape(cross).group_opacity(0.62, 1.0))
}

/// Section label: semibold, foreground.
pub fn panel_title(label: &str) -> El {
    text(label.to_owned())
        .font(SEMIBOLD)
        .color(c().foreground)
        .single_line()
}

/// Small muted text under a title or beside a value.
pub fn caption(label: impl Into<String>) -> El {
    text(label.into()).font(SMALL).color(c().muted_fg)
}

pub fn divider() -> El {
    el().h(1.0).w_full().fill(c().border)
}

/// A grouped block inside a panel (shadcn Card).
pub fn card() -> El {
    rune::col()
        .fill(c().card)
        .border(1.0, c().border)
        .radius(R_LG)
        // 12 inside the border, which draws over the padding.
        .padding(13.0)
}

// -- logo ----------------------------------------------------------------------
/// Clip capsules as [x, y, width, height] in a 1×1 box.
const MARK_CLIPS: [[f32; 4]; 3] = [
    [0.1027, 0.1724, 0.8477, 0.2126],
    [0.0496, 0.4784, 0.6782, 0.2155],
    [0.1027, 0.7845, 0.8477, 0.2155],
];
/// Playhead line [x, y, width, height]; its tip points down from the top.
const MARK_LINE: [f32; 4] = [0.2967, 0.0632, 0.0187, 0.9368];

/// The Eddie mark, `size` points square: three timeline clips forming an E,
/// crossed by a violet playhead. At menu-bar size the playhead keeps a
/// visible width and tip.
pub fn mark(size: f32) -> El {
    let c = c();
    let mut shape = Shape::new().view_box(size, size);
    for [x, y, w, h] in MARK_CLIPS {
        let (x, y, w, h) = (x * size, y * size, w * size, h * size);
        let radius = h / 2.0;
        // A capsule: a round-capped line as thick as the clip is tall.
        shape = shape.line(
            [x + radius, y + radius],
            [x + w - radius, y + radius],
            h,
            rune::LineCap::Round,
            c.logo_clip,
        );
    }
    let [x, y, w, h] = MARK_LINE;
    let center = (x + w / 2.0) * size;
    let half = (w * size).max(1.6) / 2.0;
    shape = shape.fill(
        vec![
            [center - half, y * size],
            [center + half, y * size],
            [center + half, (y + h) * size],
            [center - half, (y + h) * size],
        ],
        c.logo_playhead,
    );
    let tip = (0.0697 * size).max(3.2);
    let top = -(tip * 1.11 - 0.0776 * size).max(0.0);
    shape = shape.fill(
        vec![
            [center - tip, top],
            [center + tip, top],
            [center, top + tip * 1.11],
        ],
        c.logo_playhead,
    );
    el().size(size, size).shape(shape)
}
