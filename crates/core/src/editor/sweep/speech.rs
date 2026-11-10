//! Utterances: where a transcript's speech can be cut without ending a
//! thought mid-sentence. Scripts read them as `utterances(asset)` and
//! widen a source range out of them with `snapToSpeech`.
//!
//! These rules came from the removed model curation's talking-edge repair,
//! where each one fixed a live mistake (noted on the tests).

use super::{ends_sentence, TranscriptSegment};

/// Pause across which a sentence without a terminal mark still continues.
pub(super) const CONTINUATION_SECONDS: f64 = 0.8;

/// Words that leave a sentence hanging when a turn ends on them.
const DANGLING_WORDS: &[&str] = &[
    "a", "an", "and", "because", "but", "from", "if", "into", "of", "or", "than", "the", "to",
    "uh", "um", "while", "with",
];

/// One utterance: speech that should not be cut inside.
#[derive(Clone, Debug, PartialEq)]
pub struct Utterance {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// A turn, or one sentence of a turn with heard word timing.
struct Piece<'a> {
    start: f64,
    end: f64,
    text: String,
    speaker: Option<&'a str>,
    /// Starts at a heard sentence end inside its turn.
    sentence_break: bool,
}

/// A short acknowledgement from another speaker ("Uh-huh.", "Really?") does
/// not finish the main speaker's thought.
fn backchannel(piece: &Piece<'_>, main: Option<&str>) -> bool {
    piece.end - piece.start <= 1.2
        && piece.text.split_whitespace().count() <= 3
        && piece.speaker.zip(main).is_some_and(|(a, b)| a != b)
}

/// Split a turn at sentence ends whose last word and the next word were both
/// heard by alignment. Estimated word times never become cut points, and the
/// pieces keep the turn's outer extent.
fn pieces(turn: &TranscriptSegment) -> Vec<Piece<'_>> {
    let speaker = turn.speaker.as_deref();
    let words = &turn.words;
    let joined = |range: &[super::TranscriptWord]| {
        range
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut out = Vec::new();
    let mut first = 0;
    for k in 0..words.len().saturating_sub(1) {
        let (word, next) = (&words[k], &words[k + 1]);
        if ends_sentence(&word.text)
            && !word.estimated
            && !next.estimated
            && next.start >= word.end - 1e-9
        {
            out.push(Piece {
                start: if first == 0 {
                    turn.start
                } else {
                    words[first].start
                },
                end: word.end,
                text: joined(&words[first..=k]),
                speaker,
                sentence_break: first > 0,
            });
            first = k + 1;
        }
    }
    if first == 0 {
        return vec![Piece {
            start: turn.start,
            end: turn.end,
            text: turn.text.trim().to_owned(),
            speaker,
            sentence_break: false,
        }];
    }
    out.push(Piece {
        start: words[first].start,
        end: turn.end,
        text: joined(&words[first..]),
        speaker,
        sentence_break: true,
    });
    out
}

/// Whether a turn ends its thought. A terminal mark ends it; a trailing comma,
/// ellipsis or dangling word does not. Punctuated text without a terminal
/// mark is unfinished, but ASR sometimes stops punctuating altogether, and
/// then only a dangling word is evidence that the thought continues.
fn turn_complete(text: &str) -> bool {
    if ends_sentence(text) {
        return true;
    }
    let text = text
        .trim_end()
        .trim_end_matches(['"', '\'', ')', ']', '\u{201d}', '\u{2019}']);
    if text.ends_with("...") || text.ends_with('\u{2026}') {
        return false;
    }
    let last = text
        .rsplit(char::is_whitespace)
        .next()
        .unwrap_or_default()
        .to_lowercase();
    !text.contains(['.', ',', '?', '!', ';', ':', '\u{3002}', '\u{ff0c}'])
        && !DANGLING_WORDS.contains(&last.as_str())
}

/// Overlapping turns and one speaker's touching turns are one utterance. An
/// unfinished thought also joins a turn that follows within
/// `CONTINUATION_SECONDS`, even across a speaker change, and another speaker's
/// backchannel neither ends nor completes it. A finished thought ends at a
/// speaker change, so a handover without a pause is a cut point and
/// unpunctuated conversation does not chain into one long utterance. A heard
/// sentence end inside a turn is a cut point too. Sparse ambience turns
/// ("[Music]", crowd noise) are not speech.
pub fn utterances(transcript: &[TranscriptSegment]) -> Vec<Utterance> {
    let mut turns: Vec<_> = transcript
        .iter()
        .filter(|turn| turn.end > turn.start && !turn.is_ambient())
        .flat_map(pieces)
        .collect();
    turns.sort_by(|a, b| a.start.total_cmp(&b.start));
    struct Group<'a> {
        utterance: Utterance,
        complete: bool,
        speaker: Option<&'a str>,
    }
    let mut groups: Vec<Group<'_>> = Vec::new();
    for turn in turns {
        let complete = turn_complete(&turn.text);
        match groups.last_mut() {
            Some(last)
                if turn.start < last.utterance.end - 1e-9
                    || (turn.start <= last.utterance.end + 1e-9
                        && !turn.sentence_break
                        && (turn.speaker.is_none() || turn.speaker == last.speaker))
                    || (!last.complete
                        && turn.start < last.utterance.end + CONTINUATION_SECONDS) =>
            {
                last.utterance.end = last.utterance.end.max(turn.end);
                if !turn.text.is_empty() {
                    if !last.utterance.text.is_empty() {
                        last.utterance.text.push(' ');
                    }
                    last.utterance.text.push_str(&turn.text);
                }
                if !backchannel(&turn, last.speaker) {
                    last.complete = complete;
                    last.speaker = turn.speaker;
                }
            }
            _ => groups.push(Group {
                utterance: Utterance {
                    start: turn.start,
                    end: turn.end,
                    text: turn.text,
                },
                complete,
                speaker: turn.speaker,
            }),
        }
    }
    groups.into_iter().map(|group| group.utterance).collect()
}

#[cfg(test)]
mod tests {
    use super::super::TranscriptWord;
    use super::*;

    fn turn(start: f64, end: f64, speaker: &str, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            start,
            end,
            speaker: Some(speaker.to_owned()),
            text: text.to_owned(),
            words: Vec::new(),
            session: None,
        }
    }

    fn spans(transcript: &[TranscriptSegment]) -> Vec<(f64, f64)> {
        utterances(transcript)
            .into_iter()
            .map(|u| (u.start, u.end))
            .collect()
    }

    #[test]
    fn an_unfinished_thought_continues_across_a_quick_speaker_change() {
        // Host long take: "...not sure why but" and the next speaker 80 ms
        // later. A cut there ended the clip mid-thought.
        let transcript = [
            turn(
                0.5,
                3.86,
                "B",
                "they said all the different Doctors were represented not sure why but",
            ),
            turn(
                3.94,
                5.5,
                "A",
                "so so our theme this weekend it's Vikings weekend then",
            ),
        ];
        assert_eq!(spans(&transcript), [(0.5, 5.5)]);
    }

    #[test]
    fn an_unpunctuated_handover_after_a_finished_thought_is_a_cut_point() {
        // ASR stopped punctuating and every touching handover chained 19 s to
        // 63 s into one utterance. The dangling "and" still continues.
        let transcript = [
            turn(0.5, 2.5, "B", "just look at how big that is"),
            turn(2.5, 4.0, "A", "what do you see there"),
            turn(4.0, 5.0, "B", "the depth of it and"),
            turn(5.0, 6.0, "A", "yeah all of that"),
        ];
        assert_eq!(spans(&transcript), [(0.5, 2.5), (2.5, 4.0), (4.0, 6.0)]);
    }

    #[test]
    fn a_backchannel_does_not_finish_the_speakers_thought() {
        // The interviewer's "Uh-huh." made the story look finished.
        let transcript = [
            turn(
                0.5,
                3.0,
                "A",
                "That is what the nobles did. So we had all these nobles with swords",
            ),
            turn(3.0, 3.4, "B", "Uh-huh."),
            turn(3.9, 5.5, "A", "and they lined up for the guard."),
        ];
        let found = utterances(&transcript);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].start, found[0].end), (0.5, 5.5));
        assert!(found[0].text.contains("Uh-huh. and they lined up"));
    }

    #[test]
    fn a_heard_sentence_end_inside_a_turn_is_a_cut_point() {
        let words: Vec<_> = [
            ("You", 0.5, 0.9),
            ("got", 0.9, 1.3),
            ("that", 1.3, 1.8),
            ("money.", 1.8, 2.95),
            ("And", 3.05, 3.5),
            ("you", 3.5, 4.0),
            ("can", 4.0, 4.6),
            ("learn", 4.6, 5.4),
            ("it.", 5.4, 7.0),
        ]
        .into_iter()
        .map(|(text, start, end)| TranscriptWord {
            start,
            end,
            text: text.to_owned(),
            confidence: None,
            estimated: false,
        })
        .collect();
        let mut heard = turn(0.5, 7.0, "A", "You got that money. And you can learn it.");
        heard.words = words;
        let found = utterances(std::slice::from_ref(&heard));
        assert_eq!(
            found,
            [
                Utterance {
                    start: 0.5,
                    end: 2.95,
                    text: "You got that money.".to_owned()
                },
                Utterance {
                    start: 3.05,
                    end: 7.0,
                    text: "And you can learn it.".to_owned()
                },
            ]
        );
        // Estimated word times are never cut points.
        for word in &mut heard.words {
            word.estimated = true;
        }
        assert_eq!(spans(std::slice::from_ref(&heard)), [(0.5, 7.0)]);
    }

    #[test]
    fn crowd_ambience_is_not_speech() {
        // ASR returned crowd noise as one 5.5 s turn reading "LT".
        let transcript = [
            turn(0.5, 6.01, "A", "LT"),
            turn(6.01, 7.21, "B", "Oh, you guys got a lot of merch."),
        ];
        assert_eq!(spans(&transcript), [(6.01, 7.21)]);
    }
}
