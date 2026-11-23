//! Shapes and stickers on the timeline. A graphic clip is a video clip
//! whose picture Eddie draws (`openreel_engine::Graphic`): its drawing is a
//! PNG in the graphics folder, named by what it draws, and the clip plays,
//! exports and composites it as a photo. The drawings are kept apart from
//! the media bin in `Editor::made`, so they never show there, in the
//! project's media list or in `state().assets`; a project only needs each
//! clip's `graphic` to draw them again.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use openreel_engine::{format_timecode, ClipId, ClipKind, Graphic, Shape};
use openreel_media::{draw_graphic, graphic_label, write_graphic, write_graphic_draft};

use super::media::{AssetState, BinAsset, PHOTO_SECONDS};
use super::sweep::SweepData;
use super::{Editor, PREVIEW_W};

/// A new graphic's size against the canvas it fits: a third of it.
pub const GRAPHIC_SCALE: f64 = 0.35;
/// How long a new graphic lasts, as a photo does.
pub const GRAPHIC_SECONDS: f64 = PHOTO_SECONDS;
/// The long side of a graphic's filmstrip picture, in pixels.
const THUMB_SIDE: u32 = 160;
/// The most new drawings one script edit may make. Drawing does not count
/// against the edit's time budget (a 2160-pixel drawing takes 10 to 40 ms
/// in release), so this bounds how long an edit can hold the window.
pub const MAX_NEW_GRAPHICS: usize = 16;

/// `graphics/` beside the settings file.
pub fn default_dir() -> Option<PathBuf> {
    Some(super::settings::config_path()?.parent()?.join("graphics"))
}

/// How long a drawing nobody used stays in the graphics folder. Each look
/// a shape takes in Details is a file of its own; a project draws any it
/// misses again when it opens.
pub const KEEP_UNUSED: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Delete what in `folder`, one of Eddie's own (drawings, remade clips),
/// went unused for longer than `keep`, and anything a crash left half
/// written there.
pub fn prune(folder: &Path, keep: Duration) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let unused = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > keep);
        if unused {
            // A remade file's scratch folder, left by a crash.
            let _ = match entry.file_type() {
                Ok(kind) if kind.is_dir() => std::fs::remove_dir_all(path),
                _ => std::fs::remove_file(path),
            };
        }
    }
}

impl Editor {
    /// Where drawings are kept. An editor without the user's config (a
    /// test) keeps them in the system's temporary folder.
    fn graphics_folder(&self) -> PathBuf {
        self.graphics_dir
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("eddie-graphics"))
    }

    /// The file `graphic` is drawn in, drawn now if it is not there yet,
    /// and known to `Editor::asset` from then on. A draft is drawn only as
    /// wide as the monitor's picture and replaced when the pointer comes
    /// up (`finish_graphic_drafts`).
    fn graphic_file(&mut self, graphic: &Graphic, draft: bool) -> Result<String, String> {
        let started = Instant::now();
        let folder = self.graphics_folder();
        let file = if draft {
            write_graphic_draft(graphic, &folder, PREVIEW_W)?
        } else {
            write_graphic(graphic, &folder)?
        };
        if file.drawn {
            self.drawing.0 += 1;
            self.drawing.1 += started.elapsed();
        }
        let path = file.info.path.clone();
        if draft && !self.graphic_drafts.contains(&path) {
            self.graphic_drafts.push(path.clone());
        }
        if !self.made.iter().any(|known| known.info.path == path) {
            let thumbs = if self.make_thumbnails {
                vec![(0.0, draw_graphic(graphic, THUMB_SIDE)?)]
            } else {
                Vec::new()
            };
            self.made.push(BinAsset {
                size_bytes: std::fs::metadata(&file.path).map_or(0, |meta| meta.len()),
                info: file.info,
                state: AssetState::Ready,
                peaks: Vec::new(),
                beats: None,
                thumbs,
                sweep: SweepData::default(),
            });
        }
        Ok(path)
    }

    /// Place `graphic` at `at` for `duration`, a third of the canvas in
    /// size, and select it: on `row` when that is a video lane (a drop),
    /// else on the lane right above the video there.
    pub fn add_graphic(
        &mut self,
        graphic: Graphic,
        row: Option<usize>,
        at: f64,
        duration: f64,
    ) -> Result<ClipId, String> {
        if !at.is_finite() || at < 0.0 || !duration.is_finite() || duration <= 0.0 {
            return Err("a graphic needs a time from 0 on and a positive length".to_owned());
        }
        let path = self.graphic_file(&graphic, false)?;
        let name = graphic_label(&graphic);
        let video_row =
            row.filter(|&row| matches!(self.timeline.lane_at_row(row), Some((ClipKind::Video, _))));
        let row = video_row.unwrap_or_else(|| {
            let track = self.timeline.overlay_lane(at, at + duration);
            self.timeline.row(ClipKind::Video, track)
        });
        let id =
            self.try_add_asset_range_to_timeline(&path, &name, Some(row), at, 0.0, duration)?;
        if let Some(clip) = self.timeline.clip_mut(id) {
            clip.graphic = Some(graphic);
            clip.transform.scale = GRAPHIC_SCALE;
        }
        self.status = format!("Added {name} at {}", format_timecode(at, self.timeline.fps));
        Ok(id)
    }

    /// Draw clip `id` as `graphic` instead. A clip still called what it
    /// drew takes the new drawing's name.
    pub fn set_graphic(&mut self, id: ClipId, graphic: Graphic) -> Result<(), String> {
        self.redraw_clip(id, graphic, false)
    }

    /// `set_graphic`, drawn as a draft when `draft`.
    fn redraw_clip(&mut self, id: ClipId, graphic: Graphic, draft: bool) -> Result<(), String> {
        let old = match self.timeline.clip(id) {
            Some(clip) => clip.graphic.clone(),
            None => return Err(format!("no clip {id}")),
        };
        let Some(old) = old else {
            return Err(format!("clip {id} is not a shape or sticker"));
        };
        let path = self.graphic_file(&graphic, draft)?;
        let (old_name, name) = (graphic_label(&old), graphic_label(&graphic));
        if let Some(clip) = self.timeline.clip_mut(id) {
            clip.source = Some(path);
            if clip.name == old_name {
                clip.name = name;
            }
            clip.graphic = Some(graphic);
        }
        Ok(())
    }

    /// The Shape card: change a shape clip's shape, each number moved into
    /// its range. A shape left with neither fill nor outline is refused.
    /// While the pointer is held (a slider moving) it is drawn as a draft.
    pub fn edit_clip_shape(&mut self, id: ClipId, edit: impl FnOnce(&mut Shape)) {
        let Some(Some(Graphic::Shape(current))) =
            self.timeline.clip(id).map(|clip| clip.graphic.clone())
        else {
            return;
        };
        let mut shape = current;
        edit(&mut shape);
        let shape = shape.clamped();
        if shape == current {
            return;
        }
        if !shape.is_valid() {
            self.status = "A shape needs a fill or an outline.".to_owned();
            return;
        }
        if let Err(error) = self.redraw_clip(id, Graphic::Shape(shape), self.pointer_held) {
            self.status = error;
        }
    }

    /// The pointer is up: draw every clip shown from a draft in full and
    /// delete the drafts no clip shows any more.
    pub(crate) fn finish_graphic_drafts(&mut self) {
        let drafts = std::mem::take(&mut self.graphic_drafts);
        self.redraw_graphics(|source| drafts.iter().any(|draft| draft == source));
        for draft in drafts {
            let shown = self
                .timeline
                .clips
                .iter()
                .any(|clip| clip.source.as_deref() == Some(&draft));
            if !shown {
                let _ = std::fs::remove_file(&draft);
                self.made.retain(|asset| asset.info.path != draft);
            }
        }
    }

    /// Point every graphic clip at its drawing, drawing it again where the
    /// file is gone or the project came from another computer. A graphic
    /// this version cannot draw keeps its path and shows as missing.
    pub(crate) fn restore_graphics(&mut self) {
        self.redraw_graphics(|_| true);
    }

    /// Point each graphic clip whose source `redraw` picks at its full
    /// drawing.
    fn redraw_graphics(&mut self, redraw: impl Fn(&str) -> bool) {
        let wanted: Vec<(usize, Graphic)> = self
            .timeline
            .clips
            .iter()
            .enumerate()
            .filter(|(_, clip)| redraw(clip.source.as_deref().unwrap_or_default()))
            .filter_map(|(index, clip)| Some((index, clip.graphic.clone()?)))
            .collect();
        for (index, graphic) in wanted {
            if let Ok(path) = self.graphic_file(&graphic, false) {
                self.timeline.clips[index].source = Some(path);
            }
        }
    }
}
