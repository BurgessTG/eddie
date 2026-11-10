# Editing with Eddie from an agent

Eddie has no AI inside it. You, the agent, do the thinking; Eddie is the
editor you drive. It runs locally and does the same thing every time.
This guide covers the command line and the JavaScript API. It started
from the rules the removed built-in chat followed; each of those rules
fixed a real mistake.

## The loop

```sh
eddie new talk.eddie footage/*.mp4 music.wav   # create a project, importing media
eddie transcribe talk.eddie                    # speech to text, saved per file
eddie curate talk.eddie                        # shots and ranked ranges, saved per file
eddie state talk.eddie                         # everything a script can read
eddie strip talk.eddie a.mp4 a.jpg             # 16 pictures through one source
eddie frames talk.eddie a.mp4 12..14 shots/    # timestamped pictures of a source range
eddie run talk.eddie edit.js                   # one edit: all of it or none of it
eddie inspect talk.eddie                       # measured problems in the timeline
eddie sheet talk.eddie sheet.jpg               # the edit as pictures
eddie export talk.eddie out.mp4
```

Every command prints one JSON object. Errors print `{"ok":false,"error":...}`
and exit 1. `eddie help` lists the commands and the script API.

After `run`, read its `introducedFindings` (or run `inspect`), look at a
`sheet`, and fix what the edit broke. `--dry-run` reports an edit without
saving it.

## A running editor

While a project is open in the Eddie window or in `eddie serve`, every
command goes to that editor instead of the project file. You call it the
same way; replies add `"live":true`.

- `eddie serve talk.eddie &` keeps a project open without a window. Start
  it when you will send more than a few commands: media stays loaded, so a
  command takes about 50 ms instead of about a second. It prints one line
  when it is listening and saves every change. `eddie stop talk.eddie`
  ends it.
- In the window, the user watches your edits arrive, and each `run` is one
  Undo step in their history.
  - The window saves only when its user does, so replies say
    `"saved":false`. Run `eddie save talk.eddie` when the user wants your
    edits on disk.
  - The user edits too. Read `state` again before each edit instead of
    reusing clip IDs from an earlier call.
  - An edit waits while the user is dragging something.
- `eddie undo talk.eddie` and `eddie redo talk.eddie` step the running
  editor's history, the user's edits included. Without a running editor
  nothing remembers earlier commands: try an edit with `--dry-run`, or
  reverse it with another script.
- `eddie events talk.eddie` prints one JSON line per change until the
  editor closes: `timeline` (`revision, clips, duration`, whoever made the
  change), `import`, `curation`, `transcription` and `export`.
- One editor holds a project at a time. A window that opens a served
  project takes it over; a second `serve` is refused.

A program can use the socket itself instead of running `eddie` per
command. `serve` prints its `socket` path. Send one JSON object per line,
`{"id":1,"method":"run","code":"split('clip:7@50%')"}`, and read lines
until one has your `id` and `ok`; lines with `progress` or `event` come in
between. The methods are the commands, with these parameters:

- `run`: `code`, `dryRun`.
- `import`: `media` (absolute paths).
- `transcribe`: `assets`, `force`, `provider`, `model`, `language`,
  `vocabulary`. `curate`: `assets`, `force`.
- `sheet`: `output`, `range`. `strip`: `asset`, `output`. `frames`:
  `asset`, `range`, `directory`, `every`, `width`. `export`: `output`.
  Output paths are absolute.
- `state`, `inspect`, `undo`, `redo`, `save` and `stop` take none.

## Pictures

JSON does not show what a shot looks like. Three commands write JPEGs for
you to open; each also prints the times it pictured.

- `eddie sheet <project> <out.jpg> [range]` shows the program: one cell
  per visible shot, labelled with its timeline time, clip ID and lane, and
  with titles drawn on the picture.
  - A sheet has 16 cells at most. When the timeline has more shots, the
    cells are spread evenly and the JSON says `sampled: true`. Give a
    timeline range (`10..25`, `clip:7`) to see every shot of one part.
  - `cells` lists `time, clip, lane, sourceTime, title` for each picture.
- `eddie strip <project> <asset> <out.jpg>` shows one source from start
  to end in 16 pictures, labelled with source times. A `*` marks a time
  the timeline already shows.
  - `pictures` lists `time, placed, shot [start, end]`. `placed` lists the
    source ranges on the timeline.
  - `shotsWithoutPicture` counts shots the strip skipped. Use `frames` on
    the gap before placing anything from one.
- `eddie frames <project> <asset> <range|time> <out-dir>` writes one JPEG
  per picture, named by its real source time (`a-00012.500s.jpg`).
  - Times are source times. `clip:7` means the source range clip 7 shows.
  - Without `--every <seconds>`, 16 pictures are spread over the range.
    One call gives 40 at most. `--width <px>` sets the size (384 by
    default, 64 to 1280).
  - Use it to find the exact frame for a cut, or to read text on screen.

## Scripts

`run` evaluates JavaScript in a fresh QuickJS sandbox. The sandbox has no
filesystem, network or OS access, and limits of 500 ms, 2,048 commands
and 1 MiB of strings. Queries read the project as it was when the script
started; edits are queued and applied together at the end. If any edit
fails, the project is left exactly as it was.

- `return` a value or `console.log(...)` to report back. Both show up in
  the JSON `run` prints.
- `importMedia` and `exportTo` are refused here. Use `eddie import` and
  `eddie export` instead.

### Reading the project

`state()` returns `{playhead, selected, duration, fps, magnet,
musicBedMuted, format, clips, assets, sweepCandidates, sceneClips,
transcriptSegments, inspection, audio}`. The shortcuts `clips()`,
`assets()`, `sweepCandidates()`, `sceneClips()`, `transcriptSegments()`,
`inspect()` and `audioReport()` return its parts.

- **Clips:** `id, name, kind ('video'|'audio'|'title'), track, start,
  duration, sourceIn, source, opacity, volume, fadeIn, fadeOut,
  smoothFades, duck, cleanVoice, embeddedAudio, linkedClip, titleStyle`.
- **Assets:** `name, path, duration, width, height, hasVideo, hasAudio,
  orientation, ready, silenceRanges [{start,end}], utterances
  [{start,end,text}], tempo, beats`.
  - `tempo` (BPM) and `beats` (source seconds) are set only for
    audio-only music with a steady beat; otherwise they are null.
- **Transcript segments:** `id, asset, assetName, start, end, speaker,
  text, timingPrecision ('turn'|'word'), words [{start,end,text,
  confidence, estimated}], asrSession`. Speaker labels mean something only
  within one source.
- **sceneClips** list every physical shot of each curated file, back to
  back, in source order. A file is curated by `eddie curate` or the
  window's Curate button; until then it has no scenes or candidates.
- **sweepCandidates** are the same ranges ranked by score, with no cap on
  how many.
- **Each range has:** `id, asset, assetName, sourceIn, duration, score,
  reason, category (reaction|talking|action|broll|establishing), tags,
  musicBed, quality {stability, audio, motion}, transcript, talkingScore,
  visual, review {status, model}`.
  - New curations are local measurements, not judgments; `review.status`
    is `local`.
  - Curations saved by the old model reviewer can still say `reviewed`,
    `needs-review` or `rejected`.

### Time

Every timeline time argument takes seconds or a spec string: `'3.2s'`,
`'120f'`, `'1:05.5'`, `'50%'`, `'end'`, `'clip:7@end'`, `'clip:7@1.5s'`,
`'clip:7@50%'`. Ranges are `'a..b'` or `'clip:7'`. `time(spec)` and
`timeRange(spec)` resolve them. Clip specs name the IDs in `state()`, not
handles.

### Editing

`selectClip, split(at), splitClip(id, at), deleteClip(id?), moveClip(id,
start), setClipLane(id, lane), trimClip(id, left, right)` (-1 keeps an
edge), `renameClip, setOpacity, setVolume, setFade(id, in, out),
setSmoothFades, setDuck, setCleanVoice, insertTime(at, seconds),
detachAudio, unlinkClip, setEmbeddedAudio, tightenSilence(id, threshold?,
minSilence?, padding?), addTitle(text, at?, dur?, style?), setTitleStyle,
addCaptionsForClip(id, style?), addAsset(nameOrPath, at?),
addAssetRange(nameOrPath, at, sourceIn, dur), setMagnet, setMusicBedMuted,
closeGaps()`.

## Rules that prevent the usual mistakes

**Clip IDs**

- Never compute or guess a clip ID. `addAsset`, `addAssetRange`,
  `addTitle`, `split` and `splitClip` return a handle for the clip they
  create (for a split, the right-hand piece). Pass that handle to later
  calls in the same script.
- Handles from an earlier run are invalid. Read the new IDs from `run`'s
  `handles`.

**Lanes and cutaways**

- Where video overlaps, the lowest lane is the picture shown, and lane 0
  wins. A clip below full opacity fades to black; it never shows the lane
  underneath. There is no picture-in-picture.
- To cut away from a long take and keep its sound:
  1. `setClipLane(base, 1)`.
  2. Add the cutaway.
  3. `setClipLane(cutaway, 0)`.
  4. `setEmbeddedAudio(cutaway, false)` if the cutaway's own sound is
     unwanted.
- To make room for an inserted shot, use `insertTime(at, seconds)` rather
  than moving clips one by one. It splits everything crossing `at`
  (titles keep their text on both sides) and moves every lane later.
- `closeGaps()` removes only time that no video or audio covers. Music
  spanning a picture gap keeps that gap open.

**Picking source ranges**

- Only place a range you can vouch for: a scene, candidate or transcript
  segment that describes it, or a `strip` or `frames` picture you looked
  at. Never guess a source time.
- Keep each range inside its source; `inspect()` reports one that runs
  outside.
- To use "the part where she talks about pricing", find the transcript
  segment whose text matches. Place it with `addAssetRange(segment.asset,
  cursor, segment.start, segment.end - segment.start)`.
- Cut speech between utterances, not inside them:
  - `utterances(asset)` lists the spans that should stay whole.
  - `snapToSpeech(asset, start, end)` widens a range out of any
    utterance, with 0.12 s handles, without running into the next one.
  - The grouping knows these cases:
    - a dangling "but" or "and" continues into the next turn;
    - another speaker's "Uh-huh." doesn't end a thought;
    - a handover after a finished sentence is a cut point;
    - a heard sentence end inside a long turn is a cut point;
    - crowd noise transcribed as "LT" or "[Music]" is not speech.
  - Only `timingPrecision: 'word'` supports word- or filler-exact cuts.
    Turn times only locate a passage.
- Ranges tagged `on-screen-text` contain burned-in titles, captions or
  news bars from an earlier edit. Don't place them unless that text is
  wanted. Local curation does not detect burned-in text; only curations
  saved by the old reviewer carry this tag.
- Video whose orientation differs from the project `format` can't be
  added.

**Sound**

- `musicBed: true` marks a range that probably has music baked in.
  - Judge those ranges on their picture, not on their sound.
  - While `musicBedMuted` is true, such video is added muted so old
    soundtracks don't stack. Audio-only tracks always stay audible.
- Music added on its own ducks under speech by default. A voiceover
  doesn't duck.
- Export normalizes to -16 LUFS, so balance clips against each other.
  - `audioReport()` gives each clip's loudness within the mix.
  - A clip 10 LU quieter than its neighbours is buried.
- `tightenSilence(id)` removes pauses from a speech clip. It refuses
  clips that are mostly music bed.
- Waveform silence finds pauses, not fillers. Decide fillers from the
  transcript text.

**Checking your work**

- `inspect()` findings are facts, not taste. A deliberate gap or flash
  can stay. The kinds are:
  - missing media and orientation clashes;
  - source ranges outside their source;
  - fades longer than the clip;
  - placed on-screen text;
  - black gaps without a title;
  - flash frames (visible for under two frames);
  - near-match cuts that skip or repeat 1–3 frames of the same shot;
  - clips hidden by a lower lane.
- `state()` doesn't show what the edit looks like. Before calling an edit
  done, open a `sheet` and compare each cell with what you meant to place
  there. Past 16 shots, sheet the timeline in ranges.
  - Without vision, say that the pictures were not checked.

## Recipes

- **Montage from the sweep:** clear the timeline, then place the
  highest-ranked video `sweepCandidates()` one after another from 0 with
  `addAssetRange(c.asset, cursor, c.sourceIn, c.duration)`.
- **Rebuild an edited video shot by shot:** use `sceneClips()`, grouped
  by asset and sorted by `sourceIn`.
- **Recap across places:** group candidates by asset (the asset name often
  carries the place). Open each group with an `establishing` or `prime`
  range, then vary `action`, `talking` and `reaction`, adding a short
  title card when the place changes.
- **Cut to the beat:** put each music-relative cut on one of the music
  asset's `beats`.
