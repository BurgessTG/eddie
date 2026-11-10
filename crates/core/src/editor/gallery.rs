//! Browsing curated ranges never changes the source inventory or timeline.

use std::cmp::Ordering;

use super::sweep::{ReviewStatus, SweepCandidate};
use super::Editor;

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum GalleryView {
    #[default]
    Clips,
    Scenes,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GalleryQuality {
    #[default]
    All,
    Prime,
    Reviewed,
    NeedsReview,
    Local,
}

impl GalleryQuality {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "Any quality",
            Self::Prime => "Prime",
            Self::Reviewed => "Reviewed",
            Self::NeedsReview => "Needs review",
            Self::Local => "Local only",
        }
    }

    pub fn matches(self, clip: &SweepCandidate) -> bool {
        match self {
            Self::All => true,
            Self::Prime => clip.is_prime(),
            Self::Reviewed => clip.review.status == ReviewStatus::Reviewed,
            Self::NeedsReview => clip.review.status == ReviewStatus::NeedsReview,
            Self::Local => clip.review.status == ReviewStatus::Local,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum GalleryDuration {
    #[default]
    All,
    UnderTwo,
    TwoToEight,
    EightToThirty,
    ThirtyPlus,
}

impl GalleryDuration {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "Any duration",
            Self::UnderTwo => "Under 2 s",
            Self::TwoToEight => "2 to 8 s",
            Self::EightToThirty => "8 to 30 s",
            Self::ThirtyPlus => "30+ s",
        }
    }

    pub fn matches(self, duration: f64) -> bool {
        match self {
            Self::All => true,
            Self::UnderTwo => duration < 2.0,
            Self::TwoToEight => (2.0..8.0).contains(&duration),
            Self::EightToThirty => (8.0..30.0).contains(&duration),
            Self::ThirtyPlus => duration >= 30.0,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum GallerySort {
    #[default]
    Score,
    Source,
    Shortest,
    Longest,
}

impl GallerySort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Score => "Best first",
            Self::Source => "Source order",
            Self::Shortest => "Shortest first",
            Self::Longest => "Longest first",
        }
    }

    pub fn compare(self, a: &SweepCandidate, b: &SweepCandidate) -> Ordering {
        match self {
            // Usable picture first: burned-in text and unreviewed ranges
            // sink below it however high the model scored them.
            Self::Score => {
                let rank = |clip: &SweepCandidate| {
                    if matches!(
                        clip.review.status,
                        ReviewStatus::NeedsReview | ReviewStatus::Rejected
                    ) {
                        2
                    } else if clip.tags.iter().any(|tag| tag == "on-screen-text") {
                        1
                    } else {
                        0
                    }
                };
                rank(a)
                    .cmp(&rank(b))
                    .then_with(|| b.score.total_cmp(&a.score))
            }
            Self::Source => Ordering::Equal,
            Self::Shortest => a.duration.total_cmp(&b.duration),
            Self::Longest => b.duration.total_cmp(&a.duration),
        }
    }
}

#[derive(Default, PartialEq, Eq, Hash)]
pub struct GalleryFilters {
    pub search: String,
    pub category: Option<String>,
    pub quality: GalleryQuality,
    pub duration: GalleryDuration,
    pub sort: GallerySort,
}

impl GalleryFilters {
    pub fn matches(&self, clip: &SweepCandidate, words: &[&str]) -> bool {
        if self
            .category
            .as_ref()
            .is_some_and(|category| *category != clip.category)
            || !self.quality.matches(clip)
            || !self.duration.matches(clip.duration)
        {
            return false;
        }
        words.iter().all(|word| {
            [&clip.asset_name, &clip.title, &clip.category, &clip.reason]
                .into_iter()
                .chain(clip.tags.iter())
                .chain(clip.transcript.iter().map(|turn| &turn.text))
                .any(|text| text.to_lowercase().contains(word))
        })
    }
}

#[derive(Default)]
pub struct CuratorGallery {
    pub open: bool,
    pub view: GalleryView,
    pub filters: GalleryFilters,
}

impl Editor {
    /// The quality choices worth offering. Only curations saved by the
    /// removed model reviewer carry a review status, so those choices
    /// appear only while the bin holds such a range.
    pub fn gallery_quality_choices(&self) -> &'static [GalleryQuality] {
        use GalleryQuality::{All, Local, NeedsReview, Prime, Reviewed};
        let reviewed = self
            .assets
            .iter()
            .flat_map(|asset| &asset.sweep.candidates)
            .any(|clip| clip.review.status != ReviewStatus::Local);
        if reviewed {
            &[All, Prime, Reviewed, NeedsReview, Local]
        } else {
            &[All, Prime]
        }
    }

    pub fn open_curator_gallery(&mut self) {
        self.set_playing(false);
        self.curator.open = true;
    }

    pub fn curator_candidate_refs(&self, scenes: bool) -> Vec<(usize, usize)> {
        let mut refs = if scenes {
            self.visible_scene_clips()
        } else {
            self.visible_sweep_candidates()
        };
        let query = self.curator.filters.search.to_lowercase();
        let words: Vec<_> = query.split_whitespace().collect();
        refs.retain(|(asset, clip)| {
            let asset = &self.assets[*asset];
            if self
                .selected_asset
                .as_ref()
                .is_some_and(|path| *path != asset.info.path)
            {
                return false;
            }
            scenes
                || self
                    .curator
                    .filters
                    .matches(&asset.sweep.candidates[*clip], &words)
        });
        refs.sort_by(|(a_asset, a), (b_asset, b)| {
            let (a, b) = if scenes {
                (
                    &self.assets[*a_asset].sweep.scene_clips[*a],
                    &self.assets[*b_asset].sweep.scene_clips[*b],
                )
            } else {
                (
                    &self.assets[*a_asset].sweep.candidates[*a],
                    &self.assets[*b_asset].sweep.candidates[*b],
                )
            };
            let order = if scenes {
                Ordering::Equal
            } else {
                self.curator.filters.sort.compare(a, b)
            };
            order
                .then_with(|| a_asset.cmp(b_asset))
                .then_with(|| a.source_in.total_cmp(&b.source_in))
                .then_with(|| a.id.cmp(&b.id))
        });
        refs
    }
}

/// `M:SS.s` source position for card captions.
pub fn clock(seconds: f64) -> String {
    let tenths = (seconds.max(0.0) * 10.0).round() as u64;
    format!("{}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
}

pub fn category_label(category: &str) -> &str {
    match category {
        "all" => "All types",
        "reaction" => "Reaction",
        "talking" => "Talking",
        "action" => "Action",
        "broll" => "B-roll",
        "establishing" => "Establishing",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(score: f32, status: ReviewStatus, tags: &[&str]) -> SweepCandidate {
        SweepCandidate {
            id: format!("{score}"),
            asset_path: "a.mp4".to_owned(),
            asset_name: "a.mp4".to_owned(),
            title: "a.mp4".to_owned(),
            source_in: 0.0,
            duration: 1.0,
            score,
            reason: String::new(),
            stability: 0.9,
            audio_energy: 0.1,
            audio_peak: 0.1,
            category: "broll".to_owned(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            motion_score: 0.3,
            audio_score: 0.1,
            music_bed: false,
            visual: None,
            has_video: true,
            transcript: Vec::new(),
            review: super::super::sweep::ClipReview {
                status,
                ..Default::default()
            },
            dismissed: false,
        }
    }

    #[test]
    fn best_first_puts_usable_picture_above_text_and_unreviewed_ranges() {
        // The highest scores belong to the ranges that must sink.
        let mut clips = vec![
            clip(100.0, ReviewStatus::NeedsReview, &[]),
            clip(95.0, ReviewStatus::Reviewed, &["on-screen-text"]),
            clip(60.0, ReviewStatus::Reviewed, &[]),
            clip(70.0, ReviewStatus::Local, &[]),
        ];
        clips.sort_by(|a, b| GallerySort::Score.compare(a, b));
        let order: Vec<_> = clips.iter().map(|c| c.score).collect();
        assert_eq!(order, vec![70.0, 60.0, 95.0, 100.0]);
    }
}
