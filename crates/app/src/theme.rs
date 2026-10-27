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
pub const BACKGROUND: Color32 = Color32::from_rgb(0x09, 0x09, 0x0B);
pub const FOREGROUND: Color32 = Color32::from_rgb(0xFA, 0xFA, 0xFA);
pub const CARD: Color32 = Color32::from_rgb(0x12, 0x12, 0x15);
pub const PRIMARY: Color32 = Color32::from_rgb(0xFA, 0xFA, 0xFA);
pub const PRIMARY_FG: Color32 = Color32::from_rgb(0x18, 0x18, 0x1B);
pub const SECONDARY: Color32 = Color32::from_rgb(0x27, 0x27, 0x2A);
pub const MUTED_FG: Color32 = Color32::from_rgb(0xA1, 0xA1, 0xAA);
pub const ACCENT: Color32 = Color32::from_rgb(0x27, 0x27, 0x2A);
pub const DESTRUCTIVE: Color32 = Color32::from_rgb(0xEF, 0x44, 0x44);
pub const BORDER: Color32 = Color32::from_rgb(0x27, 0x27, 0x2A);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x3F, 0x3F, 0x46);
pub const INPUT: Color32 = Color32::from_rgb(0x1C, 0x1C, 0x21);
pub const RING: Color32 = Color32::from_rgb(0xA1, 0xA1, 0xAA);

/// Clip fills. Descriptive per-kind colour, kept muted like pro NLEs.
pub const CLIP_VIDEO: Color32 = Color32::from_rgb(0x2E, 0x4A, 0x6B);
pub const CLIP_AUDIO: Color32 = Color32::from_rgb(0x2C, 0x54, 0x3C);
pub const CLIP_TITLE: Color32 = Color32::from_rgb(0x6B, 0x53, 0x18);
pub const PLAYHEAD: Color32 = Color32::from_rgb(0xEF, 0x44, 0x44);

pub const R_SM: u8 = 6;
pub const R_MD: u8 = 8;
pub const R_LG: u8 = 10;

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
            (TextStyle::Body, FontId::proportional(13.5)),
            (TextStyle::Button, FontId::proportional(13.0)),
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Heading, FontId::proportional(17.0)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(12.0, 5.0);
        style.spacing.menu_margin = Margin::same(6);
        style.spacing.indent = 14.0;
        style.spacing.interact_size = Vec2::new(40.0, 30.0);
        style.spacing.slider_width = 140.0;
        style.spacing.scroll.bar_width = 10.0;
    });
}

fn visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = BACKGROUND;
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

    v.widgets.noninteractive.bg_fill = CARD;
    v.widgets.noninteractive.weak_bg_fill = BACKGROUND;
    v.widgets.noninteractive.bg_stroke = stroke(BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, FOREGROUND);
    v.widgets.noninteractive.corner_radius = CornerRadius::same(R_MD);

    // Resting widgets are borderless and nearly flat (shadcn-style).
    // Definition comes from hover and pressed fills, not outlines.
    v.widgets.inactive.bg_fill = Color32::from_white_alpha(13);
    v.widgets.inactive.weak_bg_fill = Color32::from_white_alpha(9);
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, FOREGROUND);
    v.widgets.inactive.corner_radius = CornerRadius::same(R_MD);

    v.widgets.hovered.bg_fill = Color32::from_rgb(0x32, 0x32, 0x39);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x32, 0x32, 0x39);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_white_alpha(20));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, FOREGROUND);
    v.widgets.hovered.corner_radius = CornerRadius::same(R_MD);
    v.widgets.hovered.expansion = 0.5;

    v.widgets.active.bg_fill = Color32::from_rgb(0x42, 0x42, 0x4B);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(0x42, 0x42, 0x4B);
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, Color32::from_white_alpha(34));
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, FOREGROUND);
    v.widgets.active.corner_radius = CornerRadius::same(R_MD);
    v.widgets.active.expansion = 0.5;

    v.widgets.open = v.widgets.hovered;

    v.selection.bg_fill = Color32::from_rgba_unmultiplied(0xFA, 0xFA, 0xFA, 45);
    v.selection.stroke = Stroke::new(1.0_f32, RING);
    v.text_cursor.stroke = Stroke::new(1.5_f32, FOREGROUND);
    v
}

/// Prefer Inter (shadcn's usual sans) when installed; fall back silently to
/// egui's bundled font rather than shipping a font binary in the repo.
fn load_fonts(ctx: &egui::Context) {
    let home = std::env::var("HOME").unwrap_or_default();
    let inter_candidates = [
        "/usr/share/fonts/inter/Inter-Regular.otf".to_owned(),
        "/usr/share/fonts/TTF/Inter-Regular.ttf".to_owned(),
        "/usr/share/fonts/TTF/InterVariable.ttf".to_owned(),
        format!("{home}/.local/share/fonts/Inter-Regular.ttf"),
    ];
    let mut fonts = FontDefinitions::default();
    for path in &inter_candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(
                "Inter".to_owned(),
                std::sync::Arc::new(FontData::from_owned(bytes)),
            );
            if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
                family.insert(0, "Inter".to_owned());
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
}

// -- shadcn-style variants -----------------------------------------------------

/// Hand-painted button so each variant gets distinct rest / hover / pressed
/// fills. egui's `Button::fill` is static across states, which reads flat.
#[allow(clippy::too_many_arguments)]
fn variant_button(
    ui: &mut Ui,
    label: &str,
    fg: Color32,
    rest: Color32,
    hover: Color32,
    pressed: Color32,
    rest_stroke: Stroke,
    hover_stroke: Stroke,
) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), FontId::proportional(13.0), fg);
    let size = galley.size() + Vec2::new(24.0, 11.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let (fill, stroke) = if response.is_pointer_button_down_on() {
        (pressed, hover_stroke)
    } else if response.hovered() {
        (hover, hover_stroke)
    } else {
        (rest, rest_stroke)
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(R_MD),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    painter.galley(rect.left_top() + Vec2::new(12.0, 5.5), galley, fg);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

pub fn primary_button(ui: &mut Ui, label: &str) -> Response {
    variant_button(
        ui,
        label,
        PRIMARY_FG,
        PRIMARY,
        Color32::from_rgb(0xE4, 0xE4, 0xE7),
        Color32::from_rgb(0xD4, 0xD4, 0xD8),
        Stroke::NONE,
        Stroke::NONE,
    )
}

pub fn outline_button(ui: &mut Ui, label: &str) -> Response {
    variant_button(
        ui,
        label,
        FOREGROUND,
        Color32::TRANSPARENT,
        ACCENT,
        Color32::from_rgb(0x32, 0x32, 0x39),
        Stroke::new(1.0_f32, Color32::from_white_alpha(28)),
        Stroke::new(1.0_f32, Color32::from_white_alpha(46)),
    )
}

pub fn ghost_button(ui: &mut Ui, label: &str) -> Response {
    variant_button(
        ui,
        label,
        FOREGROUND,
        Color32::TRANSPARENT,
        ACCENT,
        Color32::from_rgb(0x32, 0x32, 0x39),
        Stroke::NONE,
        Stroke::NONE,
    )
}

pub fn destructive_button(ui: &mut Ui, label: &str) -> Response {
    let red = |a: u8| Color32::from_rgba_unmultiplied(0xEF, 0x44, 0x44, a);
    variant_button(
        ui,
        label,
        FOREGROUND,
        red(45),
        red(80),
        red(120),
        Stroke::new(1.0_f32, red(90)),
        Stroke::new(1.0_f32, red(150)),
    )
}

/// Section label: small, muted. The one place muted-foreground is used at
/// small size, matching shadcn panel headers.
pub fn panel_title(ui: &mut Ui, label: &str) {
    ui.label(RichText::new(label).size(12.0).strong().color(MUTED_FG));
}
