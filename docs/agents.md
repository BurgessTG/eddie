# Editing with Eddie from an agent

Eddie has no AI inside it. You, the agent, do the thinking; Eddie is the
editor you drive. It runs locally and does the same thing every time.
This guide covers the command line and the JavaScript API. Each rule in
it fixed a real mistake.

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
and exit 1. `eddie help` lists the commands and the script API; `eddie
guide` prints this guide as written for the installed version.

- `curate` also transcribes each file that has no saved transcript, with
  the Speech provider from Settings. Run `transcribe` first to choose the
  provider, model or language.
- `transcribe` and `curate` list each file under `assets`. `reused: true`
  means a saved result answered; `--force` computes it again. `curate
  --force` keeps a saved transcript; `transcribe --force` replaces it.
- Read each file's `note`. A `curate` note with "Transcription failed: …"
  means the file was curated without speech data, not that it is silent.
  Fix the cause, run `transcribe`, then `curate --force`.
- `new` and `import` list `media.imported` (paths) and `media.failed`
  (`[{path, error}]`). A failed file is not in the project, unless an
  earlier import read it: then the project keeps what it read then.
- A reply's `saved` says whether the project file was written.
- `run` and `import` on one project take turns, each waiting up to 60
  seconds for the one before it. Starting several at once loses no edit.
- An unknown command is an error too; it never opens the window.

After `run`, read its `introducedFindings` (or run `inspect`), look at a
`sheet`, and fix what the edit broke. `run --dry-run` keeps nothing: its
reply adds `clips`, the timeline the edit would leave.

## A running editor

While a project is open in the Eddie window or in `eddie serve`, every
command goes to that editor instead of the project file. You call it the
same way; replies add `"live":true`.

- `eddie serve talk.eddie &` keeps a project open without a window. Start
  it when you will send more than a few commands: media stays loaded, so a
  command takes about 50 ms instead of about a second. It prints one line
  when it is listening and saves every change. `eddie stop talk.eddie`
  ends it.
  - A reply with `"saved":false` and a `saveError` means the edit is in
    the editor but the file could not be written. Do not send the edit
    again; fix the cause, then `eddie save talk.eddie`.
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
  - "… is open in an Eddie this command cannot reach" means an editor
    holds the project where this command cannot find its socket. Run the
    command with that editor's `XDG_RUNTIME_DIR`, or close the editor.

A program can use the socket itself instead of running `eddie` per
command. `serve` prints its `socket` path; sockets live in
`$XDG_RUNTIME_DIR/eddie`, or in `run/` beside the settings file when that
variable is unset. The editor's first line is `{"event":"hello","project",
"window","version"}`. Send one JSON object per line,
`{"id":1,"method":"run","code":"split('clip:7@50%')"}`, and read lines
until one has your `id` and `ok`; lines with `progress` or `event` come in
between. Read each answer before sending much more: a connection with 256
requests or 16 MiB waiting gets errors for the rest. The methods are the
commands, with these parameters:

- `run`: `code`, `dryRun` (`true` or `false`; any other value is an
  error, not a real run).
- `import`: `media` (absolute paths).
- `transcribe`: `assets`, `force`, `provider`, `model`, `language`,
  `vocabulary`, `apiKey` (the `eddie` command sends
  `EDDIE_TRANSCRIBE_API_KEY` here). `curate`: `assets`, `force`.
- `sheet`: `output`, `range`. `strip`: `asset`, `output`. `frames`:
  `asset`, `range`, `directory`, `every`, `width`. `export`: `output`.
  Output paths are absolute.
- `state`, `inspect`, `undo`, `redo`, `save` and `stop` take none.

## Pictures

JSON does not show what a shot looks like. Three commands write JPEGs for
you to open; each also prints the times it pictured. `sheet` and `strip`
outputs must end in `.jpg`, and cannot replace the project or its media.

- `eddie sheet <project> <out.jpg> [range]` shows the program: one cell
  per visible shot, labelled with its timeline time, clip ID and lane, and
  with titles drawn on the picture.
  - A sheet has 16 cells at most. `shots` counts the pictures in the
    range; a new caption over the same picture is not a shot. Past 16
    shots the cells are spread evenly and the JSON says `sampled: true`.
    Give a timeline range (`10..25`, `clip:7`) to see every shot of one
    part.
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

- Scripts are synchronous. One that uses `async`, `await` or a promise
  callback is refused whole.
- The 500 ms limit is checked between JavaScript steps, so it cannot stop
  one built-in call on strings of megabytes (`indexOf`). After 1 s such a
  script fails with "inside one built-in call" and applies nothing. Do
  not send it again: in a running editor the call goes on in the
  background, and once four are running every script is refused until
  one returns or the editor restarts. Keep strings small.

- `run` prints `{changed, saved, output, notes, handles,
  introducedFindings, summary}`.
  - `output` is the script's `return` value as JSON text; parse it again.
  - `notes` holds `console.log` lines and what commands report, such as
    how many captions `addCaptionsForClip` added and why it left lines
    out.
  - `handles` maps each handle to the ID of the clip it created.
- `importMedia` and `exportTo` are refused here. Use `eddie import` and
  `eddie export` instead.
- The queued edits get 250 ms to apply. "edit took too long; try a
  smaller batch" means split the script into several runs.

### Reading the project

`state()` returns `{playhead, playing, selected, duration, fps, magnet,
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
  - `silenceRanges` are stretches of 0.35 s or more below the recording's
    own quiet level.
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
  - `category` is the first of these that fits: `reaction` (a loud burst
    of at most 7 s), `talking` (heard speech), `action` (strong picture or
    subject motion), `establishing` (steady, 3 s or more, no subject in
    frame), else `broll`.
  - `quality.motion` is `1 - quality.stability`, on one scale for every
    file. `quality.audio` is relative to the file's own loudest range, so
    compare it only within one file.
  - The `music-bed` tag is the sound alone: steady, like music.
    `musicBed: true` also needs the range to have no transcribed speech.
  - Curations saved by the old model reviewer can still say `reviewed`,
    `needs-review` or `rejected`.

### Time

Every timeline time argument (`at`, `start`, `left`, `right`) takes
seconds or a spec string: `'3.2s'`, `'120f'`, `'1:05.5'`, `'50%'`,
`'end'`, `'clip:7@end'`, `'clip:7@1.5s'`, `'clip:7@50%'`. Ranges are
`'a..b'` or `'clip:7'`. `time(spec)` and `timeRange(spec)` resolve them.
Clip specs name the IDs in `state()`, not handles. Durations and source
times (`dur`, `seconds`, `sourceIn`) are plain seconds.

### Editing

Arguments out of range are errors, never moved into range: a clip ID or
lane that is not a whole number, a negative time, a length of 0 or less.

- **Select and cut:** `selectClip(id)`, `split(at)` (the selected clip),
  `splitClip(id, at)`, `deleteClip(id?)`, `clearTimeline()`. Without an
  ID, `deleteClip` deletes the selected clip and fails when there is
  none.
- **Place:** `addAsset(nameOrPath, at?)`, `addAssetRange(nameOrPath, at,
  sourceIn, dur)`. A new clip lands on the first lane that is free for
  its whole span. Read its lane as `track` in the next `state()`, or set
  it with `setClipLane`.
  - A name that two assets share is an error; pass the asset's `path`.
- **Move and trim:** `moveClip(id, start)`, `setClipLane(id, lane)`,
  `insertTime(at, seconds)`, `closeGaps()`, `trimClip(id, left, right)`.
  - `left` is the clip's new start and `right` its new end, both timeline
    times. `null` or -1 keeps that edge. An end at or before the start
    is an error.
  - A new start moves the source in-point by the same amount. A new end
    stops at the end of the source.
  - Two clips can't overlap on one lane. This is checked once, after the
    whole script: clips may cross on the way, but a script that ends with
    a new overlap is refused with an error naming both clips. Trim one,
    or move it in time or to another lane.
  - Lanes run from 0 to 64 and times to 1,000,000 seconds.
- **Picture and sound:** `setOpacity(id, 0..1)`, `setVolume(id, gain)`,
  `setFade(id, inSeconds, outSeconds)`, `setSmoothFades(id, bool)`,
  `setDuck(id, bool)`, `setCleanVoice(id, bool)`, `setEmbeddedAudio(id,
  bool)`, `detachAudio(id)`, `unlinkClip(id)`.
  - `gain` is linear, from 0 to 2: 1 leaves the clip as recorded, 0.5 is
    about 6 dB quieter, 0.25 about 12 dB.
- **Pauses:** `tightenSilence(id, threshold?, minSilence?, padding?)`
  removes each pause of `minSilence` seconds or more (0.35 by default)
  and keeps `padding` seconds (0.08) at each cut. Leave `threshold` out
  to use the recording's own quiet level.
- **Titles:** `addTitle(text, at?, dur?, style?)`, `setTitleStyle(id,
  style)`, `renameClip(id, text)` (a title's name is its text),
  `addCaptionsForClip(id, style?)`.
  - Styles: `lower-third`, `caption`, `pop-caption`, `minimal-caption`
    and `highlight-caption`, which lights each word as it is spoken and
    needs word timing.
  - `addTitle` defaults to the playhead, 4 seconds and `lower-third`;
    `addCaptionsForClip` to `caption`.
  - Title text cannot hold control characters. Line breaks are fine;
    they are drawn as spaces.
  - One title shows at a time. Where two overlap, the one added first
    shows.
  - `addCaptionsForClip` returns no handle. Its line in `notes` says how
    many captions it added; read their IDs from `state()` in the next
    run.
- **Settings:** `setMagnet(bool)`, `setMusicBedMuted(bool)`.
- **Transport and view:** `play()`, `pause()`, `toggle()`, `seek(t)`,
  `step(nFrames)`, `setZoom(pxPerSec)`. In the window these move the
  user's playhead and view, so use them to show the user something, not
  to edit. Pass times to edits instead of seeking first.

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

  A base clip covered from end to end gets the `inspect()` finding
  `hidden-clip` with "only its sound plays". Here that is the intent.
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
- Scene edges are measured, not exact. A fast move or a dissolve can put
  a cut a few frames off, so look at a scene's first and last pictures
  with `frames` before a cut that has to be clean.
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

- `musicBed: true` marks a range that probably has music baked in and no
  speech.
  - Judge those ranges on their picture, not on their sound.
  - While `musicBedMuted` is true, such video is added muted so old
    soundtracks don't stack. Audio-only tracks always stay audible.
- Music added on its own ducks under speech by default. A voiceover
  doesn't duck.
- Export normalizes to -16 LUFS, so balance clips against each other.
  - `audioReport()` gives the mix's loudness while each audible clip
    plays, as `clips [{id, integratedLufs, truePeakDbtp}]`. Its `status`
    is `measured`, `silent` when no clip is audible, or `failed`.
  - A stretch 10 LU quieter than its neighbours is buried. Raise or
    lower clips with `setVolume`.
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
