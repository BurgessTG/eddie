# Eddie build guidance

Read this before changing code. It records decisions the user has made.
Update it when a decision changes.

## Stack

- Rust only. `crates/engine` owns the timeline model. `crates/media` owns
  FFmpeg. `crates/app` owns egui UI.
- FFmpeg 9 is the target. In-process decode uses `ffmpeg-next` 9.
- Builds need FFmpeg dev libraries, `pkg-config`, and `clang` for bindgen,
  plus `cmake` and a C++ compiler for whisper.cpp (`whisper-rs`).
- Runtime needs `ffmpeg` and `ffprobe` on `PATH` until probe and export
  also move in-process. Audio playback uses cpal (ALSA on Linux, so builds
  need the ALSA dev library) and libswresample.

## Commands

```sh
cd /mnt/data/Work/eddie
mise exec -- cargo run            # debug
mise exec -- cargo run --release  # use this to judge performance
SDL_AUDIODRIVER=dummy mise exec -- cargo test   # headless; media end-to-end needs it without an audio device
mise exec -- cargo fmt
```

Rust is pinned in `mise.toml`. Do not switch toolchains casually.

## Storage

The dev machine has a small disk. Rust target directories are the main cost.

- `target/` has reached 5.8 GB here. Watch it with `du -sh target`.
- The dev profile sets `debug = 0` and `incremental = false` on purpose.
  Do not re-enable them without asking.
- Build one profile at a time. When a release build is stale, run
  `mise exec -- cargo clean --release`. It freed 1.2 GB here.
- Never create scratch crates with their own `target/` for probes. Reuse
  the workspace, or delete the scratch directory when done.
- Do not run `cargo clean` mid-iteration. It trades disk for a multi-minute
  rebuild on this CPU. Clean at slice boundaries.
- Sample media is generated, not committed by hand. Keep `samples/` small.

## pstack

The poteto pstack skills are installed in Muse user scope
(`~/.config/muse/skills`, 34 skills, validated 2026-10-25).

- Start new sessions with `/poteto-mode` for non-trivial work.
- Use `/tdd` when a bug has a cheap local test. Timeline interactions have
  synthetic-pointer tests in `timeline_ui.rs`. Add cases there.
- Use `/interrogate` before shipping contested designs. In one-model
  sessions, apply its rubric directly and say so.
- `setup-pstack` targets Cursor model rules. It does not map onto Muse
  Code. Skip it here.
- Skills installed mid-session do not hot-load. Restart the session to get
  slash invocation.

The media bin and import pipeline live in `crates/app/src/app/media.rs`.
Chat editing lives in `crates/app/src/app/chat.rs`. Its HTTP call runs on
a worker thread in app code. Generated JavaScript still goes through
`scripting.rs` and mutates state only as `ScriptCommand`s. `addAsset`,
`addAssetRange` and `addTitle` return a handle (from `CLIP_HANDLE_BASE`,
never reused within one engine); each applied batch maps handles to the
clips it created, so later commands in the same run can target new clips
without guessing IDs. A handle from an earlier run is an error. Checked chat
evals use a fresh QuickJS runtime/context, so a retry can reuse
declarations without inheriting changed globals; the interactive REPL
still persists vars. Each eval can enqueue at most 2,048 commands and
1 MiB of owned command strings, and rejects nonfinite numeric inputs.
The VM, command callbacks, and eval completion check a cooperative
500 ms deadline; a single native operation cannot be preempted.
Chat and local-intent batches roll back timeline, selection, transport,
magnet, zoom, and music-bed policy on application errors. They preserve
the previous Undo on failure or a timeline-neutral reply. Application
checks a 250 ms deadline between commands; this is cooperative, not
preemption of a single native operation. Generated import/export is
rejected before any command runs; explicit console and GUI I/O remain.
A chat reply that makes no edits shows its console.log lines in chat
(questions, problems), not just the one-line summary. `setClipLane(id,
lane)` moves a clip between lanes; lane 0 is the visible picture, so a
cutaway over a long base clip moves the base to lane 1 first. There is no
video-over-video overlay.
Pending chat retains its request-time timeline and state JSON. Timeline
changes reject stale replies, selection/playhead defaults use that saved
state, and an explicit timeline clear cancels the pending request.
Chat session state (persisted history, pinned project summary, token
budget `TOKEN_BUDGET`, and compaction of older turns into one summary
message) lives in `crates/app/src/app/session.rs`, adapted from
Mitsuro's `mitsuro-client-state`/`mitsuro-core` and its compaction
research — state store and budget discipline only, not its
coding-agent loop. Requests are assembled developer contract →
few-shot → pinned summary → recent history → timeline JSON last, with
a per-project `prompt_cache_key`, so the cached prefix stays stable.
Editing requests omit repeated candidate transcripts, detailed visual
measurements, and review frame arrays. They retain every range and ID;
QuickJS still reads the full request-time snapshot. Requests that remain
over the estimated context budget after history compaction fail before
dispatch rather than silently dropping clips.
The model replies with a structured `{code, explanation}` envelope
(free-text parsing is a fallback). Only 429/500/503 are retried.
Built-in Meta choices use `muse-spark-1.3` and `muse-wonder-1.0`;
Wonder is the default (Spark's curation missed burned-in text and ran
about 85 minutes on 11 expo videos).
Settings load maps the old unversioned names to these API IDs, preserving
explicit versioned/custom IDs. The Contributor checkbox is disclosure
only; it does not automatically select a Contributor model ID.
Settings save uses a same-directory atomic replacement, private from
creation on Unix, and reports failures. Check connection runs off the UI
thread and validates authentication plus the exact selected model through
`GET /models`; it does not prove inference, ASR or SAM availability.
Results from changed settings are discarded. Keyless endpoints must be
parsed localhost or loopback addresses, not hostname-prefix matches.
Button intents are declared in `crates/app/src/app/intents.rs`: local
intents arrange sweep candidates offline through the same
`ScriptCommand` batch path; model intents are fixed prompts through
the chat loop. AI edits snapshot the timeline first for one-click
undo, and in-flight calls can be cancelled.
General undo (`app/history.rs`) compares the timeline each frame and
commits a step when the pointer is up and no field has focus, or 0.75 s
after typing stops, so every edit path is covered without per-path
hooks. Selection-only changes are not steps; 200 steps are kept.
Projects (`app/project.rs`, `.eddie` JSON, version 1) hold the timeline,
media paths and music-bed muting. Opening replaces the timeline and
history, keeps the bin, imports listed media and reports missing files;
saved IDs are never reused. Ctrl+S/Ctrl+O work in text fields; split's
S/B shortcut ignores Ctrl. Projects also store their own folder: a
missing file is retried at the same relative place beside the moved
project (not an edit), then File → Locate missing media relinks by hand,
including same-named files in the picked folder. Close, Open and New
project (Ctrl+N) ask "Save changes?" when the timeline differs from the
last save or open; New empties the timeline and bin except files still
importing or curating. Right-click a bin tile to remove it; media used
by a timeline clip, importing or curating is refused, and the file and
its saved curation stay on disk.
Clear timeline does not ask because it is undoable. CI
(`.github/workflows/ci.yml`) runs fmt and engine tests only.
The import QC sweep is the curator. It lives in
`crates/app/src/app/sweep.rs`, with motion sampling and segmentation in
`crates/media/src/sweep.rs`. Import only imports; curation never runs
automatically. The user selects a ready asset and presses Curate
(Curate all new covers a batch; Re-curate replaces prior results).
Up to cores/4 files (at most three) curate at once; review mostly waits
on the provider. Within a file, three review batches, then three scenes'
seam/timing passes, are in flight at once (`REVIEW_CONCURRENCY`); source
pictures for review batches decode in order on one sampler, each edge
worker has its own, and results apply in source order. Offline, with
250 ms of fake provider latency, a 120 s clip went from 32.6 s to 12.6 s
with the same 121 requests. The app peaked near 1.5 GB RSS while three local 4K
analyses ran on 16 cores. Each finished curation is saved, private, under `curation/`
beside the settings file and restored when the same path, size and
modification time is imported again. Restoring is not curating.
Scene boundaries are visual only: every frame is analyzed at 64 px
wide with pixel motion kept separate from global and 4x4-block
RGB-histogram scene scores. Histogram weights interpolate between
color-bin centers so small lighting changes do not become cuts.
High scene scores also check illumination-tolerant texture and coherent
background motion. This rejects the labeled flash and foreground-exit
false cuts without merging short shots. A defocused foreground passing
the lens is an excursion: a featureless run (strong-edge share under 2%,
at most 2 s) entered or left through a partly covered frame. Its
boundaries drop to the score between a detailed frame 0.5 s before the
run and a frame within 1 s after it, only when that across-run
comparison is not a cut. Textured inserts, featureless runs between
different views, and instant cuts to a flat card and back keep their
cuts. Similar shots and textureless flashes remain ambiguous; this is
not universal cut recognition.
The adaptive median/MAD baseline uses neighboring score maxima to
handle repeated-frame cadence, and needs eight history samples.
The block term is load-bearing for same-palette montage shots in
one location. Audio never creates a boundary. It only supplies energy,
peak, coverage, music-bed, waveform silence, and transcript evidence for scoring and
classification. Per-frame local visual evidence also records
foreground coverage/centroid travel, center saliency, edge density,
luminance, contrast, and a broad skin-tone pixel ratio. Those are
measured proxies, not a person/face/expression detector.
Curation returns two distinct lists, neither capped by clip count.
`scene_clips` is the complete source-ordered detected-shot inventory,
contiguous from zero to the asset duration, including one-frame inserts
at their exact source PTS. `candidates` contains ranked workable ranges;
without model access, every physical shot remains a local suggestion.
Long takes are not arbitrarily chopped into eight-second candidates.
Decode or scaling errors fail scene analysis instead of claiming that
partially analyzed footage has complete detected coverage.
The central Program/Gallery tabs keep browsing separate from the timeline
preview. Curate opens Gallery and pauses playback; completion does not
override a manual return to Program. Gallery lives in `app/gallery.rs`.
Gallery uses a smaller default timeline height and remembers its resize
separately from Program so browsing leaves room for complete clip cards.
Its Clips grid combines text search, source, category, quality/review and
duration filters. Best first is the default and ranks usable picture above `on-screen-text`
ranges, then Needs review or rejected ranges, each by score; source order,
shortest and longest are alternatives. Search matches words across names, titles,
categories, tags, review notes and transcripts. Add shown uses exactly
the displayed ranges and order. All scenes ignores Clip filters and stays
complete and source-ordered, including rejected material. Reset clears
Clip filters and restores Best first without changing the selected source.
Virtualized cards request source-range midpoint thumbnails lazily through
one native still worker, with a shared 256-entry success/failure cache;
audio-only ranges show their waveform. Compact layouts group filters in
a popup and show the number of active type/quality/duration filters.
JS/chat expose all assets through `sweepCandidates()` and `sceneClips()`
independently of the UI filter. Dismiss hides a suggestion, not its source.
Asset tooltips state which stages ran or failed, counts, coverage, and
reported editorial-review and SAM usage. Review tokens include refinement;
SAM reports requests, completions, failures and provider-metered frames/images.
Those totals exclude ASR and retries whose usage the provider did not report.
Every candidate/scene clip carries one category (`reaction`,
`talking`, `action`, `broll`, `establishing`), tags from the
`CURATOR_TAGS` vocabulary (including `subject-centered`,
`subject-large`, `subject-moving`, and `face-visible`), quality subscores
(stability, audio, motion), a cross-modal `talkingScore`, a `visual`
evidence object, and `review` metadata. Prime is a quality tag, not a
category: top 20% of candidates per asset at score 75+, assigned by the
app after ranking, never by the model or to Needs review material.
General-production tags include `detail`, `wide-shot`, `demonstration`,
`interaction`, `reveal`, `subject-entering`, `subject-exiting`, `occluded`,
`camera-move`, `expressive`, and `on-screen-text`. The reviewer tags
burned-in titles, day cards, lower thirds, news bars, subtitles and end
cards from an earlier edit (not small logos filmed in the scene) and
splits them from clean picture. Local montage buttons skip those ranges,
and chat is told not to place them unless the user asks for the text. These describe observed content, not
guaranteed emotion, action completion, or subject identity.
Reaction filtering uses the category, not reason substrings or soundtrack peaks.
Local classification derives categories and tags from measured signals.
With model access, `sweep/curation.rs` sends the selected Meta model
timestamped JPEG sequences and transcripts in 24-second review windows
with context, anchored to the shot origin so fractional-frame rounding
cannot accumulate into an extra one-frame window. Requests contain at most
40 images and eight windows; those
are request-size limits, not extraction caps. The reusable native
`EvidenceSampler` in `crates/media/src/evidence.rs` supplies 384px JPEGs
and actual normalized PTS without a per-still subprocess. Discovery requests
frames at intervals of at most one second, more densely for short windows. Its bounded
`frames` API uses one seek followed by sequential decoding, returns at most
40 distinct source pictures over 30 seconds and 16 MiB of JPEGs, and shares
half-open presentation-interval selection with analysis video sampling.
`curation/orientation.rs` first asks the selected model for a small routing
description and optional concrete subject checks. It cannot reject footage;
an invalid check (unknown edge ID, unshown anchor) is skipped, not the window.
The substantive review then receives the complete overview and transcript,
plus any SAM evidence; SAM is not a gate that hides untracked source ranges.
Reviews can select multiple useful ranges inside one shot and must account
for every other interval as rejected or Needs review. Integer edge IDs map
to host-owned source PTS; source bounds, complete coverage, ordering,
categories and continuation flags are validated. Metrics are recomputed
for each range. Editorial scores use a declared 0–100 scale.
`curation/seams.rs` reconciles useful beats across analysis windows using
shared context. Explicit same-shot, same-category continuations join;
otherwise the model can join, separate or preserve uncertainty. A window
clock is not a physical cut. `sweep/curation/timing.rs` verifies interior
editorial edges against source pictures sampled about every 0.1 seconds
within 0.75 seconds of each proposal, can request one inspection out to
two seconds, then resolves adjacent source pictures inside the chosen bracket.
The model chooses supplied
integer frame choices, not invented times; out-points use the selected
picture's exclusive end. Adjacent searches meet at proposal midpoints,
and each moment retains an original midpoint picture, so choices cannot
cross or erase short moments. A shared split moves both edges once.
This pass cannot discover missing moments, change categories, or move
physical scene boundaries. An invalid or missing decision keeps that
boundary's proposed edge. A failed seam keeps both window ranges
unreconciled; a failed timing pass keeps the validated pre-timing ranges.
Either way `review.edgeIssue` says the edges were not reconciled or
frame-verified, the note reports it, and coverage, ordering and
non-overlap still hold. Talking edges are repaired the same way before
the timing pass and on that fallback: they move out of any available
utterance, to its end plus a silence handle or back to the silence before
it. They may take frames from a Needs review neighbor, or from a touching
non-talking cutaway that keeps at least min(1 s, half its length); a pull
back must keep half the talking range. A range that cannot becomes Needs
review with its metadata. Only an invalid or missing
editorial review preserves the whole window as Needs review.
More
than eight boundaries create more bounded requests, not fewer clips.
Timing usage is included in review totals, with separate timing batch and
boundary counts and per-stage usage. Built-in Meta models use low reasoning effort and an
8,192-token completion allowance for this constrained choice pass; the
allowance includes hidden reasoning, not just the visible JSON.
Physical shots remain available even when the model rejects their content.
The editorial review allows 8,192 completion tokens per batched window plus
one more allowance, capped at 32,768; hidden reasoning counts against it.
Missing or invalid reviews preserve Needs review ranges. A provider failure
stops new requests while retaining completed reviews; requests already in
flight still finish. Sparse image
sampling and model scores are not proof of every meaningful moment.
Candidates are suggestions only; the source asset stays whole.
Voice Transcribe returns speaker turns, never word timings (Meta's docs
say so for every mode). Meta stays the transcript: text, speakers, turns.
`sweep/align.rs` adds word timing locally with whisper.cpp and the
multilingual base model (`ggml-base.bin`, MIT): `EDDIE_WHISPER_MODEL`, or
`whisper/ggml-base.bin` beside the settings file (here a symlink to
`/mnt/data/models/whisper`). It runs only on turns of 2 s or more with a
sentence end before their last word, without a prompt (prompting with
Meta's text made the base model hallucinate). Heard words are matched to
Meta's words by longest common subsequence; Meta's words and punctuation
stay authoritative, unmatched words are interpolated and `estimated`, and
a turn under half matched keeps turn timing. Each sentence break between
two heard words snaps to the quietest 20 ms within 0.25 s, searching past
word midpoints but leaving each neighbor 40 ms. Speech grouping
splits a turn at those breaks only, never at estimated words. Without the
model, curation keeps turn-level timing. Tests ignore the installed model
unless `EDDIE_WHISPER_MODEL` is set. `timingPrecision` is `turn` or
`word`. ASR session identity and source intervals
scope speaker labels; a turn touching an internal ASR chunk edge is not
evidence of a whole utterance. Speech selections retain available complete
turns with silence handles. Only the `talking` category owns its turns;
narration tagged `speech` under b-roll, action or establishing pictures is
cut on the picture. Overlapping turns and one speaker's touching turns
form one utterance. An unfinished turn (trailing comma or ellipsis,
punctuated text without a terminal mark, or a dangling word such as
"but"/"and") also joins a turn that follows within 0.8 s, even across a
speaker change. A finished turn ends at a speaker change, so a handover
without a pause is a cut point. ASR sometimes stops punctuating; fully
unpunctuated text counts as finished unless it ends on a dangling word,
so conversation does not chain into one long utterance. Another
speaker's short backchannel (at most three words and 1.2 s, such as
"Uh-huh.") neither ends nor completes an unfinished thought. Turn-level
ASR edges are not frame-accurate: a picture edge within 50 ms of a turn
boundary is on it. A turn of 2 s or more with under 1.5 letters per second
(crowd ambience such as "LT" across 16 s, "[Music]", sparse lyrics) is not
an utterance: it neither blocks edges nor counts as missing timing, so a
talking range over it is cut on the picture. Adjacent selected beats without a source-picture
edge between their turns join rather than inventing word cuts; explicitly
omitted gaps do not join. Missing transcription is uncertainty, not silence.
Audio-only imports get a steady-tempo `BeatGrid` (`crates/media/src/beats.rs`)
from those peaks: comb search over 70–180 BPM, a weak 120 BPM octave prior,
then a line fit through onset-snapped beats so long tracks do not drift.
It requires comb contrast 3x the mean onset and onsets near 75% of beats;
otherwise `tempo`/`beats` are null. Tempo changes are not followed.
Waveform peaks are imported at
50/sec and are never stretched to container duration. Model-enabled
curation transcribes with `muse-voice-transcribe-1.0`; continuous music
hints do not gate transcription because continuous speech can look similar.
Reviewed-range audio evidence overrides whole-shot hints where it overlaps.
Asset state exposes silence ranges, and `tightenSilence`
removes pauses from speech, ripples later clips once, rebuilds linked
detached audio in matching phrases, and rejects mostly music-bed
clips. Editing chat sees compact metrics and transcript text, not frames;
visual curation is the separate image-bearing request path.
SAM 3.1 is wired through Meta's Responses API (`sam-3.1`) in app
sweep code. It is part of Curate's orientation/review loop, not a separate
user action or an after-the-fact quality bonus. Orientation can request
up to two concrete subject checks per window, each with a concept, source
interval and editorial question. An observed picture and normalized box can
anchor a specific subject instead of a larger distractor; otherwise SAM
searches the concept. Anchored video tracking runs in both directions.
Concepts are not limited to people/faces.
The native sampler makes an analysis video at roughly eight source samples
per second (plus the last source frame); one-frame checks use a JPEG.
Actual source PTS, not proxy playback time, maps every returned observation.
Analysis copies are capped at 256 frames, 30 seconds and 16 MiB; each uses
one native decoder and one bounded-thread encoder, not a subprocess per frame.
Up to three temporally spread rectangular detail crops per check and
range-scoped track summaries return to the selected model's substantive
review, alongside the full overview. No second full draft or recursive checks
are allowed. Empty detections are uncertainty, not an editorial rejection.
Failed evidence or invalid review preserves the whole window as Needs
review. Completed physical scenes remain available regardless of selection.
Track IDs are request-local. Source-range metadata shares observations and
derives scoped summaries; editing chat omits detailed tracks and crop arrays.
Representative box/face fields and `face-visible` remain available, but box
area, centering and region sharpness/exposure never add quality points.
Eddie does not decode SAM's arithmetic-coded mask payload: `samMaskCoverage`
and expression remain `null`. Rectangular proxies are not mask measurements,
and a face box is not expression, lip-sync or active-speaker proof.
Incomplete/failed streams, conflicting final text and malformed box records
are errors, not negative detections. A later successful check cannot hide an
earlier failure. Model tags cannot assert `face-visible`; that needs SAM evidence.
The official mask decoder's SAM license needs an explicit licensing decision
before copying/porting it or adding it as a dependency; no decoder is shipped.
Without a key (or after a SAM error), local visual proxies remain and
`samStatus` says `unavailable` or `error`. The in-process
`SubjectSegmentationProvider` remains only a local-runtime boundary.
Media paths passed as command-line arguments are imported on launch.
Timeline lanes are free-form and kind-agnostic. Projects start with one
lane; dragging or dropping above or below grows new lanes, gaps are
allowed anywhere, and adds land on the first lane free at that span
(or the dropped lane). Media adds preserve exact start, source-in, and
positive duration, including sub-project-frame scenes. Generated clips
and explicit move/trim/split edits still use the project frame grid.
Viewing Details does not snap starts or expand short scene durations;
its timing controls clamp only on interaction.
Linked detach/split/close-gap operations preserve media coverage and
alignment. Output samples at the project frame rate; a shorter shot can
remain in the timeline without appearing in a lower-rate render.
Timeline hit-testing reserves a 12-point move body; narrow clips have no
trim handles until zoom makes room. Final release coordinates apply to a
drag, and rejected placements restore exact home bounds without lane growth.
Adjacent unequal-length swaps validate linked partners atomically and preserve
outer span, gaps, durations and source coverage; the internal reorder edge
can remain off the project grid. Free moves/trims still use that grid.
The magnet compares actual edge matches, excluding the moving linked partner.
It only snaps edges while dragging; it
never ripples clips closed (`close_gaps` runs only from the explicit
script `CloseGaps`). It removes time no video or audio covers and shifts
every lane together, so captions and titles stay over their pictures;
closing lanes independently desynced them. Split is playhead-based (toolbar
Split, S or B); clicking a lane parks the playhead there. There is no
split tool mode and no in-app sample project (`loadSample` is gone);
`samples/` exists for tests. Import batches queue instead of being
refused while one runs. Dragging from the bin carries a grid-size
thumbnail ghost at the cursor plus a drop-lane highlight.
Music-bed muting on curator adds is user-selectable (Curator checkbox,
`state().musicBedMuted`, `setMusicBedMuted(bool)`); it defaults on.
It mutes only video ranges; a curated audio-only track is the new
soundtrack and always adds audible.
Clips carry linear audio fades (`fade_in`/`fade_out` seconds, `setFade`,
and a Fades row on audio clips in Details). `fit_fades` scales fades
that together exceed the clip by their requested ratio. The preview mixer
and export's `afade` share that rule; split keeps the head fade left and
the tail fade right, and faded clips never coalesce in export.
Video audio can be detached into a linked audio clip. The picture's
`embeddedAudio` turns off so preview/export never double the source;
linked pairs move, trim, split, and close gaps together until
explicitly unlinked for J/L cuts or replacement sound. Titles and
generated captions share `TitleStyle` (`lower-third`, `caption`,
`pop-caption`, `minimal-caption`) across preview and export. One table,
`openreel_media::title_look`, holds each style's size, colour, box and
placement in export-canvas pixels; the monitor scales it to its frame.
Formats never mix. `Timeline::orientation` (Settings Format, default
landscape, saved as `format` in config; cleared projects keep it) fixes
the canvas. Video of the other displayed orientation (square counts as
landscape) is refused by every add path, skipped by bulk adds with a
count in the status, dimmed in the bin and disabled in the gallery;
audio-only media fits either. Changing the format leaves placed clips and
reports how many clash. Preview and export share `export_canvas`: the
largest source of the project orientation sets the size (width capped at
1920), else 1280×720 or 720×1280. `Timeline::active_video`/`active_title`: the
lowest video lane wins (insertion order breaks ties), its opacity fades
against black rather than revealing another lane, and the first active
title wins. All audible clips still mix independently of picture visibility.
Export streams raw video batches into one H.264/AAC encoder rather
than sharing decoded frames across clips on different source clocks.
Batches use at most four independent video inputs and eight seconds;
decoder, filter, and final encoder parallelism are bounded. A branch
starts at its first visible output frame, not at batch time zero, and
stops at its last one (decoding to the batch end was most of the 4K
work). Inputs use slice threads: frame threads on four 4K inputs reached
about 2.8 GB RSS. A 60 s, 29-cut montage from 4K sources exported in
46.6 s (was 103.6 s).
Titles and audio remain on the whole timeline clock. Existing export
resolution, CRF, and audio settings are unchanged. Batch bytes and
child exits are checked before export can report success.
SDR batches convert source YCbCr matrices and ranges to limited-range
BT.709 with accurate rounding and full chroma interpolation (without it,
BT.601 phone footage shifted green by about 1.3 levels on textured
pictures; flat test colors hide this). The raw-video encoder input declares that
same representation so the output keeps its color tags. This is not an
HDR tone-mapping or wide-gamut color-management pipeline.
Video sample boundaries and source offsets share one integer tick clock;
floating-point noise from subtracting source timestamps must not add a frame.
The finite raw-frame stream bounds video; audio is trimmed separately
to the timeline end. An off-grid end retains its final active video
sample, whose full CFR interval can extend past the timeline by less
than one frame. Do not add a final encoder `-t` that rounds away that sample.
Display rotation (phone portrait footage) is honored everywhere: `probe`
reports displayed width/height, and the native decoder rotates quarter and
half turns while copying out, matching the FFmpeg CLI's autorotate.
Native seeks select presentation intervals `[PTS, next PTS)`, normalize
source time to the container origin, and drain delayed frames at EOF.
Playback discards seek preroll and paces from the requested source-in.
Preview uses one latest-generation worker with two decoder lanes. While a
clip plays or buffers, the idle lane prerolls the next visible cut's first
picture; a request at that file/time hands over to it and shows that picture
without decoding further. A cut that continues the active file forward stays
on the active decoder and decodes forward when no keyframe seek would start
later. Each lane holds one native decoder (about 100-190 MB at 1080p-1440p
on 16 cores). A lane that switches files parks its decoder with its position
and undelivered pictures (at most four, least recently used evicted), so a
re-entered file decodes forward instead of from its keyframe. Only machines
with fewer than eight cores park: decoder memory grows with frame threads. Pause and seek
release the idle lane's and every parked decoder on a lane thread. On ten
0.3 s cuts across five 1080p-1440p files, parking cut buffering from 2.8 s
to 1.35 s on two cores (+90 MB RSS) and 1.1 s to 0.42 s on four (+140 MB);
on eight cores it saved 0.04 s for 200 MB and on 16 nothing for 300 MB. Preroll remains
CPU-bound. Clip switches and seeks
do not wait for native open/seek/decode; obsolete frames and errors are ignored.
Decode completes a captured clock target before chasing a newer one, preventing
moving-clock starvation. Audio plays in process (`crates/media/src/audio.rs`):
one mixer at 48 kHz stereo, one ffmpeg-next decode/swresample thread per
audible clip, with clips starting within 0.75 s prerolled. The samples the
mixer renders are the transport clock; the playhead reads it less ALSA's
reported delay, which includes PipeWire's graph quantum (about 21 ms here).
Keep cpal >= 0.18: older cpal used alsa 0.9.1, whose status struct reads as
zero in release builds, so the delay was silently lost. Silent held blocks do
not restart that interpolation, so a held clock reaches its limit (and the
timeline end) once the queue drains; between seeks it never steps back. The app gives the mixer
a limit: the next visible cut, or the one after when its first picture is
prerolled. Rendering never passes the limit, so a buffering cut holds sound
and picture together instead of consuming an unseen short clip; the playhead
still lands on each cut for one tick so the clip is requested at its first
picture. A request served from a ready preroll counts as ready at once;
waiting for its upload stalled audio already rendered past the cut by one
device block (a 43 ms dropout about 0.1 s into each prerolled cut). A voice under the clock with no decoded data holds the clock too.
Without a device (headless, `SDL_AUDIODRIVER=dummy`) a paced null output
keeps the same clock silently; app tests use the deterministic manual output.
A PipeWire null-sink loopback measured audio reaching the sink 0.0-0.2 ms
after the clock crosses it (30 clicks, including after a 0.5 s hold; it was
21.4 ms, one quantum, before the cpal fix). Hardware output latency beyond
the sink is not measured or compensated. This does not make CPU-bound preroll smooth. Decode failure pauses with an error, not an automatic retry loop.
Native calls themselves are not preemptible.
`addCaptionsForClip` and the inspector generate readable caption
clips from a placed clip's transcript, using word timing when
present and turn-proportional estimates otherwise.

## UI rules

- shadcn is the design standard. Tokens and button variants live in
  `crates/app/src/theme.rs`. Use them. Do not hand-roll new colors.
- No verbose tags, badges, or explainer paragraphs in the UI. A control
  gets a short label. Explanation goes in a hover tooltip.
- Every button needs a hover tooltip that says what it does and its
  shortcut if it has one.
- Feedback lives in the status bar. Keep messages to one short sentence.
- Importing shows a placeholder card immediately with the file name and
  known metadata. Up to four files import at once. Filmstrip thumbnails
  are keyframe stills from one slice-threaded native decoder (exact
  pictures only when a keyframe is over one interval away); 13 expo files
  imported in 6.8 s, down from 72.4 s. The thumbnail area stays greyed with a spinner until
  probe, waveform, and thumbnails finish. Placeholders cannot be added
  to the timeline or curated. A finished import selects itself only when
  no asset is selected; it never replaces the user's selection.
- Buttons are borderless at rest. Hover and pressed fills carry the
  affordance. Do not reintroduce hard outlines.
- Surfaces: `BACKGROUND` is the stage (program monitor, gallery, lanes);
  docked panels and bars use `SURFACE` via `theme::panel_frame()`;
  grouped blocks use `theme::card_frame()`. `BRAND` blue marks selection
  only. Tabs use `theme::segmented`. Everyday actions use the filled
  `secondary_button`; one `primary_button` per area. Inter loads from
  `Inter.ttc` (Regular plus SemiBold via `theme::semibold()`).
- The media bin is an image grid. Thumbnails and waveforms only. Names,
  metadata, and paths live in the hover tooltip.
- Agent control goes through the QuickJS console in
  `crates/app/src/scripting.rs`. JS enqueues `ScriptCommand`s. It never
  mutates editor state directly, gets no OS or filesystem access from the
  sandbox, and runs under memory and time limits.

## Performance rules

- The target is a responsive editor on weak hardware. Judge with
  `--release`. The status bar shows UI frame ms and RSS. Keep both down.
- No frame-sized allocations per frame at steady state. Pool buffers.
- No process spawn per frame. Reuse decoders. Coalesce stale scrub work.
- Never buffer a whole media decode in memory. Stream it.
- Scrub at low resolution while dragging. Settle at full preview size.
- Export quality is not the place to save time. Preview is the proxy.

## Testing rules

- The chat-edit dev harness lives in `crates/app/src/dev_harness.rs`
  (test-only; `dev-harness` feature enables the `muse exec` driver).
  `cargo test` runs the canned scenarios offline. Live stand-in runs
  need `OPENREEL_LIVE_HARNESS=1`. It never validates Spark/Wonder;
  the live-API checklist is at the top of that file.
- Bug fixes start with a failing test when a local test path exists.
- Media changes need the end-to-end test in `crates/media` to pass. It
  generates samples, decodes, streams, exports, and re-probes the output.
- Screenshot the real window before calling UI work done.
- "It compiles" is not verification. Run the feature surface.
