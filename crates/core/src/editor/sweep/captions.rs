//! Breaking a transcript into caption lines a viewer can read.

use super::{TranscriptSegment, TranscriptWord};

#[derive(Clone, Debug, PartialEq)]
pub struct CaptionChunk {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// Source start time of each word in `text`, only from real word timing.
    pub words: Vec<f64>,
}

/// A word's letters and apostrophes, lowercased, for comparing repeats.
fn caption_key(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'' || *c == '’')
        .flat_map(char::to_lowercase)
        .collect()
}

fn ends_with_any(text: &str, marks: &str) -> bool {
    text.trim_end_matches(['"', '”', ')'])
        .chars()
        .last()
        .is_some_and(|c| marks.contains(c))
}

/// Remove spoken stutters: a run of one to three words immediately
/// repeated ("out of of", "if it's if it's"). The first copy keeps its
/// start, the second's end and punctuation. Different words that only
/// share a stem ("Eddie Eddie's") are kept.
fn drop_stutters(words: Vec<(f64, f64, String)>) -> Vec<(f64, f64, String)> {
    let mut kept: Vec<(f64, f64, String)> = Vec::new();
    let mut index = 0;
    'words: while index < words.len() {
        for run in (1..=3).rev() {
            if kept.len() < run || index + run > words.len() {
                continue;
            }
            let earlier = &kept[kept.len() - run..];
            let later = &words[index..index + run];
            // A sentence end between the copies is a real repeat, not a stutter.
            if ends_with_any(&earlier[run - 1].2, ".?!") {
                continue;
            }
            let same = earlier.iter().zip(later).all(|(a, b)| {
                !caption_key(&a.2).is_empty() && caption_key(&a.2) == caption_key(&b.2)
            });
            if same {
                let last = kept.last_mut().expect("run is non-empty");
                let (_, end, text) = &later[run - 1];
                last.1 = last.1.max(*end);
                if ends_with_any(text, ".?!,;:") && !ends_with_any(&last.2, ".?!,;:") {
                    last.2 = text.clone();
                }
                index += run;
                continue 'words;
            }
        }
        kept.push(words[index].clone());
        index += 1;
    }
    kept
}

/// How many words of a full caption line to keep when the next word does
/// not fit: after a clause mark, else before a conjunction, in the later
/// two thirds; else all but a dangling article, preposition or pronoun.
fn caption_break(group: &[(f64, f64, String)]) -> usize {
    const JOINS: [&str; 9] = [
        "and", "but", "so", "because", "while", "when", "after", "then", "or",
    ];
    const DANGLING: [&str; 24] = [
        "a", "an", "the", "i", "to", "of", "in", "on", "at", "by", "from", "into", "with", "for",
        "my", "our", "your", "it", "it's", "and", "but", "so", "or", "is",
    ];
    let earliest = group.len().div_ceil(3).max(1);
    if let Some(keep) = (earliest..group.len())
        .rev()
        .find(|&keep| ends_with_any(&group[keep - 1].2, ",;:"))
    {
        return keep;
    }
    if let Some(keep) = (earliest..group.len())
        .rev()
        .find(|&keep| JOINS.contains(&caption_key(&group[keep].2).as_str()))
    {
        return keep;
    }
    let mut keep = group.len();
    while keep > earliest && DANGLING.contains(&caption_key(&group[keep - 1].2).as_str()) {
        keep -= 1;
    }
    keep
}

/// Split one transcript turn into readable caption chunks. Real
/// word timings are used when present; turn-only text is allocated
/// proportionally and therefore remains an estimate.
pub fn caption_chunks(segment: &TranscriptSegment) -> Vec<CaptionChunk> {
    const MAX_CHARS: usize = 42;
    const MAX_DURATION: f64 = 3.5;
    if segment.text.trim().is_empty() || segment.end <= segment.start || segment.is_ambient() {
        return Vec::new();
    }

    if !segment.words.is_empty() {
        let mut words: Vec<&TranscriptWord> = segment
            .words
            .iter()
            .filter(|word| {
                word.start.is_finite()
                    && word.end.is_finite()
                    && word.end >= word.start
                    && !word.text.trim().is_empty()
            })
            .collect();
        words.sort_by(|a, b| a.start.total_cmp(&b.start));
        let words = drop_stutters(
            words
                .into_iter()
                .map(|word| (word.start, word.end, word.text.trim().to_owned()))
                .collect(),
        );
        if !words.is_empty() {
            let mut chunks: Vec<CaptionChunk> = Vec::new();
            let mut group: Vec<(f64, f64, String)> = Vec::new();
            let push_group = |group: &[(f64, f64, String)], chunks: &mut Vec<CaptionChunk>| {
                let (Some(first), Some(last)) = (group.first(), group.last()) else {
                    return;
                };
                let (start, last_end) = (first.0, last.1);
                chunks.push(CaptionChunk {
                    start,
                    end: last_end.max(start + 0.8).min(segment.end.max(last_end)),
                    text: group
                        .iter()
                        .map(|word| word.2.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                    words: group.iter().map(|word| word.0).collect(),
                });
            };
            for word in words {
                let next_len = group.iter().map(|w| w.2.len() + 1).sum::<usize>() + word.2.len();
                let next_duration = group.first().map(|first| word.1 - first.0).unwrap_or(0.0);
                let ends_sentence = ends_with_any(&word.2, ".?!");
                // A sentence may run a little long rather than leave its
                // last word alone on the next caption.
                let max_chars = if ends_sentence {
                    MAX_CHARS + 8
                } else {
                    MAX_CHARS
                };
                if !group.is_empty() && (next_len > max_chars || next_duration > MAX_DURATION) {
                    let keep = caption_break(&group);
                    let carried = group.split_off(keep);
                    push_group(&group, &mut chunks);
                    group = carried;
                }
                group.push(word);
                if ends_sentence {
                    push_group(&group, &mut chunks);
                    group.clear();
                }
            }
            push_group(&group, &mut chunks);
            for index in 0..chunks.len().saturating_sub(1) {
                let next_start = chunks[index + 1].start;
                if chunks[index].end > next_start {
                    chunks[index].end = next_start;
                }
            }
            return chunks
                .into_iter()
                .filter(|chunk| chunk.end > chunk.start)
                .collect();
        }
    }

    let mut pieces: Vec<String> = Vec::new();
    let mut current = String::new();
    let words = drop_stutters(
        segment
            .text
            .split_whitespace()
            .map(|word| (0.0, 0.0, word.to_owned()))
            .collect(),
    );
    for (_, _, word) in &words {
        if !current.is_empty() && current.len() + 1 + word.len() > MAX_CHARS {
            pieces.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    if pieces.is_empty() {
        return Vec::new();
    }
    let total_chars: usize = pieces.iter().map(|piece| piece.len()).sum();
    let duration = segment.end - segment.start;
    let mut cursor = segment.start;
    let last_index = pieces.len() - 1;
    pieces
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let end = if index == last_index {
                segment.end
            } else if total_chars > 0 {
                cursor + duration * text.len() as f64 / total_chars as f64
            } else {
                segment.end
            };
            let chunk = CaptionChunk {
                start: cursor,
                end,
                text,
                words: Vec::new(),
            };
            cursor = end;
            chunk
        })
        .collect()
}
