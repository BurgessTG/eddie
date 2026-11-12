//! The audio mix: measured once for loudness, encoded once.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use openreel_engine::{Clip, ClipKind, Timeline};

use super::scratch::FilterGraphFile;
use super::{export_error, f, ExportOptions};
use crate::{MediaInfo, MediaResult};

/// Integrated loudness a normalized export aims for, in LUFS.
pub const LOUDNESS_TARGET: f64 = -16.0;

/// Sample-peak ceiling after normalization (about -1.9 dBFS), leaving
/// headroom for AAC's inter-sample peaks.
const LOUDNESS_PEAK_LIMIT: f64 = 0.8;

/// The audio half of an export: the files it reads and the graph that mixes
/// them into `[aout]`.
pub(super) struct Mix {
    /// Source files, in input order after the placeholder input zero.
    pub(super) inputs: Vec<String>,
    pub(super) graph: String,
}

/// The timeline's audio mix, limited to the timeline's end. With
/// `options.normalize_loudness` the mix is measured first and one fixed gain
/// and a limiter are added.
pub(super) fn mix(
    timeline: &Timeline,
    infos: &HashMap<String, MediaInfo>,
    options: &ExportOptions,
) -> MediaResult<Mix> {
    // Audio input zero is a placeholder so the source inputs keep the same
    // indices in the loudness measurement and the encode. Audio remains
    // independent of picture visibility and is encoded once, without seams.
    let mut inputs: Vec<String> = Vec::new();
    let mut filters: Vec<String> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    for clip in audible_runs(timeline, infos) {
        let Some(src) = clip.source.as_ref() else {
            continue;
        };
        let idx = if let Some(i) = inputs.iter().position(|p| p == src) {
            i + 1
        } else {
            inputs.push(src.clone());
            inputs.len()
        };
        let label = format!("a{}", labels.len());
        filters.push(branch(timeline, options, &clip, idx, &label));
        labels.push(label);
    }
    let audio_mix = match labels.len() {
        0 => "anullsrc=r=48000:cl=stereo".to_owned(),
        1 => format!("[{}]anull", labels[0]),
        n => format!(
            "{}amix=inputs={n}:duration=longest:normalize=0",
            labels.iter().map(|l| format!("[{l}]")).collect::<String>()
        ),
    };
    // Video is already bounded by frame_count. Output -t rounds on its frame
    // clock and can drop the final active frame; limit audio independently.
    let mut audio_out = format!("{audio_mix},atrim=end={}", f(timeline.end()));
    if options.normalize_loudness && !labels.is_empty() {
        if let Some(loudness) = measure_loudness(&inputs, &filters, &audio_out)? {
            let gain = (LOUDNESS_TARGET - loudness).clamp(-30.0, 30.0);
            audio_out.push_str(&format!(
                ",volume={gain:.2}dB,alimiter=limit={LOUDNESS_PEAK_LIMIT}:level=disabled:latency=1"
            ));
        }
    }
    let graph = format!("{};{audio_out}[aout]", filters.join(";"));
    Ok(Mix {
        inputs,
        graph: graph.trim_start_matches(';').to_owned(),
    })
}

/// Every clip that sounds, in timeline order, with touching pieces of one
/// recording joined into one run.
fn audible_runs(timeline: &Timeline, infos: &HashMap<String, MediaInfo>) -> Vec<Clip> {
    // Audio is independent of visibility. Coalesce only genuinely contiguous
    // same-source, same-gain runs (the All scenes workflow), rather than
    // creating hundreds of delayed amix branches for continuous sound.
    // Overlapping clips are never coalesced: every audible contribution still
    // reaches amix.
    let mut audio_clips: Vec<_> = timeline
        .clips
        .iter()
        .filter(|clip| {
            clip.source.is_some()
                && (clip.kind == ClipKind::Audio
                    || (clip.kind == ClipKind::Video
                        && clip.embedded_audio
                        && clip
                            .source
                            .as_ref()
                            .and_then(|src| infos.get(src))
                            .is_some_and(|info| info.has_audio)))
        })
        .collect();
    audio_clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut runs: Vec<Clip> = Vec::new();
    let same_time =
        |a: f64, b: f64| (a - b).abs() <= 8.0 * f64::EPSILON * a.abs().max(b.abs()).max(1.0);
    for clip in audio_clips {
        if let Some(last) = runs.last_mut().filter(|last| {
            last.source == clip.source
                && last.volume == clip.volume
                && !last.duck
                && !clip.duck
                && last.clean_voice == clip.clean_voice
                && last.smooth_fades == clip.smooth_fades
                && last.fade_lengths() == (0.0, 0.0)
                && clip.fade_lengths() == (0.0, 0.0)
                && same_time(last.end(), clip.start)
                && same_time(last.source_in + last.duration, clip.source_in)
        }) {
            last.duration = clip.end() - last.start;
        } else {
            runs.push(clip.clone());
        }
    }
    runs
}

/// One run's branch of the graph: input `idx` trimmed to the run, cleaned,
/// set to its volume, faded, ducked and delayed to its place, as `[label]`.
fn branch(
    timeline: &Timeline,
    options: &ExportOptions,
    clip: &Clip,
    idx: usize,
    label: &str,
) -> String {
    // To the sample, as the preview mixer places it. Whole milliseconds
    // left a gap or an overlap of up to 24 samples between clips that
    // meet off the millisecond grid, such as at a 1/30 s frame edge.
    let delay = (clip.start.max(0.0) * 48_000.0).round() as i64;
    // afade's default curve is linear and its qsin is
    // `openreel_engine::fade_shape`'s quarter sine, matching the preview.
    let (fade_in, fade_out) = clip.fade_lengths();
    let curve = if clip.smooth_fades { ":curve=qsin" } else { "" };
    let mut fades = String::new();
    if fade_in > 0.0 {
        fades.push_str(&format!(",afade=t=in:st=0:d={}{curve}", f(fade_in)));
    }
    if fade_out > 0.0 {
        fades.push_str(&format!(
            ",afade=t=out:st={}:d={}{curve}",
            f(clip.duration - fade_out),
            f(fade_out)
        ));
    }
    let cleanup = if clip.clean_voice {
        crate::voice::voice_filter()
    } else {
        String::new()
    };
    // The preview mixer's `duck_gain` on clip-local time: raised-cosine
    // ramps in decibels. Merged ranges never share a ramp, so their
    // amounts add. Short frames keep each step under 6 ms so the ramps
    // do not zipper. Speech just before the clip gives negative times,
    // hence the brackets around each one.
    let ducks = timeline.duck_ranges(clip, &options.speech);
    if !ducks.is_empty() {
        use openreel_engine::{DUCK_ATTACK, DUCK_HOLD, DUCK_LEVEL, DUCK_RELEASE};
        let amount: Vec<String> = ducks
            .iter()
            .map(|&(start, end)| {
                let (down, hold) = (start - DUCK_ATTACK, end + DUCK_HOLD);
                format!(
                    "if(gte(t,({}))*lt(t,({})),if(lt(t,({})),(1-cos((t-({}))/{}*PI))/2,\
                     if(lt(t,({})),1,(1+cos((t-({}))/{}*PI))/2)),0)",
                    f(down),
                    f(hold + DUCK_RELEASE),
                    f(start),
                    f(down),
                    f(DUCK_ATTACK),
                    f(hold),
                    f(hold),
                    f(DUCK_RELEASE),
                )
            })
            .collect();
        fades.push_str(&format!(
            ",asetnsamples=n=256:p=0,volume=volume='pow({},min(1,{}))':eval=frame",
            f(f64::from(DUCK_LEVEL)),
            amount.join("+")
        ));
    }
    format!(
        "[{idx}:a]atrim=start={}:end={},asetpts=PTS-STARTPTS,aresample=48000{cleanup},\
         volume={:.3}{fades},adelay=delays={delay}S:all=1[{label}]",
        f(clip.source_in),
        f(clip.source_in + clip.duration),
        clip.volume,
    )
}

/// Encode the timeline's audio mix once, on the whole-timeline clock.
pub(super) fn encode_audio(inputs: &[String], graph: &str, path: &Path) -> MediaResult<()> {
    let graph_file = FilterGraphFile::create(graph)?;
    let mut cmd = crate::cli::ffmpeg();
    cmd.args(["-y", "-v", "error", "-f", "lavfi", "-i", "anullsrc=d=0.01"]);
    for input in inputs {
        cmd.args(["-vn", "-i", input]);
    }
    let out = cmd
        .arg("-/filter_complex")
        .arg(&graph_file.0)
        .args(["-map", "[aout]", "-c:a", "aac", "-b:a", "160k", "-f", "mp4"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(export_error(&String::from_utf8_lossy(&out.stderr)));
    }
    Ok(())
}

/// Integrated loudness (LUFS) of the export's audio mix, decoding only the
/// audio inputs. `None` when the mix is silent.
fn measure_loudness(
    inputs: &[String],
    audio_filters: &[String],
    mix: &str,
) -> MediaResult<Option<f64>> {
    let graph = format!(
        "{};{mix},loudnorm=print_format=json[aout]",
        audio_filters.join(";")
    );
    let graph_file = FilterGraphFile::create(&graph)?;
    let mut cmd = crate::cli::ffmpeg();
    // Input 0 stands in for the video pipe so audio input indices match.
    cmd.args([
        "-hide_banner",
        "-nostats",
        "-f",
        "lavfi",
        "-i",
        "anullsrc=d=0.01",
    ]);
    for input in inputs {
        cmd.args(["-vn", "-i", input]);
    }
    let out = cmd
        .arg("-/filter_complex")
        .arg(&graph_file.0)
        .args(["-map", "[aout]", "-f", "null", "-"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to launch ffmpeg: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(export_error(&stderr));
    }
    // The JSON report is followed by FFmpeg's own summary lines.
    let report = stderr
        .rfind('{')
        .zip(stderr.rfind('}'))
        .and_then(|(start, end)| {
            serde_json::from_str::<serde_json::Value>(stderr.get(start..=end)?).ok()
        })
        .ok_or_else(|| "loudness measurement returned no report".to_owned())?;
    Ok(report["input_i"]
        .as_str()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > -70.0))
}
