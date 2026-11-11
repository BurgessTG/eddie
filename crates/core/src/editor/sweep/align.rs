//! Local word timing for turn-level transcripts.
//!
//! Meta's Voice Transcribe returns speaker turns only. For a turn that holds
//! more than one sentence, whisper.cpp re-hears the turn's audio locally with
//! per-word timestamps. Meta's words and punctuation stay authoritative: they
//! only take timing from matched heard words, and unmatched words are
//! interpolated and marked `estimated`, so they never become cut points.

use std::path::{Path, PathBuf};

use openreel_media::MediaInfo;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::{ends_sentence, TranscriptSegment, TranscriptWord, SAMPLE_RATE};

/// Shorter turns rarely hold two sentences worth separating.
const MIN_TURN_SECONDS: f64 = 2.0;
/// Audio around the turn, so words at its edges are heard whole.
const PAD_SECONDS: f64 = 0.3;
/// Below this share of matched words, the local hearing is a different
/// transcript, not timing for this one.
const MIN_MATCHED_SHARE: f64 = 0.5;
/// Heard word edges are good to about ±0.2 s. A sentence break moves to the
/// quietest 20 ms of audio within this distance.
pub(super) const SNAP_SECONDS: f64 = 0.25;
/// A snapped break leaves each neighboring word at least this much.
pub(super) const MIN_KEEP_SECONDS: f64 = 0.04;

/// The multilingual base model (`ggml-base.bin`): `EDDIE_WHISPER_MODEL`, or
/// `ggml-base.bin` in `installed`, the user's model folder. Absent means no
/// local alignment; curation keeps turn-level timing. Without `installed`
/// (an editor built for a test) the environment is not read either.
pub fn model_path(installed: Option<&Path>) -> Option<PathBuf> {
    let installed = installed?;
    std::env::var_os("EDDIE_WHISPER_MODEL")
        .map(PathBuf::from)
        .or_else(|| Some(installed.join("ggml-base.bin")))
        .filter(|path| path.is_file())
}

/// Turns that can gain a cut point: no word timing yet, long enough, and
/// holding a sentence end before their last word.
fn needs_alignment(turn: &TranscriptSegment) -> bool {
    let words: Vec<&str> = turn.text.split_whitespace().collect();
    turn.words.is_empty()
        && turn.end - turn.start >= MIN_TURN_SECONDS
        && words
            .split_last()
            .is_some_and(|(_, inner)| inner.iter().any(|word| ends_sentence(word)))
}

/// Add word timing to every turn that needs it. Returns (aligned, candidates).
/// A turn whose local hearing does not match keeps turn-level timing.
pub fn align_transcript(
    info: &MediaInfo,
    model: Option<&Path>,
    transcript: &mut [TranscriptSegment],
) -> Result<(usize, usize), String> {
    let candidates: Vec<usize> = (0..transcript.len())
        .filter(|index| needs_alignment(&transcript[*index]))
        .collect();
    if candidates.is_empty() {
        return Ok((0, 0));
    }
    let path = model.ok_or("no local model")?;
    whisper_rs::install_logging_hooks();
    // A model that does not fit the GPU crashes the process (`gpu`), and
    // this one is quick on the CPU.
    let params = WhisperContextParameters {
        use_gpu: super::gpu::memory().is_some_and(|memory| memory.fits(path)),
        ..Default::default()
    };
    let context = WhisperContext::new_with_params(path, params)
        .map_err(|error| format!("local model could not load: {error}"))?;
    let mut state = context
        .create_state()
        .map_err(|error| format!("local model could not start: {error}"))?;
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(8)) as i32;
    let mut aligned = 0;
    for index in &candidates {
        let turn = &mut transcript[*index];
        let start = (turn.start - PAD_SECONDS).max(0.0);
        let end = (turn.end + PAD_SECONDS).min(info.duration.max(turn.end));
        let mut pcm = openreel_media::pcm_mono_16k(&info.path, start, end - start)
            .map_err(|_| "turn audio could not be decoded".to_owned())?;
        // whisper.cpp rejects input shorter than one second.
        pcm.resize(pcm.len().max(SAMPLE_RATE * 11 / 10), 0.0);
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        // No initial prompt: prompting with Meta's text made the base model
        // hallucinate continuations on noisy walk-and-talk audio.
        params.set_language(Some("auto"));
        params.set_token_timestamps(true);
        params.set_max_len(1);
        params.set_split_on_word(true);
        params.set_no_context(true);
        params.set_n_threads(threads);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);
        state
            .full(params, &pcm)
            .map_err(|error| format!("local hearing failed: {error}"))?;
        let heard: Vec<(f64, f64, String)> = state
            .as_iter()
            .filter_map(|segment| {
                let text = segment.to_str_lossy().ok()?.trim().to_owned();
                (!text.is_empty()).then(|| {
                    (
                        start + segment.start_timestamp() as f64 / 100.0,
                        start + segment.end_timestamp() as f64 / 100.0,
                        text,
                    )
                })
            })
            .collect();
        if let Some(mut words) = place(turn, &heard) {
            snap_breaks(&mut words, &pcm, start);
            turn.words = words;
            aligned += 1;
        }
    }
    Ok((aligned, candidates.len()))
}

/// Move each heard sentence break to the quietest 20 ms frame near it, so a
/// cut there falls in the pause rather than on a word edge.
pub fn snap_breaks(words: &mut [TranscriptWord], pcm: &[f32], offset: f64) {
    const FRAME: usize = SAMPLE_RATE / 50;
    for k in 0..words.len().saturating_sub(1) {
        if !ends_sentence(&words[k].text) || words[k].estimated || words[k + 1].estimated {
            continue;
        }
        // Heard edges can be early or late by more than half a short word,
        // so search up to each word's far edge, leaving it MIN_KEEP_SECONDS.
        let low = words[k].start + MIN_KEEP_SECONDS;
        let high = words[k + 1].end - MIN_KEEP_SECONDS;
        let from = (words[k].end - SNAP_SECONDS).max(low) - offset;
        let to = (words[k + 1].start + SNAP_SECONDS).min(high) - offset;
        // Frames whose centre lies in [from, to].
        let first = ((from * SAMPLE_RATE as f64 / FRAME as f64) - 0.5)
            .ceil()
            .max(0.0) as usize;
        let last = ((to * SAMPLE_RATE as f64 / FRAME as f64) - 0.5).floor();
        if last < first as f64 {
            continue;
        }
        let quietest = (first..=last as usize)
            .filter_map(|frame| {
                let samples = pcm.get(frame * FRAME..(frame + 1) * FRAME)?;
                Some((samples.iter().map(|x| x * x).sum::<f32>(), frame))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, frame)) = quietest {
            let at = offset + (frame as f64 + 0.5) * FRAME as f64 / SAMPLE_RATE as f64;
            words[k].end = at.max(words[k].start);
            words[k + 1].start = at.min(words[k + 1].end);
        }
    }
}

fn key(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Give the turn's own words times from heard words. Matching is a longest
/// common subsequence of normalized words; matches that would run backwards
/// or leave the turn are dropped. Unmatched words share the gap between
/// their matched neighbors by letter count and are marked `estimated`.
fn place(turn: &TranscriptSegment, heard: &[(f64, f64, String)]) -> Option<Vec<TranscriptWord>> {
    let words: Vec<&str> = turn.text.split_whitespace().collect();
    let ours: Vec<String> = words.iter().map(|word| key(word)).collect();
    let theirs: Vec<String> = heard.iter().map(|(_, _, text)| key(text)).collect();
    let (n, m) = (ours.len(), theirs.len());
    let mut table = vec![vec![0_u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if !ours[i].is_empty() && ours[i] == theirs[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut times: Vec<Option<(f64, f64)>> = vec![None; n];
    let (mut i, mut j, mut floor) = (0, 0, turn.start);
    while i < n && j < m {
        if !ours[i].is_empty() && ours[i] == theirs[j] {
            let (start, end) = (heard[j].0.max(turn.start), heard[j].1.min(turn.end));
            if start >= floor - 1e-9 && end > start {
                times[i] = Some((start, end));
                floor = end;
            }
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    let spoken = ours.iter().filter(|word| !word.is_empty()).count();
    let matched = times.iter().filter(|time| time.is_some()).count();
    if spoken == 0 || (matched as f64) < MIN_MATCHED_SHARE * spoken as f64 {
        return None;
    }
    let mut placed = Vec::with_capacity(n);
    let mut index = 0;
    while index < n {
        if let Some((start, end)) = times[index] {
            placed.push(TranscriptWord {
                start,
                end,
                text: words[index].to_owned(),
                confidence: None,
                estimated: false,
            });
            index += 1;
            continue;
        }
        let run_end = (index..n).find(|k| times[*k].is_some()).unwrap_or(n);
        let from = placed
            .last()
            .map_or(turn.start, |word: &TranscriptWord| word.end);
        let to = times
            .get(run_end)
            .copied()
            .flatten()
            .map_or(turn.end, |t| t.0);
        let letters: Vec<f64> = (index..run_end)
            .map(|k| words[k].chars().count().max(1) as f64)
            .collect();
        let total: f64 = letters.iter().sum();
        let mut at = from;
        for (offset, share) in letters.iter().enumerate() {
            let next = at + (to - from).max(0.0) * share / total;
            placed.push(TranscriptWord {
                start: at,
                end: next,
                text: words[index + offset].to_owned(),
                confidence: None,
                estimated: true,
            });
            at = next;
        }
        index = run_end;
    }
    Some(placed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(text: &str, start: f64, end: f64) -> TranscriptSegment {
        TranscriptSegment {
            start,
            end,
            speaker: Some("A".to_owned()),
            text: text.to_owned(),
            words: Vec::new(),
            session: None,
        }
    }

    #[test]
    fn heard_words_time_the_turns_own_words_and_gaps_are_estimated() {
        // Meta's text keeps its words and punctuation. Heard words differ in
        // case, punctuation and one misheard word ("for" vs "four").
        let turn = turn("You got that money. So yeah, four parts.", 1.0, 5.0);
        let heard = [
            (1.1, 1.3, " you".to_owned()),
            (1.3, 1.5, " got".to_owned()),
            (1.5, 1.7, " that".to_owned()),
            (1.7, 2.2, " money,".to_owned()),
            (2.9, 3.1, " so".to_owned()),
            (3.1, 3.4, " yeah".to_owned()),
            (3.4, 3.6, " for".to_owned()),
            (3.6, 4.2, " parts".to_owned()),
        ];
        let words = place(&turn, &heard).unwrap();
        let summary: Vec<_> = words
            .iter()
            .map(|w| (w.text.as_str(), w.start, w.end, w.estimated))
            .collect();
        assert_eq!(
            summary,
            [
                ("You", 1.1, 1.3, false),
                ("got", 1.3, 1.5, false),
                ("that", 1.5, 1.7, false),
                ("money.", 1.7, 2.2, false),
                ("So", 2.9, 3.1, false),
                ("yeah,", 3.1, 3.4, false),
                // Unmatched: fills the gap between "yeah" and "parts".
                ("four", 3.4, 3.6, true),
                ("parts.", 3.6, 4.2, false),
            ]
        );
    }

    #[test]
    fn a_different_hearing_is_not_used_as_timing() {
        let turn = turn("You got that money. So yeah, five parts.", 0.0, 4.0);
        let heard = [
            (0.1, 0.5, " that's".to_owned()),
            (0.5, 1.0, " good.".to_owned()),
            (1.5, 2.0, " money".to_owned()),
        ];
        assert!(place(&turn, &heard).is_none());
    }

    #[test]
    fn only_turns_with_an_inner_sentence_end_need_alignment() {
        assert!(needs_alignment(&turn("Done here. Next part now", 0.0, 4.0)));
        assert!(!needs_alignment(&turn("One long sentence only.", 0.0, 4.0)));
        assert!(!needs_alignment(&turn("Short. Turn.", 0.0, 1.5)));
    }

    #[test]
    fn a_sentence_break_finds_the_pause_past_the_next_words_midpoint() {
        // Heard edges are early: "You" really starts after the dip at
        // 7.14 s, past its heard midpoint (7.10 s). A quieter frame at the
        // very end of "You" must not swallow the word.
        let word = |text: &str, start, end| TranscriptWord {
            start,
            end,
            text: text.to_owned(),
            confidence: None,
            estimated: false,
        };
        let mut words = vec![word("parts.", 6.7, 7.0), word("You", 7.0, 7.2)];
        let offset = 6.0;
        let mut pcm = vec![0.5_f32; SAMPLE_RATE * 2];
        let frame = |at: f64| ((at - offset) * SAMPLE_RATE as f64).round() as usize;
        pcm[frame(7.14)..frame(7.16)].fill(0.05);
        pcm[frame(7.18)..frame(7.20)].fill(0.0);
        snap_breaks(&mut words, &pcm, offset);
        assert!((words[0].end - 7.15).abs() < 1e-9, "{:?}", words[0].end);
        assert!((words[1].start - 7.15).abs() < 1e-9, "{:?}", words[1].start);
    }
}
