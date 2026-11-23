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
- `export` writes what the output's extension names: `.mp4` (or `.mov`,
  `.m4v`, `.mkv`) a video; `.mp3`, `.wav`, `.m4a` (AAC) or `.flac` the
  mix alone; `.gif` a silent looping GIF at 15 fps, or the project's rate
  when that is lower. A name without one of these gets the extension of
  what the window's Export dialog last chose (`cut` becomes `cut.mp4` at
  first).
  - A video takes `--resolution auto|480p|720p|1080p|2k|4k`, `--fps
    24|25|30|50|60` and `--quality lower|recommended|higher`; a GIF takes
    `--resolution 240p|320p|480p|640p`. A flag that does not apply is an
    error. Without them it uses what the Export dialog last chose: at
    first `auto` (the largest video's size, at most 1080p), the project's
    frame rate and `recommended`, and 480p for a GIF.
  - Its reply gives the `output` it wrote and the `kind` (`video`,
    `audio` or `gif`), with the `size`, `fps`, `quality` and `encoder`
    (`x264`, or `nvenc` when the user turned on Settings → Hardware
    encode) of a video, the `format` of a sound file, or the `size` and
    `fps` of a GIF.

After `run`, read its `introducedFindings` (or run `inspect`), look at a
`sheet`, and fix what the edit broke. `run --dry-run` keeps nothing: its
reply adds `clips`, the timeline the edit would leave.

## A running editor

While a project is open in the Eddie window or in `eddie serve`, every
command goes to that editor instead of the project file. You call it the
same way; replies add `"live":true`.

- `eddie editors` lists every running editor: `{project, window,
  untitled, pid, version, socket}`. Start there when the user says Eddie
  is open: use its `project` as the project argument of every command.
- A window whose project was never saved works on an untitled project in
  Eddie's settings folder (`"untitled": true`). It takes every command
  like any other. Only the user picks where it is saved: their Save asks
  for a name. `eddie save` writes the untitled file, and a file with clips
  is kept there after the window closes.
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
  `asset`, `range`, `directory`, `every`, `width`. `export`: `output`,
  `resolution`, `fps`, `quality`. Output paths are absolute.
- `state`, `inspect`, `undo`, `redo`, `save` and `stop` take none.

## Pictures

JSON does not show what a shot looks like. Three commands write JPEGs for
you to open; each also prints the times it pictured. `sheet` and `strip`
outputs must end in `.jpg`, and cannot replace the project or its media.

- `eddie sheet <project> <out.jpg> [range]` shows the program: one cell
  per visible shot, labelled with its timeline time, clip ID and lane, and
  with titles drawn on the picture. Each picture is placed, scaled and
  turned as export draws it, over the lanes it does not hide; the label
  names those too ("clip 7 L0 over 3 L1").
  - A sheet has 16 cells at most. `shots` counts the pictures in the
    range; a new caption over the same picture is not a shot. Past 16
    shots the cells are spread evenly and the JSON says `sampled: true`.
    Give a timeline range (`10..25`, `clip:7`) to see every shot of one
    part.
  - `cells` lists `time, clip, lane, sourceTime, title` for each cell's
    top picture, and `under` the pictures beneath it as `[{clip, lane,
    sourceTime}]`, the nearest first.
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
musicBedMuted, format, formats, canvas, clips, markers, assets,
sweepCandidates, sceneClips, transcriptSegments, inspection, audio}`.
`canvas` is the picture size export renders, `[width, height]`. The
shortcuts `clips()`, `markers()`, `assets()`, `sweepCandidates()`,
`sceneClips()`, `transcriptSegments()`, `inspect()` and `audioReport()`
return its parts.

- **Clips:** `id, name, kind ('video'|'audio'|'title'), track, start,
  duration, sourceIn, source, opacity, volume, fadeIn, fadeOut,
  smoothFades, duck, reduceNoise, cleanVoice, embeddedAudio, linkedClip, titleStyle,
  transform, background, color, blend, chroma, mask, graphic, speed,
  remade, photo, freeze, text, transition, keyframes`. `chroma` is the chroma key
  `{color, strength, softness, spill}` or `null`; `mask` is `{shape, x,
  y, width, height, rotation, feather, round, invert}` or `null`;
  `graphic` is `{type: 'shape', kind, fill, outline, thickness, round,
  ratio}`, `{type: 'sticker', name}` or `null`. `remade` is `{original,
  start, end, reverse, stabilize}` or `null` (see Reverse and Stabilize
  below). A clip
  shows
  source `sourceIn` to `sourceIn + duration * speed`; a photo clip
  (`photo`) shows its one picture for its whole length, and a freeze
  frame (`freeze`) the picture at `sourceIn`. A title's `text` holds what
  `setText` changed; a `null` field follows the style. `transition` is
  `{kind, duration}` or `null`. `keyframes` holds `x`, `y`, `scale`,
  `rotation`, `opacity` and `volume` keys as `[{at, value}]`, `at` in
  seconds from the clip's start; a property without keys is left out, and
  a keyed property's plain value (`opacity`, `transform.scale`, …) is not
  what plays.
- **Markers:** `id, time, name, color`, in time order. The user drops
  them while watching, often with a note such as "B-roll here"; read them
  as their requests.
- **Assets:** `name, path, duration, width, height, hasVideo, hasAudio,
  photo, orientation, ready, silenceRanges [{start,end}], utterances
  [{start,end,text}], tempo, beats`.
  - `photo: true` marks a photo (PNG, JPEG, WebP or HEIC): one picture, with
    `duration` 0. `curate`, `strip` and `frames` refuse it; open the file
    itself to see it.
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
`'end'`, `'clip:7@end'`, `'clip:7@1.5s'`, `'clip:7@50%'`, `'marker:3'`.
Ranges are `'a..b'` (such as `'marker:1..marker:2'`) or `'clip:7'`.
`time(spec)` and `timeRange(spec)` resolve them. Clip and marker specs
name the IDs in `state()`, not handles. Durations and source
times (`dur`, `seconds`, `sourceIn`) are plain seconds.

### Editing

Arguments out of range are errors, never moved into range: a clip ID or
lane that is not a whole number, a negative time, a length of 0 or less.

- **Select and cut:** `selectClip(id)`, `split(at)` (the selected clip),
  `splitClip(id, at)`, `deleteClip(id?)`, `rippleDelete(id)`,
  `clearTimeline()`. Without an ID, `deleteClip` deletes the selected
  clip and fails when there is none. `deleteClip` leaves a gap.
  - `rippleDelete(id)` deletes the clip and its linked sound and moves
    every clip after it earlier by its length, on every lane. Deleting a
    picture also deletes the titles over it, and cuts that time out of
    titles across its edges, unless other video shows there. A lane
    where a remaining clip runs into the deleted time, such as a music
    bed, does not move; `notes` says how many lanes stayed.
- **Copy:** `duplicateClip(id)` puts a copy, with its linked sound, right
  after the clip. When that time is taken on a video clip's lane, every
  clip from there on, on every lane, moves later to make room; a title or
  sound copy goes to the first free lane of its kind instead.
- **Freeze frame:** `freezeFrame(id, at?, seconds?)` splits a video clip
  at `at` (default: the playhead) and holds the picture shown there for
  `seconds` (3 by default), silent. Every clip that starts at `at` or
  later, on every lane, moves later by that length; clips running across
  `at` on other lanes, such as music, stay whole. A freeze frame has no
  speed or source range, like a photo.
- **Place:** `addAsset(nameOrPath, at?)`, `addAssetRange(nameOrPath, at,
  sourceIn, dur)`. A new clip lands on the first lane of its kind that is
  free for its whole span. Read its lane as `track` in the next `state()`,
  or set it with `setClipLane`.
  - A photo added with `addAsset` lasts 3 seconds. `addAssetRange` places
    it for `dur` and ignores `sourceIn`.
  - A name that two assets share is an error; pass the asset's `path`.
- **Move and trim:** `moveClip(id, start)`, `setClipLane(id, lane)`,
  `insertTime(at, seconds)`, `closeGaps()`, `trimClip(id, left, right)`.
  - `left` is the clip's new start and `right` its new end, both timeline
    times. `null` or -1 keeps that edge. An end at or before the start
    is an error.
  - A new start moves the source in-point by the same amount, times the
    clip's speed. A new end stops at the end of the source. A photo has
    no source range, so either edge of it goes anywhere.
  - Two clips can't overlap on one lane. This is checked once, after the
    whole script: clips may cross on the way, but a script that ends with
    a new overlap is refused with an error naming both clips. Trim one,
    or move it in time or to another lane.
  - Lanes run from 0 to 64 and times to 1,000,000 seconds.
- **Picture and sound:** `setOpacity(id, 0..1)`, `setVolume(id, gain)`,
  `setFade(id, inSeconds, outSeconds)`, `setSmoothFades(id, bool)`,
  `setDuck(id, bool)`, `setReduceNoise(id, bool)`, `setCleanVoice(id,
  bool)`, `setEmbeddedAudio(id, bool)`, `detachAudio(id)`,
  `unlinkClip(id)`.
  - `gain` is linear, from 0 to 2: 1 leaves the clip as recorded, 0.5 is
    about 6 dB quieter, 0.25 about 12 dB.
  - `setReduceNoise(id, true)` brings down steady noise (hiss, hum, fans)
    by up to 18 dB, from a measurement of the file's quietest parts, taken
    once per file. It hardly touches noise that comes and goes, such as
    traffic or a crowd, and can thin out quiet music, so use it on speech.
    `setCleanVoice` then evens out the voice.
- **Speed:** `setSpeed(id, speed)` plays a video or audio clip 0.25 to 4
  times as fast; its sound keeps its pitch, and a linked partner changes
  with it. The clip keeps its source range, so its length becomes that
  range at the new speed: `setSpeed(id, 0.5)` doubles it. A slowed clip
  that would run into the next one on its lane is refused like any
  overlap, so make room in the same script first (`insertTime` or
  `moveClip`). `tightenSilence` refuses a clip that is not at speed 1:
  remove pauses first, then change the speed. A photo has no speed; trim
  it to change how long it shows.
- **Reverse:** `setReverse(id, true)` plays a video or audio clip
  backwards, and its linked sound with it; `false` plays it forwards
  again. Photos, freeze frames, shapes and stickers are refused.
  - Eddie writes the stretch the clip shows, and 1 s more at each end,
    backwards into a file of its own. The first time, `run` waits for that
    file, printing `reversing: N%` lines on stderr, then applies the
    script: about 3 s for 7 s of 1080p and 25 s for 7 s of 4K. Reversing
    the same stretch again is immediate.
  - A reversed clip's `source` is that file, and `sourceIn` counts from
    its start. `remade.original` is the file shot, and `start` and `end`
    the part of it the file holds. `frames clip:7`, `strip` and a sheet's
    `sourceTime` give times in the original.
  - Trim a reversed clip at most 1 s past its ends. For more, turn it
    forwards, trim it and reverse it again.
- **Stabilize:** `setStabilize(id, level)` steadies a shaking camera in a
  video clip; its linked sound plays the same file. Levels are
  `minimal-crop`, `recommended` (also `true`) and `most-stable`: each
  smooths the camera's path more and enlarges the picture more to hide
  the edges that moving it uncovers. `null` or `false` plays it as shot.
  - It writes a file the way Reverse does, and waits for it the same way,
    printing `stabilizing: N%`: about 11 s for 7 s of 1080p and 35 s for
    4 s of 4K at 60 fps. A clip can be stabilized and reversed at once.
  - It steadies the whole picture. A scene with little detail, or one
    where most of the picture moves (a crowd, water), can steady badly;
    look at the clip with `frames` before you keep it.
- **Transitions:** `setTransition(id, kind, seconds?)` sets how a video
  clip's picture comes in from the clip that ends where it starts, on
  its lane; `null` removes it. A mix or dip blends each side together
  with the lanes under it; a wipe or slide moves only the two pictures,
  over those lanes. Kinds: `mix`, `dip-black`, `dip-white`,
  `wipe-left`, `wipe-right`, `wipe-up`, `wipe-down`, `slide-left`,
  `slide-right`, `slide-up`, `slide-down`. `seconds` runs from 0.1 to 5
  (0.5 by default).
  - A transition is centred on the cut and moves no clip: the clip
    before plays on past its out point and the clip after starts before
    its in point. Each side uses at most half of its clip, so a short
    clip shortens it.
  - Where a file has no picture past the clip's edge, its last (or
    first) picture holds; `inspect()` says so. Leave the sources some
    room on both sides of a cut you transition.
  - The sound crossfades under it at equal power: the clip before plays
    on to the transition's end, fading out, and the clip after starts at
    the transition's start (or at its file's start, if that is later),
    fading in. The sound that crosses is the picture's own or its linked
    sound. Two pieces of one take split apart play straight on instead.
  - A clip with no clip right before it on its lane cannot take one.
    Moving clips apart keeps the setting but it does not play.
- **Canvas and framing:** video of any shape goes on any canvas. Each
  picture fits inside the canvas, centred, until you move it.
  - `setFormat(name)` sets the canvas shape: `original` (the shape of the
    picture that starts first; the default), `landscape` (16:9),
    `portrait` (9:16), `square`, `vertical` (4:5), `classic` (4:3),
    `cinema` (21:9), or a ratio such as `'9:16'`.
  - `setTransform(id, {...})` places a video clip's picture. It changes
    only the fields you give; `null` resets them all. `scale`: 1 fits,
    `'fill'` covers the canvas and cuts off the overflow. `x`, `y`: the
    centre's offset in canvas widths and heights, right and down
    positive. `rotation`: clockwise degrees. `flipH`, `flipV`. `cropLeft`,
    `cropTop`, `cropRight`, `cropBottom`: the share of the source cut
    from each edge, before any flip.
  - `setBackground(id, 'blur')` fills the canvas around the picture with
    a blurred, enlarged copy of it; `'black'` is the default.
- **Blend modes:** `setBlend(id, mode)` mixes a video clip's picture
  with what the lanes under it show, as photo editors do: `normal` (the
  default), `darken`, `multiply`, `color-burn`, `linear-burn`, `lighten`,
  `screen`, `color-dodge`, `add`, `overlay`, `soft-light`, `hard-light`,
  `difference` or `exclusion`. Opacity then mixes the result with the
  lanes under it.
  - A blended picture never hides the lanes under it. Over nothing it
    blends with black, which `inspect()` reports.
  - Inside a transition both sides draw without their blend modes.
- **Chroma key:** `setChroma(id, {...})` clears the parts of a video
  clip's picture near one colour, such as a green screen, so the lanes
  under it show through. `setChroma(id, {})` keys green (`'#00B140'`);
  `{color: '#0047BB'}` keys blue. It changes only the fields you give;
  `null` removes the key. `strength` (0 to 1, 0.6 by default) is how far
  from the colour still clears, as a share of the way to grey, so 1
  clears greys too; `softness` (0.3) how gradual the edge is, 0 cutting
  hard; `spill` (0.5) how much of the colour's cast on what
  stays, such as green on hair and skin, is taken off.
  - Only colour counts, not brightness, so the screen in shadow clears
    with the lit part. Raise `strength` while screen remains; lower it
    when hair or clothes of a similar colour clear too.
  - Put the background on a lane under the keyed clip. Over nothing the
    screen shows black, which `inspect()` reports.
  - A sheet draws the key; strips and frames show the file as shot.
- **Masks:** `setMask(id, {...})` keeps the part of a video clip's
  picture inside a shape and clears the rest, so the lanes under it show
  through. `setMask(id, {})` keeps a circle in the middle; it changes
  only the fields you give, and `null` removes the mask.
  - `shape` is `split` (keeps the picture above a line through it),
    `filmstrip` (a band across it, `height` tall), `circle`, `rectangle`,
    `heart` or `star`.
  - The shape sits on the picture as it shows, after its crop and flip,
    and moves, scales and turns with it. `x` and `y` move the shape's
    centre from the picture's, in picture widths and heights (-1 to 1,
    right and down positive). `width` and `height` are in the picture's
    shorter side (0.01 to 5, 0.6 by default), so equal sizes make a
    circle on any picture. `rotation` turns the shape clockwise in
    degrees; turn `split` to angle its line.
  - `feather` (0 to 1) softens the edge, 1 fading over half the
    picture's shorter side; `round` (0 to 1) rounds a rectangle's
    corners; `invert: true` keeps the outside instead.
  - A `'blur'` background stays whole: the mask clears only the picture
    over it. A sheet draws the mask.
- **Shapes and stickers:** `addShape(shape?, at?, dur?)` and
  `addSticker(name, at?, dur?)` draw a picture and place it on the lane
  right above the video at `at` (the playhead) for `dur` (3 s), a third of
  the canvas in size, and return a handle. The clip is a video clip
  holding one picture, like a photo: `setTransform`, keys, `setBlend`,
  `setMask` and `setTransition` work on it, and it never sets the
  canvas's shape.
  - `shape` is a kind, `rectangle`, `circle`, `triangle`, `star`,
    `heart`, `arrow` (pointing right) or `bubble` (a speech bubble), or
    `{kind, fill, outline, thickness, round, ratio}`. `fill` and
    `outline` are `'#RRGGBB'` or `null` for none (a white fill and no
    outline by default). `thickness` is the outline's, 0.005 to 0.25 of
    the shape's shorter side, drawn inside its edge; `round` (0 to 1)
    rounds a rectangle's or bubble's corners; `ratio` is width over
    height (0.1 to 10, the kind's own by default). Turn an arrow with
    `setTransform(id, {rotation: 90})`.
  - `setShape(id, {...})` changes only the fields you give; a new `kind`
    takes its own ratio unless you give one, and `null` makes the shape
    plain white.
  - `stickers()` lists the built-in emoji stickers, such as `thumbs-up`,
    `fire`, `red-heart` and `party-popper`.
  - Text on a shape is a title (`addTitle`) placed over it with `setText`;
    titles draw over every lane.
  - One edit draws at most 16 new shapes or stickers; add more in another
    run. Drawings are kept outside the project, and a project opened on
    another computer draws them again.
- **Keyframes:** `setKeyframe(id, property, at, value)` animates a clip.
  `property` is `x`, `y`, `scale`, `rotation` or `opacity` on video, or
  `volume` on video or audio, in the units of `setTransform`,
  `setOpacity` and `setVolume`. `at` is a timeline time on the clip.
  - The value runs in a straight line from each key to the next, and
    holds before the first key and after the last. A key at the time of
    another replaces it. Rotation keys may pass 360 to keep turning.
  - Keys move with the clip and stay on the same moment of its footage
    through trims, splits and speed changes. A freeze frame holds the
    look at its point, without keys.
  - `removeKeyframe(id, property, at)` takes one key away.
    `clearKeyframes(id, property?)` takes away a property's keys, or
    every property's, keeping the value at the clip's start.
  - `setOpacity`, `setVolume` and `setTransform` set one value for the
    whole clip and remove the keys of each property they set.
  - A picture that moves or fades by key never hides the lanes under it.
    A sheet shows each picture where its keys put it at the cell's time.
- **Colour:** `setColor(id, {...})` grades a video clip. It changes only
  the fields you give; `null` resets them all. `brightness`, `contrast`,
  `saturation`, `temperature` (warm is positive) and `tint` (magenta is
  positive) run from -1 to 1, 0 leaving the picture as it is;
  `saturation: -1` is black and white. `exposure` is in stops, -2 to 2.
  `filter` is `none`, `vivid`, `dramatic`, `warm`, `cool`, `fade`,
  `vintage`, `sepia`, `mono` or `noir`, at `strength` 0 to 1 (1). The
  filter applies first, then the adjustments. Contact sheets, strips and
  frames show the colours as shot, so check a grade by exporting.
- **Pauses:** `tightenSilence(id, threshold?, minSilence?, padding?)`
  removes each pause of `minSilence` seconds or more (0.35 by default)
  and keeps `padding` seconds (0.08) at each cut. Leave `threshold` out
  to use the recording's own quiet level.
- **Titles:** `addTitle(text, at?, dur?, style?)`, `setTitleStyle(id,
  style)`, `setText(id, {...})`, `renameClip(id, text)` (a title's name
  is its text), `addCaptionsForClip(id, style?)`.
  - Styles: `lower-third`, `caption`, `pop-caption`, `minimal-caption`
    and `highlight-caption`, which lights each word as it is spoken and
    needs word timing.
  - `setText(id, {...})` changes a title's look over its style, and only
    the fields you give. `size` is 0.25 to 4 times the style's size; the
    text grows upward. `color`, `boxColor` and `outlineColor` are
    `'#RRGGBB'`. `boxOpacity` runs from 0 (no box) to 1 (solid);
    `outline` is the outline's thickness, from 0 (none) to 0.2 of the
    text size. `x` and `y` move the text from where its style puts it, in
    canvas widths and heights from -1 to 1, right and down positive.
    - A field set to `null` goes back to the style's; `setText(id, null)`
      resets them all. A box or outline colour on a style without one
      turns it on (box at 0.6, outline at 0.06).
    - A style change keeps these. Captions are separate titles, so style
      each one: loop over the IDs `state()` gives.
  - `addTitle` defaults to the playhead, 4 seconds and `lower-third`;
    `addCaptionsForClip` to `caption`.
  - Title text cannot hold control characters. Line breaks are fine;
    they are drawn as spaces.
  - One title shows at a time. Where two overlap, the one added first
    shows.
  - `addCaptionsForClip` returns no handle. Its line in `notes` says how
    many captions it added; read their IDs from `state()` in the next
    run.
- **Markers:** `addMarker(at?, name?, color?)` pins a note to a
  timeline time (the playhead by default), on its frame. Colours are
  `yellow`, `orange`, `red`, `pink`, `purple`, `blue`, `cyan` and
  `green`; without one it takes the newest marker's. It returns no
  handle: `notes` names the new marker's ID. `setMarker(id, {time, name,
  color})` changes only the fields you give. `removeMarker(id)`,
  `clearMarkers()`.
  - A marker stays at its time when clips move, ripple or are cleared:
    `clearTimeline()` keeps markers. Move them yourself after
    `insertTime` or `rippleDelete` if they should follow the footage.
- **Settings:** `setMagnet(bool)`, `setMusicBedMuted(bool)`.
- **Transport and view:** `play()`, `pause()`, `toggle()`, `seek(t)`,
  `step(nFrames)`, `setZoom(pxPerSec)`. In the window these move the
  user's playhead and view, so use them to show the user something, not
  to edit. Pass times to edits instead of seeking first.

## Rules that prevent the usual mistakes

**Clip IDs**

- Never compute or guess a clip ID. `addAsset`, `addAssetRange`,
  `addTitle`, `split`, `splitClip`, `duplicateClip` and `freezeFrame`
  return a handle for the clip they create (for a split, the right-hand
  piece). Pass that handle to later calls in the same script.
- Handles from an earlier run are invalid. Read the new IDs from `run`'s
  `handles`.

**Lanes and cutaways**

- Titles, video and sound each have their own lanes. A clip's `track` and
  `setClipLane` count only the lanes of its kind, from 0, so a title never
  shares a lane with a picture. A lane number past the last one adds lanes
  of that kind.
- Video lanes stack: lane 0 draws on top, and each lane draws over the
  lanes with higher numbers. A picture hides everything under it only
  when it is fully opaque, blends `normal`, has no chroma key and no
  transparency of its own (a PNG with an alpha channel shows through),
  and covers the canvas without a mask or has a `'blur'` background. Anything else shows the lanes under it: a clip below full
  opacity shows them through it, and around a picture made smaller with
  `setTransform` they show where its black background would be.
- For picture-in-picture, put the inset on a lane above the base and
  shrink it: `setClipLane(inset, 0)` after `setClipLane(base, 1)`, then
  `setTransform(inset, {scale: 0.35, x: 0.3, y: -0.3})`. A logo or
  sticker PNG with transparency goes on a lane above at full size.
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
  - missing media;
  - pictures moved entirely off the canvas, and titles moved off it;
  - source ranges outside their source;
  - fades longer than the clip;
  - placed on-screen text;
  - black gaps without a title;
  - flash frames (visible for under two frames);
  - near-match cuts that skip or repeat 1–3 frames of the same shot;
  - clips that the lanes above always cover;
  - transitions that play shorter than set, have no clip before them, or
    run past a file's pictures.
  - blended or keyed pictures with no picture under them;
  - masks that keep none of their picture.
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
