//! Captions made from a placed clip's transcript.

use openreel_engine::{ClipId, ClipKind};

use super::sweep;
use crate::Editor;

/// What captioning one clip did with each transcript line in its range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct CaptionPass {
    added: usize,
    /// Already captioned by an earlier pass.
    repeated: usize,
    /// Under half inside the clip: the other side of the cut shows them.
    outside: usize,
}

impl CaptionPass {
    /// One line for the script's notes, so a clip that got no caption is
    /// not mistaken for one that was captioned.
    pub(super) fn note(self, clip: ClipId) -> String {
        let count = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
        let mut parts = vec![match self.added {
            0 => "no captions added".to_owned(),
            added => format!("added {}", count(added, "caption")),
        }];
        if self.repeated > 0 {
            parts.push(format!(
                "{} already captioned",
                count(self.repeated, "line")
            ));
        }
        if self.outside > 0 {
            parts.push(format!(
                "{} left out, under half inside the clip",
                count(self.outside, "line")
            ));
        }
        if self == Self::default() {
            parts.push("no speech in its source range".to_owned());
        }
        format!("clip {clip}: {}", parts.join("; "))
    }
}

impl Editor {
    /// Generate styled caption title clips for the transcript that
    /// overlaps one media clip's source range. Word timings are
    /// honored when the provider supplied them; turn-only chunks
    /// are proportional estimates. Exact duplicates are skipped.
    pub fn add_captions_for_clip(
        &mut self,
        id: ClipId,
        style: openreel_engine::TitleStyle,
    ) -> Result<usize, String> {
        Ok(self.caption_clip(id, style)?.added)
    }

    pub(super) fn caption_clip(
        &mut self,
        id: ClipId,
        style: openreel_engine::TitleStyle,
    ) -> Result<CaptionPass, String> {
        let target = self
            .timeline
            .clip(id)
            .cloned()
            .ok_or_else(|| format!("no clip {id}"))?;
        if target.kind == ClipKind::Title {
            return Err(format!("clip {id} is already a title"));
        }
        let source = target
            .source
            .as_deref()
            .ok_or_else(|| format!("clip {id} has no transcript source"))?;
        let asset = self
            .asset(source)
            .ok_or_else(|| format!("clip {id} source is not in the media bin"))?;
        if asset.sweep.transcript.is_empty() {
            return Err(format!("clip {id} has no transcript to caption"));
        }
        let transcript = asset.sweep.transcript.clone();
        let source_start = target.source_in;
        let source_end = target.source_in + target.duration;
        let frame = 1.0 / self.timeline.fps;
        let mut pass = CaptionPass::default();
        let mut last_added: Option<ClipId> = None;

        for segment in &transcript {
            for chunk in sweep::caption_chunks(segment) {
                if chunk.end <= source_start || chunk.start >= source_end {
                    continue;
                }
                let clipped_start = chunk.start.max(source_start);
                let clipped_end = chunk.end.min(source_end);
                // A line mostly spoken on the other side of a cut belongs
                // there; showing it again here repeats it for a moment.
                if clipped_end - clipped_start < 0.5 * (chunk.end - chunk.start) {
                    pass.outside += 1;
                    continue;
                }
                let start = self
                    .timeline
                    .snap(target.start + (clipped_start - source_start));
                let mut end = self
                    .timeline
                    .snap(target.start + (clipped_end - source_start));
                if end - start < frame {
                    end = (start + frame).min(target.end());
                }
                if end <= start {
                    pass.outside += 1;
                    continue;
                }
                let duplicate = self.timeline.clips.iter().any(|clip| {
                    clip.kind == ClipKind::Title
                        && clip.name == chunk.text
                        && clip.title_style == style
                        && (clip.start - start).abs() <= frame / 2.0
                });
                if duplicate {
                    pass.repeated += 1;
                    continue;
                }
                let duration = end - start;
                let track = self.first_free_track(start, duration);
                let caption = self.timeline.add_clip(
                    &chunk.text,
                    ClipKind::Title,
                    track,
                    start,
                    duration,
                    0.0,
                );
                if let Some(clip) = self.timeline.clip_mut(caption) {
                    clip.title_style = style;
                    // Offsets from the caption's start, so word `i` lights
                    // when its source time plays.
                    clip.words = chunk
                        .words
                        .iter()
                        .map(|word| {
                            (target.start + word - source_start - start).clamp(0.0, duration)
                        })
                        .collect();
                }
                last_added = Some(caption);
                pass.added += 1;
            }
        }
        // A second identical pass is a successful no-op, as is a clip whose
        // source range holds no speech; the counts say which.
        if let Some(caption) = last_added {
            self.selected = Some(caption);
        }
        Ok(pass)
    }
}
