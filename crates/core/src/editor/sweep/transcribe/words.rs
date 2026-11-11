//! Words and turns from what whisper heard: only detected speech is
//! listened to, and a word ends where its sound stops.

use std::sync::Arc;

use super::super::{
    align, speech, TranscriptSegment, TranscriptSession, TranscriptWord, SAMPLE_RATE,
};

/// The stretches of a window that hold speech. whisper hears only these,
/// so music and room tone are never "heard" as words; times it reports
/// are mapped back to the window with `original`.
pub(super) struct Speech {
    /// First and end sample of each stretch, in order.
    pub(super) kept: Vec<(usize, usize)>,
}

/// Sound kept either side of detected speech, so word edges are heard whole.
const SPEECH_PAD: f64 = 0.2;

/// Speech this close together stays one stretch: whisper reads sentence
/// ends from ordinary pauses, so only longer non-speech is removed.
const SPEECH_JOIN: f64 = 1.0;

/// Silence put where sound was removed, as whisper.cpp's own detector does.
const SPEECH_GAP: usize = SAMPLE_RATE / 10;

impl Speech {
    pub(super) fn whole(samples: usize) -> Self {
        Self {
            kept: vec![(0, samples)],
        }
    }

    /// `spans` are the detector's (start, end) seconds, in order.
    pub(super) fn from_spans(spans: impl Iterator<Item = (f64, f64)>, samples: usize) -> Self {
        let sample = |seconds: f64| ((seconds.max(0.0) * SAMPLE_RATE as f64) as usize).min(samples);
        let mut kept: Vec<(usize, usize)> = Vec::new();
        for (start, end) in spans {
            let (from, to) = (sample(start - SPEECH_PAD), sample(end + SPEECH_PAD));
            if to <= from {
                continue;
            }
            match kept.last_mut() {
                Some(last) if sample(start - SPEECH_JOIN) <= last.1 => last.1 = last.1.max(to),
                _ => kept.push((from, to)),
            }
        }
        Self { kept }
    }

    /// What whisper hears: the kept stretches with short silences between.
    pub(super) fn samples(&self, pcm: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        for (index, (from, to)) in self.kept.iter().enumerate() {
            if index > 0 {
                out.resize(out.len() + SPEECH_GAP, 0.0);
            }
            out.extend_from_slice(&pcm[*from..*to]);
        }
        out
    }

    /// Window time of a time in `samples`. A time in an inserted silence is
    /// the end of the speech before it.
    pub(super) fn original(&self, heard: f64) -> f64 {
        let seconds = |samples: usize| samples as f64 / SAMPLE_RATE as f64;
        let mut at = 0.0;
        let mut last_end = 0.0;
        for (index, (from, to)) in self.kept.iter().enumerate() {
            if index > 0 {
                at += seconds(SPEECH_GAP);
            }
            if heard < at {
                return last_end;
            }
            let length = seconds(to - from);
            if heard < at + length {
                return seconds(*from) + (heard - at);
            }
            at += length;
            last_end = seconds(*to);
        }
        last_end
    }

    fn holds(&self, sample: usize) -> bool {
        self.kept
            .iter()
            .any(|(from, to)| (*from..*to).contains(&sample))
    }
}

/// Which 20 ms frames of a window hold sound: inside the speech whisper
/// heard, above that speech's own silence level, and in a run that gets
/// well above it. A breath or a click in a pause is over the silence level
/// for a frame or two and never loud; a word's soft start or tail is quiet
/// too, but runs into the word.
pub(super) struct Sound {
    frames: Vec<bool>,
}

const SOUND_FRAME: usize = SAMPLE_RATE / 50;

/// A run of sound must reach this many times the silence level.
const SOUND_REACH: f32 = 3.0;

impl Sound {
    pub(super) fn new(pcm: &[f32], speech: &Speech) -> Self {
        let peaks: Vec<f32> = pcm
            .chunks(SOUND_FRAME)
            .enumerate()
            .map(|(frame, samples)| {
                if speech.holds(frame * SOUND_FRAME + SOUND_FRAME / 2) {
                    samples.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()))
                } else {
                    0.0
                }
            })
            .collect();
        let spoken: Vec<f32> = peaks.iter().copied().filter(|peak| *peak > 0.0).collect();
        let silent = openreel_media::silence_threshold(&spoken);
        let mut frames = vec![false; peaks.len()];
        let mut at = 0;
        while at < peaks.len() {
            let begins = at;
            while at < peaks.len() && peaks[at] > silent {
                at += 1;
            }
            if peaks[begins..at]
                .iter()
                .any(|peak| *peak >= silent * SOUND_REACH)
            {
                frames[begins..at].fill(true);
            }
            at = at.max(begins + 1);
        }
        Self { frames }
    }

    /// Runs of frames without sound that begin in `from..to` (seconds), as
    /// (start, end). A run already under way at `from` begins there, and a
    /// run is followed to its end even past `to`.
    fn pauses(&self, from: f64, to: f64) -> Vec<(f64, f64)> {
        let seconds = |frame: usize| (frame * SOUND_FRAME) as f64 / SAMPLE_RATE as f64;
        let frame = |at: f64| (at.max(0.0) * SAMPLE_RATE as f64 / SOUND_FRAME as f64) as usize;
        let mut runs = Vec::new();
        let mut at = frame(from);
        while at < self.frames.len() && seconds(at) < to {
            if self.frames[at] {
                at += 1;
                continue;
            }
            let begins = at;
            while at < self.frames.len() && !self.frames[at] {
                at += 1;
            }
            runs.push((seconds(begins).max(from), seconds(at)));
        }
        runs
    }
}

/// One whisper token on its window's clock.
pub(super) struct Token {
    pub(super) text: String,
    /// Where the token before it ends, or its segment starts.
    pub(super) start: f64,
    pub(super) end: f64,
    pub(super) probability: f32,
    /// A segment's first token starts a word even without a leading space.
    pub(super) opens_segment: bool,
}

/// A pause this short between two words is only trusted where whisper put
/// punctuation; elsewhere it may be a consonant's closure inside a word.
const PAUSE_SECONDS: f64 = 0.16;

/// A pause shorter than this is not looked for at all.
const MIN_PAUSE_SECONDS: f64 = 0.04;

/// Join whisper tokens into words: a token starting with a space starts a
/// new word. A word runs from its first token's start to the end of its
/// last token that is not punctuation. Punctuation time, and the start of
/// the next word, hold any pause between two words: where the sound stops
/// there the word ends, and the next starts where sound returns. Words
/// with no pause between them touch.
pub(super) fn words_from_tokens(
    tokens: &[Token],
    sound: &Sound,
    offset: f64,
) -> Vec<TranscriptWord> {
    struct Raw {
        start: f64,
        /// End of the first token: the word's sound has begun by here.
        begun: f64,
        end: f64,
        text: String,
        probabilities: Vec<f32>,
    }
    let spoken = |text: &str| text.chars().any(char::is_alphanumeric);
    let mut raw: Vec<Raw> = Vec::new();
    for token in tokens {
        match raw.last_mut() {
            Some(word) if !token.text.starts_with(' ') && !token.opens_segment => {
                if spoken(&token.text) {
                    word.end = token.end;
                }
                word.text.push_str(&token.text);
                word.probabilities.push(token.probability);
            }
            _ => raw.push(Raw {
                start: token.start,
                begun: token.end,
                end: token.end,
                text: token.text.clone(),
                probabilities: vec![token.probability],
            }),
        }
    }
    let last_token_end = tokens.last().map_or(0.0, |token| token.end);
    raw.retain(|word| spoken(&word.text));

    // Where the word before a gap stops and the word after it starts.
    // Either may be missing: the gap before the first word or after the last.
    let gap = |before: Option<&Raw>, word: Option<&Raw>| -> (f64, f64) {
        let from = match (before, word) {
            (Some(before), _) => before.end,
            (None, Some(word)) => word.start,
            (None, None) => 0.0,
        };
        // Punctuation after `before` ends where `word` starts.
        let lead = word.map_or(last_token_end, |word| word.start).max(from);
        let until = word.map_or(lead + align::SNAP_SECONDS, |word| {
            (word.begun - align::MIN_KEEP_SECONDS).max(lead)
        });
        // The longest quiet run is the pause. A shorter one beside it is a
        // consonant's closure in one of the two words.
        sound
            .pauses(from, (lead + align::SNAP_SECONDS).min(until).max(lead))
            .into_iter()
            .map(|(start, end)| (start, end.min(until)))
            .filter(|(start, end)| {
                let least = if *start <= lead {
                    MIN_PAUSE_SECONDS
                } else {
                    PAUSE_SECONDS
                };
                end - start >= least - 1e-9
            })
            .max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0)))
            // No pause: punctuation time stays with the earlier word.
            .unwrap_or((lead, lead))
    };

    let mut words: Vec<TranscriptWord> = Vec::with_capacity(raw.len());
    for index in 0..=raw.len() {
        let word = raw.get(index);
        let (stops, starts) = gap(index.checked_sub(1).map(|index| &raw[index]), word);
        if let Some(earlier) = words.last_mut() {
            earlier.end = (offset + stops).max(earlier.start);
        }
        if let Some(word) = word {
            words.push(TranscriptWord {
                start: offset + starts,
                end: offset + word.end.max(starts),
                text: word.text.trim().to_owned(),
                confidence: Some(
                    word.probabilities.iter().sum::<f32>() / word.probabilities.len().max(1) as f32,
                ),
                estimated: false,
            });
        }
    }
    words
}

/// Group a window's words into turns: a turn is speech without a pause of
/// `speech::CONTINUATION_SECONDS`, and starts and ends on its own words.
/// whisper's segments are not used: they split sentences and run through
/// pauses.
pub(super) fn turns_from_words(
    words: Vec<TranscriptWord>,
    session: &Arc<TranscriptSession>,
) -> Vec<TranscriptSegment> {
    let mut turns: Vec<TranscriptSegment> = Vec::new();
    for word in words {
        match turns.last_mut() {
            Some(turn) if word.start - turn.end < speech::CONTINUATION_SECONDS => {
                turn.end = word.end;
                turn.text.push(' ');
                turn.text.push_str(&word.text);
                turn.words.push(word);
            }
            _ => turns.push(TranscriptSegment {
                start: word.start,
                end: word.end,
                speaker: None,
                text: word.text.clone(),
                words: vec![word],
                session: Some(Arc::clone(session)),
            }),
        }
    }
    turns
}
