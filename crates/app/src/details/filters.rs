//! The filter tiles in the Colour card: each filter applied to a small
//! copy of the frame the monitor shows, as CapCut lays them out. The copy
//! comes from the monitor's own ungraded frame, so no decoder runs for
//! them; while the selected clip is not on screen the tiles grade a colour
//! swatch instead.

use openreel_engine::{ClipId, ColorAdjust, Filter};
use rune::{Color, Cursor, El, Pixels, SurfaceFit, col, el, row, text};

use crate::app::Ui;
use crate::theme::{self, c};

/// The longest side of the picture the tiles grade, in pixels: a tile is
/// at most about 80 points wide, 160 pixels on a 2x screen.
const MAX_EDGE: usize = 200;

/// The narrowest a tile gets before a row holds one fewer.
const MIN_TILE: f32 = 60.0;

const GAP: f32 = 6.0;

/// The small, ungraded picture the tiles show and one graded copy each.
pub(super) struct Previews {
    /// The selected clip and the monitor frame the picture was taken from;
    /// `None` while it is the swatch.
    taken: Option<(ClipId, u64)>,
    width: u32,
    rgba: Vec<u8>,
    /// The adjustments the tiles were last graded with.
    graded: Option<ColorAdjust>,
    pixels: [Pixels; Filter::ALL.len()],
}

impl Default for Previews {
    fn default() -> Self {
        let (width, rgba) = swatch();
        Self {
            taken: None,
            width,
            rgba,
            graded: None,
            pixels: std::array::from_fn(|_| Pixels::new()),
        }
    }
}

impl Previews {
    /// Grade every tile with `color`'s adjustments and its own filter at
    /// full strength, unless they already are.
    fn grade(&mut self, color: ColorAdjust) {
        let base = ColorAdjust {
            filter: Filter::None,
            strength: 1.0,
            ..color
        };
        if self.graded == Some(base) {
            return;
        }
        self.graded = Some(base);
        let height = (self.rgba.len() / (self.width as usize * 4).max(1)) as u32;
        for (filter, pixels) in Filter::ALL.into_iter().zip(&self.pixels) {
            let mut graded = self.rgba.clone();
            ColorAdjust { filter, ..base }.apply_rgba(&mut graded);
            pixels.set(self.width, height, graded);
        }
    }
}

/// Once per frame: take the monitor's frame when it shows the selected
/// clip and nothing plays, fall back to the swatch for a clip that has
/// never been on screen, then grade the tiles.
pub(super) fn sync(ui: &mut Ui) {
    let Some(id) = ui.details.shown.filter(|_| ui.show_inspector) else {
        return;
    };
    let editor = &ui.editor;
    let Some(color) = editor
        .timeline
        .clip(id)
        .filter(|clip| clip.kind == openreel_engine::ClipKind::Video && clip.source.is_some())
        .map(|clip| clip.color)
    else {
        return;
    };
    let on_screen = !editor.playing
        && editor
            .timeline
            .program_video(editor.playhead)
            .map(|clip| clip.id)
            == Some(id)
        && editor.preview_is_current();
    let previews = &mut ui.details.filters;
    if on_screen {
        let picture = ui.monitor.picture.borrow();
        let (width, rgba, frame) = picture.upright();
        if previews.taken != Some((id, frame)) && !rgba.is_empty() {
            previews.width = shrink(width, rgba, &mut previews.rgba);
            previews.taken = Some((id, frame));
            previews.graded = None;
        }
    } else if previews.taken.is_some_and(|(taken, _)| taken != id) {
        (previews.width, previews.rgba) = swatch();
        previews.taken = None;
        previews.graded = None;
    }
    previews.grade(color);
}

/// `rgba`, `width` pixels wide, box-averaged into `out` so its longer side
/// is at most [`MAX_EDGE`]. Returns the new width.
fn shrink(width: u32, rgba: &[u8], out: &mut Vec<u8>) -> u32 {
    let w = (width as usize).max(1);
    let h = rgba.len() / (w * 4);
    let scale = (MAX_EDGE as f64 / w.max(h) as f64).min(1.0);
    let ow = ((w as f64 * scale).round() as usize).max(1);
    let oh = ((h as f64 * scale).round() as usize).max(1);
    out.clear();
    for oy in 0..oh {
        let y0 = oy * h / oh;
        let y1 = ((oy + 1) * h / oh).max(y0 + 1);
        for ox in 0..ow {
            let x0 = ox * w / ow;
            let x1 = ((ox + 1) * w / ow).max(x0 + 1);
            let mut sum = [0u32; 4];
            for y in y0..y1 {
                let line = &rgba[(y * w + x0) * 4..(y * w + x1) * 4];
                for px in line.as_chunks::<4>().0 {
                    for (s, v) in sum.iter_mut().zip(px) {
                        *s += u32::from(*v);
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            out.extend(sum.map(|s| ((s + n / 2) / n) as u8));
        }
    }
    ow as u32
}

/// A 160x120 swatch: every hue across, paling towards the top, over a
/// grey ramp from black to white, so each filter's tone and contrast show.
fn swatch() -> (u32, Vec<u8>) {
    const W: usize = 160;
    const H: usize = 120;
    let mut rgba = Vec::with_capacity(W * H * 4);
    for y in 0..H {
        for x in 0..W {
            let across = x as f32 / (W - 1) as f32;
            let [r, g, b] = if y >= H * 3 / 4 {
                [across; 3]
            } else {
                let pale = 1.0 - y as f32 / (H * 3 / 4) as f32;
                let hue = across * 6.0;
                let channel = |offset: f32| {
                    let k = (hue + offset).rem_euclid(6.0);
                    1.0 - (k.min(4.0 - k).clamp(0.0, 1.0))
                };
                [channel(5.0), channel(3.0), channel(1.0)].map(|v| v + (1.0 - v) * pale * 0.6)
            };
            rgba.extend([r, g, b].map(|v| (v * 255.0).round() as u8));
            rgba.push(255);
        }
    }
    (W as u32, rgba)
}

/// What each look does, for its tooltip.
fn help(filter: Filter) -> &'static str {
    match filter {
        Filter::None => "No filter",
        Filter::Vivid => "Vivid: richer colour and a little more contrast",
        Filter::Dramatic => "Dramatic: muted colour and strong contrast",
        Filter::Warm => "Warm: an orange cast",
        Filter::Cool => "Cool: a blue cast",
        Filter::Fade => "Fade: lifted blacks, softer contrast, less colour",
        Filter::Vintage => "Vintage: faded and warm, with a brown tone",
        Filter::Sepia => "Sepia: brown-toned black and white",
        Filter::Mono => "Mono: black and white",
        Filter::Noir => "Noir: high-contrast black and white",
    }
}

/// How many tiles fit a row `width` wide, and each tile's size.
pub(super) fn tile_size(width: f32) -> (usize, f32, f32) {
    let cols = ((width + GAP) / (MIN_TILE + GAP)).floor().clamp(3.0, 5.0);
    let w = ((width - GAP * (cols - 1.0)) / cols).floor();
    (cols as usize, w, (w * 0.75).round())
}

/// `tiles` in rows of `cols`.
pub(super) fn tile_grid(key: &str, tiles: Vec<El>, cols: usize) -> El {
    let mut rows = Vec::new();
    let mut tiles = tiles.into_iter().peekable();
    while tiles.peek().is_some() {
        rows.push(
            row()
                .gap(GAP)
                .children(tiles.by_ref().take(cols).collect::<Vec<_>>()),
        );
    }
    col().key(key.to_owned()).w_full().gap(8.0).children(rows)
}

/// The tiles, as many to a row as fit `width`. A press picks a filter at
/// full strength; pressing the current one keeps its strength.
pub(super) fn grid(ui: &Ui, id: ClipId, current: Filter, width: f32) -> El {
    let (cols, w, h) = tile_size(width);
    let tiles = Filter::ALL
        .into_iter()
        .zip(&ui.details.filters.pixels)
        .map(|(filter, pixels)| {
            let picture = el()
                .absolute(0.0, 0.0)
                .size(w, h)
                .pixels_fit(pixels, SurfaceFit::Fill);
            let key = format!("details-filter-{}", filter.as_str());
            tile(
                key,
                filter.label(),
                help(filter),
                picture,
                filter == current,
                (w, h),
            )
            .on_press(move |ui: &mut Ui| {
                ui.editor.edit_clip_color(id, |color| {
                    if color.filter != filter {
                        color.filter = filter;
                        color.strength = 1.0;
                    }
                });
            })
        })
        .collect();
    tile_grid("details-filters", tiles, cols)
}

/// A picture to pick, `picture` drawn inside its frame, with its label
/// under it; the current one is outlined in the selection colour.
pub(super) fn tile(
    key: String,
    label: &'static str,
    help: &'static str,
    picture: El,
    current: bool,
    (w, h): (f32, f32),
) -> El {
    let c = c();
    let layer = || {
        el().absolute(0.0, 0.0)
            .size(w, h)
            .radius(theme::R_SM)
            .inert()
    };
    let picture = el()
        .size(w, h)
        .radius(theme::R_SM)
        .fill(c.secondary)
        .clip_rounded(theme::R_SM)
        .child(picture.inert())
        .child(if current {
            layer().border(2.0, c.brand)
        } else {
            layer()
                .border(1.0, Color::WHITE.alpha(70.0 / 255.0))
                .group_opacity(0.0, 1.0)
        });
    col()
        .key(key)
        .w(w)
        .gap(4.0)
        .items_center()
        .cursor(Cursor::Pointer)
        .hover_group()
        .role("option")
        .label(label)
        .help(help)
        .child(picture)
        .child(
            text(label)
                .font(theme::SMALL)
                .color(if current { c.brand } else { c.muted_fg })
                .single_line()
                .truncate()
                .max_w(w),
        )
}
