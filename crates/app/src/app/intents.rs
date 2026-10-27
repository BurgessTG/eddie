//! Button intents: packaged actions behind the chat button row.
//!
//! Two kinds, one registry. Local intents are pure app code over
//! sweep results — instant, zero cost, no network. Model intents are
//! a fixed prompt through the same chat loop as typed requests. A
//! new button is a new `INTENTS` entry, not a new pipeline.
//!
//! Local intents still mutate only through `ScriptCommand`s: they
//! build a command batch here and the app applies it through the
//! same `apply_script_commands` path (and undo snapshot) as model
//! JavaScript. No new state-mutation path.

use crate::scripting::ScriptCommand;

use super::sweep::SweepCandidate;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LocalSort {
    ReactionsFirst,
    Steadiest,
    ByScore,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum IntentKind {
    Local(LocalSort),
    Model,
}

pub(crate) struct Intent {
    pub id: &'static str,
    pub label: &'static str,
    pub tooltip: &'static str,
    pub kind: IntentKind,
    /// Fixed prompt for model intents.
    pub prompt: Option<&'static str>,
    /// Progress label while a model intent runs.
    pub progress: &'static str,
}

pub(crate) const INTENTS: &[Intent] = &[
    Intent {
        id: "reactions-first",
        label: "Reactions first",
        tooltip:
            "Rebuild the video timeline from sweep clips, reactions first. Instant, no model call.",
        kind: IntentKind::Local(LocalSort::ReactionsFirst),
        prompt: None,
        progress: "Ranking clips…",
    },
    Intent {
        id: "steadiest-clips",
        label: "Steadiest clips",
        tooltip:
            "Rebuild the video timeline from sweep clips, steadiest first. Instant, no model call.",
        kind: IntentKind::Local(LocalSort::Steadiest),
        prompt: None,
        progress: "Ranking clips…",
    },
    Intent {
        id: "by-score",
        label: "By score",
        tooltip:
            "Rebuild the video timeline from sweep clips in ranked order. Instant, no model call.",
        kind: IntentKind::Local(LocalSort::ByScore),
        prompt: None,
        progress: "Ranking clips…",
    },
    Intent {
        id: "build-the-montage",
        label: "Build the montage",
        tooltip: "Ask the model to build a montage from the sweep clips",
        kind: IntentKind::Model,
        prompt: Some(
            "Build the montage from the sweep: replace the current timeline clips with the \
             highest-ranked video sweep candidates (all candidates only if none has video), \
             placed sequentially from 0.",
        ),
        progress: "Ranking clips…",
    },
    Intent {
        id: "tighten-the-intro",
        label: "Tighten the intro",
        tooltip: "Ask the model to tighten the opening of the timeline",
        kind: IntentKind::Model,
        prompt: Some(
            "Tighten the intro: make the opening of the timeline faster and cleaner. Trim \
             dead air at the starts of the first clips, close any gaps, and keep the rest \
             of the edit intact.",
        ),
        progress: "Writing the edit…",
    },
];

pub(crate) fn intent_by_id(id: &str) -> Option<&'static Intent> {
    INTENTS.iter().find(|intent| intent.id == id)
}

/// Lightweight candidate copy for ordering and command building.
/// No transcript payload, so cloning the whole set is cheap.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CandidateView {
    pub id: String,
    pub asset_path: String,
    pub title: String,
    pub source_in: f64,
    pub duration: f64,
    pub score: f32,
    pub stability: f32,
    pub reaction_score: f32,
    pub is_reaction: bool,
    pub has_video: bool,
}

impl From<&SweepCandidate> for CandidateView {
    fn from(candidate: &SweepCandidate) -> Self {
        Self {
            id: candidate.id.clone(),
            asset_path: candidate.asset_path.clone(),
            title: candidate.title.clone(),
            source_in: candidate.source_in,
            duration: candidate.duration,
            score: candidate.score,
            stability: candidate.stability,
            reaction_score: candidate.reaction_score(),
            is_reaction: candidate.is_reaction(),
            has_video: candidate.has_video,
        }
    }
}

/// Video candidates when any exist (the montage rule), else all.
pub(crate) fn preferred_candidates(candidates: &[CandidateView]) -> Vec<CandidateView> {
    if candidates.iter().any(|c| c.has_video) {
        candidates.iter().filter(|c| c.has_video).cloned().collect()
    } else {
        candidates.to_vec()
    }
}

pub(crate) fn ordered_candidates(
    candidates: &[CandidateView],
    sort: LocalSort,
) -> Vec<CandidateView> {
    let mut ordered = preferred_candidates(candidates);
    match sort {
        LocalSort::ByScore => ordered.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.source_in.total_cmp(&b.source_in))
                .then_with(|| a.id.cmp(&b.id))
        }),
        LocalSort::Steadiest => ordered.sort_by(|a, b| {
            b.stability
                .total_cmp(&a.stability)
                .then_with(|| b.score.total_cmp(&a.score))
                .then_with(|| a.id.cmp(&b.id))
        }),
        LocalSort::ReactionsFirst => ordered.sort_by(|a, b| {
            b.is_reaction
                .cmp(&a.is_reaction)
                .then_with(|| b.reaction_score.total_cmp(&a.reaction_score))
                .then_with(|| b.score.total_cmp(&a.score))
                .then_with(|| a.id.cmp(&b.id))
        }),
    }
    ordered
}

/// Replace the given clips with the ordered candidates laid out
/// sequentially from 0, as one `ScriptCommand` batch.
pub(crate) fn local_commands(
    existing_clip_ids: &[u64],
    ordered: &[CandidateView],
) -> Vec<ScriptCommand> {
    let mut commands: Vec<ScriptCommand> = existing_clip_ids
        .iter()
        .map(|id| ScriptCommand::DeleteClip(*id))
        .collect();
    let mut cursor = 0.0;
    for candidate in ordered {
        commands.push(ScriptCommand::AddAssetRange {
            asset: candidate.asset_path.clone(),
            at: cursor,
            source_in: candidate.source_in,
            duration: candidate.duration,
        });
        cursor += candidate.duration;
    }
    commands.push(ScriptCommand::Log(format!(
        "Done — arranged {} sweep clip{} (no model call).",
        ordered.len(),
        if ordered.len() == 1 { "" } else { "s" }
    )));
    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(id: &str, score: f32, stability: f32, reaction: f32, has_video: bool) -> CandidateView {
        CandidateView {
            id: id.to_owned(),
            asset_path: "/m/rushes.mp4".to_owned(),
            title: id.to_owned(),
            source_in: 0.0,
            duration: 4.0,
            score,
            stability,
            reaction_score: reaction,
            is_reaction: reaction >= 0.45,
            has_video,
        }
    }

    #[test]
    fn registry_has_both_kinds_with_unique_ids() {
        assert!(INTENTS
            .iter()
            .any(|i| matches!(i.kind, IntentKind::Local(_))));
        assert!(INTENTS.iter().any(|i| i.kind == IntentKind::Model));
        for intent in INTENTS {
            assert_eq!(intent_by_id(intent.id).map(|i| i.id), Some(intent.id));
            if intent.kind == IntentKind::Model {
                assert!(intent.prompt.is_some());
            } else {
                assert!(intent.prompt.is_none());
            }
        }
        let mut ids: Vec<&str> = INTENTS.iter().map(|i| i.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), INTENTS.len());
    }

    #[test]
    fn local_orderings_are_deterministic() {
        let candidates = vec![
            view("c-steady-calm", 70.0, 0.95, 0.1, true),
            view("c-top", 95.0, 0.5, 0.2, true),
            view("c-reaction", 60.0, 0.4, 0.9, true),
            view("c-audio-only", 99.0, 0.99, 0.99, false),
        ];
        fn ids(ordered: &[CandidateView]) -> Vec<&str> {
            ordered.iter().map(|c| c.id.as_str()).collect()
        }
        // Audio-only drops out whenever video candidates exist.
        assert_eq!(
            ids(&ordered_candidates(&candidates, LocalSort::ByScore)),
            vec!["c-top", "c-steady-calm", "c-reaction"]
        );
        assert_eq!(
            ids(&ordered_candidates(&candidates, LocalSort::Steadiest)),
            vec!["c-steady-calm", "c-top", "c-reaction"]
        );
        assert_eq!(
            ids(&ordered_candidates(&candidates, LocalSort::ReactionsFirst)),
            vec!["c-reaction", "c-top", "c-steady-calm"]
        );
    }

    #[test]
    fn local_commands_replace_and_sequence_from_zero() {
        let mut first = view("one", 90.0, 0.9, 0.1, true);
        first.source_in = 12.0;
        first.duration = 4.0;
        let mut second = view("two", 80.0, 0.8, 0.1, true);
        second.source_in = 40.0;
        second.duration = 5.0;
        let commands = local_commands(&[7, 9], &[first, second]);
        assert_eq!(
            commands,
            vec![
                ScriptCommand::DeleteClip(7),
                ScriptCommand::DeleteClip(9),
                ScriptCommand::AddAssetRange {
                    asset: "/m/rushes.mp4".to_owned(),
                    at: 0.0,
                    source_in: 12.0,
                    duration: 4.0,
                },
                ScriptCommand::AddAssetRange {
                    asset: "/m/rushes.mp4".to_owned(),
                    at: 4.0,
                    source_in: 40.0,
                    duration: 5.0,
                },
                ScriptCommand::Log("Done — arranged 2 sweep clips (no model call).".to_owned()),
            ]
        );
    }
}
