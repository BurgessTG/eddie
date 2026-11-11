//! Tighten silence: cut the pauses out of a spoken clip.

use openreel_engine::{Clip, ClipId};
use openreel_media::TimeRange;

use crate::Editor;

impl Editor {
    /// Remove dead air from one speech clip using its imported
    /// waveform peaks. Kept phrases become contiguous clips; all
    /// material after the original clip ripples left once. A linked
    /// detach partner is rebuilt in matching linked phrases.
    ///
    /// This is waveform DSP only. It can find pauses accurately,
    /// but it cannot know that a sound is “um” or another filler;
    /// transcript evidence supplies that meaning.
    pub fn tighten_clip_silence(
        &mut self,
        id: ClipId,
        threshold: Option<f32>,
        min_silence: f64,
        padding: f64,
    ) -> Result<usize, String> {
        let target = self
            .timeline
            .clip(id)
            .cloned()
            .ok_or_else(|| format!("no clip {id}"))?;
        let kept = self.kept_phrases(&target, threshold, min_silence, padding)?;
        let frame = 1.0 / self.timeline.fps;
        let kept_total: f64 = kept.iter().map(|range| range.duration()).sum();
        let removed = (target.duration - kept_total).max(0.0);
        if removed <= frame / 2.0 {
            return Ok(1);
        }

        let templates = self.cut_together(&target)?;
        let original_end = target.end();
        let followers: Vec<ClipId> = self
            .timeline
            .clips
            .iter()
            .filter(|clip| {
                templates.iter().all(|template| template.id != clip.id)
                    && clip.start >= original_end - f64::EPSILON
            })
            .map(|clip| clip.id)
            .collect();

        self.stop_sources();
        let mut cursor = target.start;
        let last = kept.len() - 1;
        for (index, range) in kept.iter().enumerate() {
            let place = Place {
                at: cursor,
                first: index == 0,
                last: index == last,
            };
            let pieces: Vec<ClipId> = templates
                .iter()
                .map(|template| self.place_phrase(template, &target, range, place))
                .collect();
            if let [left, right] = pieces[..] {
                self.link(left, right);
                self.link(right, left);
            }
            cursor = self.timeline.snap(cursor + range.duration());
        }

        for follower in followers {
            let Some(start) = self.timeline.clip(follower).map(|clip| clip.start) else {
                continue;
            };
            let shifted = self.timeline.snap((start - removed).max(0.0));
            if let Some(clip) = self.timeline.clip_mut(follower) {
                clip.start = shifted;
            }
        }
        // The first phrase is the clip itself.
        self.selected = Some(id);
        Ok(kept.len())
    }

    /// The phrases of `target` worth keeping: source ranges on the project
    /// frame grid, in order, never overlapping, each a frame or longer.
    fn kept_phrases(
        &self,
        target: &Clip,
        threshold: Option<f32>,
        min_silence: f64,
        padding: f64,
    ) -> Result<Vec<TimeRange>, String> {
        let id = target.id;
        let source = target
            .source
            .as_deref()
            .ok_or_else(|| format!("clip {id} has no waveform audio"))?;
        let asset = self
            .asset(source)
            .ok_or_else(|| format!("clip {id} source is not in the media bin"))?;
        if !asset.info.has_audio || asset.peaks.is_empty() || asset.info.duration <= 0.0 {
            return Err(format!("clip {id} has no analyzed waveform"));
        }

        let source_start = target.source_in;
        let source_end = target.source_in + target.duration;
        let bed_overlap = asset.sweep.music_bed_overlap(source_start, source_end);
        if target.duration > 0.0 && bed_overlap >= target.duration * 0.5 {
            return Err(format!(
                "clip {id} is mostly baked-in music; silence tightening is for speech"
            ));
        }

        let ranges = openreel_media::audible_ranges(
            &asset.peaks,
            openreel_media::WAVEFORM_PEAKS_PER_SEC as f64,
            source_start,
            source_end,
            threshold.unwrap_or_else(|| openreel_media::silence_threshold(&asset.peaks)),
            min_silence,
            padding,
        );
        if ranges.is_empty() {
            return Err(format!("clip {id} has no waveform above the threshold"));
        }

        let frame = 1.0 / self.timeline.fps;
        let mut kept: Vec<TimeRange> = Vec::new();
        for range in ranges {
            let mut start = self.timeline.snap(range.start);
            let end = self.timeline.snap(range.end);
            if let Some(previous) = kept.last() {
                start = start.max(previous.end);
            }
            if end - start >= frame {
                kept.push(TimeRange { start, end });
            }
        }
        if kept.is_empty() {
            return Err(format!("clip {id} has no phrase long enough to keep"));
        }
        Ok(kept)
    }

    /// `target`, then its linked partner when it has one: the clips that
    /// are cut into the same phrases.
    fn cut_together(&self, target: &Clip) -> Result<Vec<Clip>, String> {
        let mut templates = vec![target.clone()];
        if let Some(companion) = target
            .linked_clip
            .and_then(|linked| self.timeline.clip(linked))
        {
            if companion.source.as_deref() != target.source.as_deref() {
                return Err(format!(
                    "clip {} is linked to another source; unlink it before tightening",
                    target.id
                ));
            }
            templates.push(companion.clone());
        }
        Ok(templates)
    }

    /// One phrase of one template, keeping the template's offsets from
    /// `target`. The first phrase is the template's own clip; each later one
    /// is a new clip with the template's settings. As a split does, the fade
    /// in stays on the first phrase and the fade out moves to the last.
    fn place_phrase(
        &mut self,
        template: &Clip,
        target: &Clip,
        range: &TimeRange,
        place: Place,
    ) -> ClipId {
        let timeline_offset = template.start - target.start;
        let source_offset = template.source_in - target.source_in;
        let start = self.timeline.snap(place.at + timeline_offset);
        let source_in = (range.start + source_offset).max(0.0);
        if place.first {
            if let Some(clip) = self.timeline.clip_mut(template.id) {
                clip.start = start;
                clip.duration = range.duration();
                clip.source_in = source_in;
                if !place.last {
                    clip.fade_out = 0.0;
                }
            }
            return template.id;
        }
        let new_id = match template.source.as_deref() {
            Some(path) => self.timeline.add_media_clip(
                &template.name,
                template.kind,
                template.track,
                start,
                range.duration(),
                source_in,
                path,
            ),
            None => self.timeline.add_clip(
                &template.name,
                template.kind,
                template.track,
                start,
                range.duration(),
                source_in,
            ),
        };
        if let Some(clip) = self.timeline.clip_mut(new_id) {
            clip.opacity = template.opacity;
            clip.volume = template.volume;
            clip.embedded_audio = template.embedded_audio;
            clip.duck = template.duck;
            clip.clean_voice = template.clean_voice;
            clip.smooth_fades = template.smooth_fades;
            if place.last {
                clip.fade_out = template.fade_out;
            }
        }
        new_id
    }

    fn link(&mut self, clip: ClipId, partner: ClipId) {
        if let Some(clip) = self.timeline.clip_mut(clip) {
            clip.linked_clip = Some(partner);
        }
    }
}

/// Where one kept phrase lands: its timeline start, and whether it is the
/// first or the last phrase.
#[derive(Clone, Copy)]
struct Place {
    at: f64,
    first: bool,
    last: bool,
}
