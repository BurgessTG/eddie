//! shadcn/ui design tokens translated to egui.
//!
//! shadcn itself is React/Tailwind; what transfers is the standard it sets:
//! zinc neutrals, CSS-variable style tokens, one radius scale, subtle
//! borders instead of shadows, and named button variants.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Response,
    RichText, Sense, Stroke, TextStyle, Ui, Vec2, Visuals,
};

// -- shadcn dark (zinc) tokens ------------------------------------------------
/// The stage behind the program monitor and gallery: the darkest surface.
pub const BACKGROUND: Color32 = Color32::from_rgb(0x09, 0x09, 0x0B);
/// Docked panels (media, inspector, timeline, bars) sit one step above.
pub const SURFACE: Color32 = Color32::from_rgb(0x10, 0x10, 0x13);
pub const FOREGROUND: Color32 = Color32::from_rgb(0xFA, 0xFA, 0xFA);
pub const CARD: Color32 = Color32::from_rgb(0x18, 0x18, 0x1B);
pub const PRIMARY: Color32 = Color32::from_rgb(0xFA, 0xFA, 0xFA);
pub const PRIMARY_FG: Color32 = Color32::from_rgb(0x18, 0x18, 0x1B);
pub const SECONDARY: Color32 = Color32::from_rgb(0x27, 0x27, 0x2A);
pub const MUTED_FG: Color32 = Color32::from_rgb(0xA1, 0xA1, 0xAA);
/// Quietest readable text: captions, counts, timecode units.
pub const SUBTLE_FG: Color32 = Color32::from_rgb(0x71, 0x71, 0x7A);
pub const ACCENT: Color32 = Color32::from_rgb(0x27, 0x27, 0x2A);
pub const DESTRUCTIVE: Color32 = Color32::from_rgb(0xEF, 0x44, 0x44);
/// Health only (the playback rate dot): green-500 and amber-500.
pub const SUCCESS: Color32 = Color32::from_rgb(0x22, 0xC5, 0x5E);
pub const WARNING: Color32 = Color32::from_rgb(0xF5, 0x9E, 0x0B);
pub const BORDER: Color32 = Color32::from_rgb(0x27, 0x27, 0x2A);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x3F, 0x3F, 0x46);
pub const INPUT: Color32 = Color32::from_rgb(0x1C, 0x1C, 0x21);
pub const RING: Color32 = Color32::from_rgb(0xA1, 0xA1, 0xAA);
/// The one chromatic accent: selection and focus (shadcn blue-500).
pub const BRAND: Color32 = Color32::from_rgb(0x3B, 0x82, 0xF6);

/// Clip fills. Descriptive per-kind colour, kept muted like pro NLEs.
pub const CLIP_VIDEO: Color32 = Color32::from_rgb(0x2E, 0x4A, 0x6B);
pub const CLIP_AUDIO: Color32 = Color32::from_rgb(0x2C, 0x54, 0x3C);
pub const CLIP_TITLE: Color32 = Color32::from_rgb(0x6B, 0x53, 0x18);
pub const PLAYHEAD: Color32 = Color32::from_rgb(0xEF, 0x44, 0x44);

pub const R_SM: u8 = 6;
pub const R_MD: u8 = 8;
pub const R_LG: u8 = 10;

const HOVER_FILL: Color32 = Color32::from_rgb(0x32, 0x32, 0x39);
const PRESSED_FILL: Color32 = Color32::from_rgb(0x42, 0x42, 0x4B);

fn stroke(color: Color32) -> Stroke {
    Stroke::new(1.0_f32, color)
}

/// Apply the full theme: fonts (Inter when present on the system), colors,
/// radius scale, and spacing.
pub fn apply(ctx: &egui::Context) {
    load_fonts(ctx);
    ctx.set_visuals(visuals());
    ctx.style_mut(|style| {
        style.text_styles = [
            (TextStyle::Body, FontId::proportional(13.0)),
            (TextStyle::Button, FontId::proportional(13.0)),
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Heading, FontId::new(17.0, semibold())),
            (TextStyle::Monospace, FontId::monospace(12.0)),
        ]
        .into();
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(12.0, 5.0);
        style.spacing.menu_margin = Margin::same(6);
        style.spacing.indent = 14.0;
        style.spacing.interact_size = Vec2::new(40.0, 28.0);
        style.spacing.slider_width = 140.0;
        style.spacing.combo_width = 120.0;
        style.spacing.text_edit_width = 200.0;
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating = true;
    });
}

fn visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = SURFACE;
    v.window_fill = CARD;
    v.faint_bg_color = CARD;
    v.extreme_bg_color = INPUT;
    v.code_bg_color = SECONDARY;
    v.warn_fg_color = Color32::from_rgb(0xEA, 0xB3, 0x08);
    v.error_fg_color = DESTRUCTIVE;
    v.hyperlink_color = Color32::from_rgb(0x93, 0xC5, 0xFD);
    v.window_stroke = stroke(BORDER);
    v.window_corner_radius = CornerRadius::same(R_LG);
    v.menu_corner_radius = CornerRadius::same(R_MD);
    v.window_shadow = egui::epaint::Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(140),
    };
    v.popup_shadow = egui::epaint::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    v.slider_trailing_fill = true;

    v.widgets.noninteractive.bg_fill = CARD;
    v.widgets.noninteractive.weak_bg_fill = SURFACE;
    v.widgets.noninteractive.bg_stroke = stroke(BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, FOREGROUND);
    v.widgets.noninteractive.corner_radius = CornerRadius::same(R_MD);

    // Resting widgets are borderless and nearly flat (shadcn-style).
    // Definition comes from hover and pressed fills, not outlines.
    v.widgets.inactive.bg_fill = SECONDARY;
    v.widgets.inactive.weak_bg_fill = Color32::from_white_alpha(10);
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, FOREGROUND);
    v.widgets.inactive.corner_radius = CornerRadius::same(R_MD);

    v.widgets.hovered.bg_fill = HOVER_FILL;
    v.widgets.hovered.weak_bg_fill = HOVER_FILL;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_white_alpha(20));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, FOREGROUND);
    v.widgets.hovered.corner_radius = CornerRadius::same(R_MD);
    v.widgets.hovered.expansion = 0.5;

    v.widgets.active.bg_fill = PRESSED_FILL;
    v.widgets.active.weak_bg_fill = PRESSED_FILL;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, Color32::from_white_alpha(34));
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, FOREGROUND);
    v.widgets.active.corner_radius = CornerRadius::same(R_MD);
    v.widgets.active.expansion = 0.5;

    v.widgets.open = v.widgets.hovered;

    v.selection.bg_fill = Color32::from_rgba_unmultiplied(0x3B, 0x82, 0xF6, 70);
    v.selection.stroke = Stroke::new(1.0_f32, BRAND);
    v.text_cursor.stroke = Stroke::new(1.5_f32, FOREGROUND);
    v
}

/// Font family used for headings, section titles, and emphasis.
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// Prefer Inter (shadcn's usual sans) when installed; fall back silently to
/// egui's bundled font rather than shipping a font binary in the repo.
/// `Inter.ttc` collection indices: 0 Regular, 12 SemiBold.
fn load_fonts(ctx: &egui::Context) {
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        ("/usr/share/fonts/inter/Inter.ttc".to_owned(), 0, Some(12)),
        ("/usr/share/fonts/TTF/Inter.ttc".to_owned(), 0, Some(12)),
        (
            "/usr/share/fonts/inter/Inter-Regular.otf".to_owned(),
            0,
            None,
        ),
        ("/usr/share/fonts/TTF/Inter-Regular.ttf".to_owned(), 0, None),
        (
            format!("{home}/.local/share/fonts/Inter-Regular.ttf"),
            0,
            None,
        ),
    ];
    let mut fonts = FontDefinitions::default();
    let mut regular = None;
    let mut bold = None;
    // One copy for the process lifetime; both faces (and every context a
    // test builds) borrow it, so the 13 MB collection is read once.
    static FONT_FILE: std::sync::OnceLock<Option<(Vec<u8>, u32, Option<u32>)>> =
        std::sync::OnceLock::new();
    let found = FONT_FILE.get_or_init(|| {
        candidates.iter().find_map(|(path, index, semibold_index)| {
            std::fs::read(path)
                .ok()
                .map(|bytes| (bytes, *index, *semibold_index))
        })
    });
    if let Some((bytes, index, semibold_index)) = found {
        let bytes: &'static [u8] = bytes.as_slice();
        bold = semibold_index.map(|index| FontData {
            index,
            ..FontData::from_static(bytes)
        });
        regular = Some(FontData {
            index: *index,
            ..FontData::from_static(bytes)
        });
    }
    let proportional = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut heading = proportional.clone();
    if let Some(data) = regular {
        fonts
            .font_data
            .insert("Inter".to_owned(), std::sync::Arc::new(data));
        if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
            family.insert(0, "Inter".to_owned());
        }
        heading.insert(0, "Inter".to_owned());
    }
    if let Some(data) = bold {
        fonts
            .font_data
            .insert("InterSemiBold".to_owned(), std::sync::Arc::new(data));
        heading.insert(0, "InterSemiBold".to_owned());
    }
    fonts.families.insert(semibold(), heading);
    ctx.set_fonts(fonts);
}

// -- surfaces ------------------------------------------------------------------

/// Frame for a docked panel: surface fill and even padding.
pub fn panel_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .inner_margin(Margin::symmetric(14, 12))
}

/// A grouped block inside a panel (shadcn Card).
pub fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .stroke(stroke(BORDER))
        .corner_radius(CornerRadius::same(R_LG))
        .inner_margin(Margin::same(12))
}

// -- shadcn-style variants -----------------------------------------------------

struct Variant {
    fg: Color32,
    rest: Color32,
    hover: Color32,
    pressed: Color32,
    rest_stroke: Stroke,
    hover_stroke: Stroke,
}

/// Hand-painted button so each variant gets distinct rest / hover / pressed
/// fills. egui's `Button::fill` is static across states, which reads flat.
/// Text is centered when `min_width` makes the button wider than its label.
fn variant_button(ui: &mut Ui, label: &str, variant: Variant, min_width: f32) -> Response {
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), FontId::proportional(13.0), variant.fg);
    let size = Vec2::new(
        (galley.size().x + 24.0).max(min_width),
        galley.size().y + 11.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let enabled = ui.is_enabled();
    let (fill, stroke) = if !enabled {
        (variant.rest, variant.rest_stroke)
    } else if response.is_pointer_button_down_on() {
        (variant.pressed, variant.hover_stroke)
    } else if response.hovered() {
        (variant.hover, variant.hover_stroke)
    } else {
        (variant.rest, variant.rest_stroke)
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(R_MD),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    let text_pos = egui::pos2(
        rect.center().x - galley.size().x / 2.0,
        rect.center().y - galley.size().y / 2.0,
    );
    let fg = if enabled {
        variant.fg
    } else {
        variant.fg.gamma_multiply(0.45)
    };
    painter.galley(text_pos, galley, fg);
    if enabled && response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

fn primary() -> Variant {
    Variant {
        fg: PRIMARY_FG,
        rest: PRIMARY,
        hover: Color32::from_rgb(0xE4, 0xE4, 0xE7),
        pressed: Color32::from_rgb(0xD4, 0xD4, 0xD8),
        rest_stroke: Stroke::NONE,
        hover_stroke: Stroke::NONE,
    }
}

fn secondary() -> Variant {
    Variant {
        fg: FOREGROUND,
        rest: SECONDARY,
        hover: HOVER_FILL,
        pressed: PRESSED_FILL,
        rest_stroke: Stroke::NONE,
        hover_stroke: Stroke::NONE,
    }
}

fn ghost() -> Variant {
    Variant {
        fg: FOREGROUND,
        rest: Color32::TRANSPARENT,
        hover: ACCENT,
        pressed: HOVER_FILL,
        rest_stroke: Stroke::NONE,
        hover_stroke: Stroke::NONE,
    }
}

pub fn primary_button(ui: &mut Ui, label: &str) -> Response {
    variant_button(ui, label, primary(), 0.0)
}

/// Primary button that fills the available width (a panel's main action).
pub fn primary_button_wide(ui: &mut Ui, label: &str) -> Response {
    let width = ui.available_width();
    variant_button(ui, label, primary(), width)
}

/// Filled, borderless neutral button for everyday actions.
pub fn secondary_button(ui: &mut Ui, label: &str) -> Response {
    variant_button(ui, label, secondary(), 0.0)
}

/// Secondary button that fills the available width.
pub fn secondary_button_wide(ui: &mut Ui, label: &str) -> Response {
    let width = ui.available_width();
    variant_button(ui, label, secondary(), width)
}

pub fn ghost_button(ui: &mut Ui, label: &str) -> Response {
    variant_button(ui, label, ghost(), 0.0)
}

/// Ghost button that fills the available width.
pub fn ghost_button_wide(ui: &mut Ui, label: &str) -> Response {
    let width = ui.available_width();
    variant_button(ui, label, ghost(), width)
}

/// Destructive button that fills the available width.
pub fn destructive_button_wide(ui: &mut Ui, label: &str) -> Response {
    let width = ui.available_width();
    destructive_variant(ui, label, width)
}

fn destructive_variant(ui: &mut Ui, label: &str, min_width: f32) -> Response {
    let red = |a: u8| Color32::from_rgba_unmultiplied(0xEF, 0x44, 0x44, a);
    variant_button(
        ui,
        label,
        Variant {
            fg: Color32::from_rgb(0xFC, 0xA5, 0xA5),
            rest: red(28),
            hover: red(60),
            pressed: red(95),
            rest_stroke: Stroke::NONE,
            hover_stroke: Stroke::NONE,
        },
        min_width,
    )
}

/// shadcn Tabs: a sunken track with the active segment raised.
/// Returns the clicked value, if any. Each option is
/// `(value, label, tooltip)`.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    current: T,
    options: &[(T, &str, &str)],
) -> Option<T> {
    let font = FontId::proportional(13.0);
    let galleys: Vec<_> = options
        .iter()
        .map(|(_, label, _)| {
            ui.painter()
                .layout_no_wrap((*label).to_owned(), font.clone(), FOREGROUND)
        })
        .collect();
    let pad = Vec2::new(14.0, 6.0);
    let inset = 3.0;
    let height = galleys.iter().map(|g| g.size().y).fold(0.0, f32::max) + pad.y * 2.0;
    let width: f32 = galleys
        .iter()
        .map(|g| g.size().x + pad.x * 2.0)
        .sum::<f32>()
        + inset * 2.0;
    let (track, _) = ui.allocate_exact_size(Vec2::new(width, height + inset * 2.0), Sense::hover());
    ui.painter()
        .rect_filled(track, CornerRadius::same(R_MD), INPUT);
    let mut clicked = None;
    let mut x = track.left() + inset;
    for ((value, _, tooltip), galley) in options.iter().zip(galleys) {
        let rect = egui::Rect::from_min_size(
            egui::pos2(x, track.top() + inset),
            Vec2::new(galley.size().x + pad.x * 2.0, height),
        );
        x = rect.right();
        let response = ui
            .interact(
                rect,
                ui.id()
                    .with(("segment", rect.left() as i32, rect.top() as i32)),
                Sense::click(),
            )
            .on_hover_text(*tooltip);
        let active = *value == current;
        let fill = if active {
            SECONDARY
        } else if response.hovered() {
            Color32::from_white_alpha(8)
        } else {
            Color32::TRANSPARENT
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(R_SM), fill);
        let fg = if active || response.hovered() {
            FOREGROUND
        } else {
            MUTED_FG
        };
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, fg);
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if response.clicked() {
            clicked = Some(*value);
        }
    }
    clicked
}

/// Section label: semibold, foreground. Matches shadcn card titles.
pub fn panel_title(ui: &mut Ui, label: &str) {
    ui.label(
        RichText::new(label)
            .family(semibold())
            .size(13.0)
            .color(FOREGROUND),
    );
}

/// Small muted caption under a title or beside a value.
pub fn caption(text: impl Into<String>) -> RichText {
    RichText::new(text).size(11.5).color(MUTED_FG)
}

/// Thin horizontal rule spanning the available width.
pub fn divider(ui: &mut Ui) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0, BORDER);
}

// -- logo ----------------------------------------------------------------------
/// The Eddie mark: three timeline clips forming an E, crossed by a violet
/// playhead whose tip points down. Units are a 1×1 box; the menu bar paints
/// it and the window icon rasterizes the same shapes.
pub const LOGO_CLIP: Color32 = Color32::from_rgb(0xFF, 0xEE, 0xEF);
pub const LOGO_PLAYHEAD: Color32 = Color32::from_rgb(0x8B, 0x5C, 0xF6);
/// Clip capsules as [x, y, width, height].
const MARK_CLIPS: [[f32; 4]; 3] = [
    [0.1027, 0.1724, 0.8477, 0.2126],
    [0.0496, 0.4784, 0.6782, 0.2155],
    [0.1027, 0.7845, 0.8477, 0.2155],
];
/// Playhead line [x, y, width, height] and its downward tip.
const MARK_LINE: [f32; 4] = [0.2967, 0.0632, 0.0187, 0.9368];
const MARK_TIP: [[f32; 2]; 3] = [[0.2364, 0.0], [0.3757, 0.0], [0.3060, 0.0776]];

/// Paint the mark into `rect` (square), the brand slot in the menu bar.
pub fn paint_mark(painter: &egui::Painter, rect: egui::Rect) {
    let at = |x: f32, y: f32| rect.min + Vec2::new(x, y) * rect.width();
    for [x, y, w, h] in MARK_CLIPS {
        let r = egui::Rect::from_min_max(at(x, y), at(x + w, y + h));
        painter.rect_filled(r, CornerRadius::same((r.height() / 2.0) as u8), LOGO_CLIP);
    }
    // At menu-bar size the playhead keeps a visible width and tip.
    let size = rect.width();
    let [x, y, w, h] = MARK_LINE;
    let (center, half) = (at(x + w / 2.0, 0.0).x, (w * size).max(1.6) / 2.0);
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(center - half, at(0.0, y).y),
            egui::pos2(center + half, at(0.0, y + h).y),
        ),
        CornerRadius::ZERO,
        LOGO_PLAYHEAD,
    );
    let tip = (0.0697 * size).max(3.2);
    let top = rect.min.y - (tip * 1.11 - 0.0776 * size).max(0.0);
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(center - tip, top),
            egui::pos2(center + tip, top),
            egui::pos2(center, top + tip * 1.11),
        ],
        LOGO_PLAYHEAD,
        Stroke::NONE,
    ));
}

/// The window icon: the mark on a dark rounded tile, `size`² RGBA,
/// 4×4 supersampled.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let inside_capsule = |px: f32, py: f32, [x, y, w, h]: [f32; 4]| {
        let r = h / 2.0;
        let cx = px.clamp(x + r, x + w - r);
        (px - cx).powi(2) + (py - (y + r)).powi(2) <= r * r
    };
    let inside_rect =
        |px: f32, py: f32, [x, y, w, h]: [f32; 4]| px >= x && px <= x + w && py >= y && py <= y + h;
    let inside_tip = |px: f32, py: f32| {
        let [a, b, c] = MARK_TIP;
        let side =
            |p: [f32; 2], q: [f32; 2]| (q[0] - p[0]) * (py - p[1]) - (q[1] - p[1]) * (px - p[0]);
        let (d1, d2, d3) = (side(a, b), side(b, c), side(c, a));
        !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
    };
    // The mark sits inside the tile with padding; the tile has round corners.
    let (pad, radius) = (0.17, 0.22);
    let tile = SURFACE;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let mut sum = [0.0_f32; 4];
            for sy in 0..4 {
                for sx in 0..4 {
                    let u = (x as f32 + (sx as f32 + 0.5) / 4.0) / size as f32;
                    let v = (y as f32 + (sy as f32 + 0.5) / 4.0) / size as f32;
                    let (cu, cv) = (u.clamp(radius, 1.0 - radius), v.clamp(radius, 1.0 - radius));
                    if (u - cu).powi(2) + (v - cv).powi(2) > radius * radius {
                        continue;
                    }
                    let (mx, my) = ((u - pad) / (1.0 - 2.0 * pad), (v - pad) / (1.0 - 2.0 * pad));
                    let color = if inside_rect(mx, my, MARK_LINE) || inside_tip(mx, my) {
                        LOGO_PLAYHEAD
                    } else if MARK_CLIPS.iter().any(|clip| inside_capsule(mx, my, *clip)) {
                        LOGO_CLIP
                    } else {
                        tile
                    };
                    for (total, channel) in sum.iter_mut().zip(color.to_array()) {
                        *total += f32::from(channel);
                    }
                }
            }
            // Accumulated colour is premultiplied by coverage; unpremultiply.
            let alpha = sum[3] / 16.0;
            for channel in &sum[..3] {
                out.push(if alpha > 0.0 {
                    (channel / sum[3] * 255.0).round() as u8
                } else {
                    0
                });
            }
            out.push(alpha.round() as u8);
        }
    }
    out
}
