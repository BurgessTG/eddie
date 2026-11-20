// The JavaScript half of the API, loaded into every VM before a script runs.
function state() {
  const s = JSON.parse(__state_json);
  if ("audio" in s) {
    const audio = s.audio;
    Object.defineProperty(s, "audio", { enumerable: true, get() { __audio_read(); return audio; } });
  }
  return s;
}
function clips() { return state().clips; }
function assets() { return state().assets; }
function sweepCandidates() { return state().sweepCandidates || []; }
function sceneClips() { return state().sceneClips || []; }
function transcriptSegments() { return state().transcriptSegments || []; }
function inspect() { return state().inspection || []; }
function audioReport() { return state().audio || { status: "silent" }; }
function __asset(nameOrPath) {
  const wanted = String(nameOrPath);
  const all = assets();
  const byPath = all.find((x) => x.path === wanted);
  if (byPath) return byPath;
  const named = all.filter((x) => x.name === wanted);
  if (named.length > 1) throw new Error(named.length + " assets are named '" + wanted + "'; use the path");
  if (!named.length) throw new Error("no asset " + nameOrPath);
  return named[0];
}
function utterances(nameOrPath) { return __asset(nameOrPath).utterances || []; }
function snapToSpeech(nameOrPath, start, end, handle) {
  const a = __asset(nameOrPath);
  const u = a.utterances || [];
  const h = handle === undefined ? 0.12 : handle;
  let s = start, e = end;
  for (let i = 0; i < u.length; i++) {
    const before = i > 0 ? u[i - 1].end : 0;
    const after = i + 1 < u.length ? u[i + 1].start : a.duration;
    if (u[i].start - h < s && s < u[i].end - 0.05) s = Math.max(u[i].start - h, before, 0);
    if (u[i].start + 0.05 < e && e < u[i].end + h) e = Math.min(u[i].end + h, after, a.duration);
  }
  return { start: s, end: e };
}
function help() {
  return [
    "state() clips() assets() sweepCandidates() sceneClips() transcriptSegments() inspect() audioReport()",
    "utterances(asset)  // [{start,end,text}] speech that should not be cut inside",
    "snapToSpeech(asset, start, end, handle?)  // {start,end} widened out of any utterance, 0.12s handles",
    "Times: seconds, or a string such as '3.2s' '120f' '1:05.5' '50%' 'end' 'clip:7@end' 'clip:7@1.5s' (request-time clips); time(spec) timeRange('a..b' | 'clip:7')",
    "play() pause() toggle() seek(t) step(nFrames)",
    "selectClip(id) split(at?) splitClip(id, at) deleteClip(id?)",
    "rippleDelete(id)  // deletes the clip and its linked sound and moves later clips up on every lane that has room; a deleted picture takes the titles over it, and titles across its edges lose that part",
    "duplicateClip(id)  // a copy (and its linked sound) right after the clip; a video clip moves later clips along to make room, a title or sound takes the first lane free there",
    "freezeFrame(id, at?, seconds?)  // splits a video clip at at (the playhead), holds that picture for seconds (3), silent, and moves everything starting from at later",
    "moveClip(id, start) setClipLane(id, lane)",
    "trimClip(id, left, right)  // the clip's new start and end as timeline times; null or -1 keeps an edge",
    "renameClip(id, name) setOpacity(id, 0..1) setVolume(id, gain)  // linear gain 0..2; 0.5 is about 6 dB quieter",
    "setTransform(id, {scale, x, y, rotation, flipH, flipV, cropLeft, cropTop, cropRight, cropBottom})  // video only; changes just the fields given, null resets. scale 1 fits the canvas, 'fill' covers it; x and y move by canvas widths and heights (right and down); rotation is clockwise degrees; crops are shares of the source",
    "setBackground(id, 'black' | 'blur')  // what fills the canvas around a video picture",
    "setColor(id, {brightness, contrast, saturation, exposure, temperature, tint, filter, strength})  // video only; changes just the fields given, null resets. -1 to 1 each (0 is unchanged), exposure in stops (-2 to 2); filter: none vivid dramatic warm cool fade vintage sepia mono noir, at strength 0 to 1 (1)",
    "setFormat(name)  // the canvas shape: original landscape portrait square vertical classic cinema, or a ratio such as '9:16'",
    "setSpeed(id, 0.25..4)  // video or audio; keeps the source range, so the clip's length becomes it at that speed (a linked partner too); sound keeps its pitch",
    "setFade(id, inSec, outSec) setDuck(id, bool) setCleanVoice(id, bool) setSmoothFades(id, bool)",
    "insertTime(at, seconds)  // splits clips crossing at and moves everything after later",
    "detachAudio(id) unlinkClip(id) setEmbeddedAudio(id, bool)",
    "tightenSilence(id, threshold?, minSilence?, padding?)  // removes pauses of minSilence (0.35 s) or more, keeping padding (0.08 s); no threshold uses the recording's own quiet level",
    "addTitle(text, at?, dur?, style?)  // defaults: playhead, 4 s, 'lower-third'",
    "setTitleStyle(id, style) addCaptionsForClip(id, style?)  // styles: lower-third caption pop-caption minimal-caption highlight-caption; captions default to 'caption'",
    "setText(id, {size, color, boxColor, boxOpacity, outline, outlineColor, x, y})  // titles only, over the style; changes just the fields given, null resets all, a field set to null goes back to the style's. size 0.25 to 4 times the style's; colours '#RRGGBB'; boxOpacity 0 (none) to 1; outline 0 (none) to 0.2 of the text size; x, y move it from the style's place in canvas widths and heights (-1 to 1, right and down positive)",
    "addAsset(nameOrPath, at?) addAssetRange(nameOrPath, at, sourceIn, dur)  // lands on the first lane free for its span",
    "  // adds, splits, duplicates and freezes return a handle for the new clip (a split's is its right piece); pass it to later calls in the same run",
    "importMedia(path) exportTo(path) clearTimeline()",
    "setMagnet(bool) setMusicBedMuted(bool) setZoom(pxPerSec) closeGaps()",
    "return reports back as `output` (JSON text); console.log lines and command reports land in `notes`. The in-app console keeps variables between lines; each eddie run starts fresh."
  ].join("\n");
}
var console = { log: (...a) => __log(a.map(String).join(" ")) };
function __resolve(spec, range) {
  const s = state();
  const clips = (s.clips || []).flatMap((c) => [c.id, c.start, c.start + c.duration]);
  return __resolve_time(String(spec), s.fps, s.duration, clips, range);
}
function time(spec) { return typeof spec === "number" ? spec : __resolve(spec, false)[0]; }
function timeRange(spec) { const r = __resolve(spec, true); return { start: r[0], end: r[1] }; }
function __t(v) { return typeof v === "string" ? time(v) : v; }
function seek(t) { __seek(__t(t)); }
function moveClip(id, start) { __move_clip(id, __t(start)); }
function insertTime(at, seconds) { __insert_time(__t(at), seconds); }
function __edge(v) { return v === undefined || v === null ? -1 : __t(v); }
function trimClip(id, left, right) { __trim_clip(id, __edge(left), __edge(right)); }
function split(at) {
  const s = state();
  if (s.selected == null) throw new Error("no clip selected");
  return __split_clip(s.selected, at === undefined ? s.playhead : __t(at));
}
function splitClip(id, at) { return __split_clip(id, __t(at)); }
function freezeFrame(id, at, seconds) {
  return __freeze_frame(id, at === undefined ? state().playhead : __t(at), seconds === undefined ? 3 : seconds);
}
function deleteClip(id) {
  if (id === undefined) {
    id = state().selected;
    if (id == null) throw new Error("no clip selected");
  }
  __delete_clip(id);
}
function setTransform(id, changes) {
  if (changes !== null && (typeof changes !== "object" || Array.isArray(changes))) {
    throw new Error("setTransform needs an object such as {scale: 1.5, x: 0.25}, or null to reset");
  }
  __set_transform(id, JSON.stringify(changes));
}
function setBackground(id, name) { __set_background(id, String(name)); }
function setColor(id, changes) {
  if (changes !== null && (typeof changes !== "object" || Array.isArray(changes))) {
    throw new Error("setColor needs an object such as {saturation: 0.2, filter: 'warm'}, or null to reset");
  }
  __set_color(id, JSON.stringify(changes));
}
function setText(id, changes) {
  if (changes !== null && (typeof changes !== "object" || Array.isArray(changes))) {
    throw new Error("setText needs an object such as {size: 1.5, color: '#FFD600'}, or null to reset");
  }
  for (const key in changes || {}) {
    // JSON writes NaN as null, which would quietly mean "the style's".
    if (typeof changes[key] === "number" && !isFinite(changes[key])) {
      throw new Error("setText " + key + " must be a finite number");
    }
  }
  __set_text(id, JSON.stringify(changes));
}
function setFormat(name) { __set_format(String(name)); }
function addTitle(text, at, dur, style) {
  return __add_title(String(text), at === undefined ? state().playhead : __t(at), dur === undefined ? 4 : dur, style === undefined ? "lower-third" : String(style));
}
function addCaptionsForClip(id, style) {
  __add_captions_for_clip(id, style === undefined ? "caption" : String(style));
}
function addAsset(nameOrPath, at) {
  return __add_asset(String(nameOrPath), at === undefined ? state().playhead : __t(at));
}
function addAssetRange(nameOrPath, at, sourceIn, dur) {
  return __add_asset_range(String(nameOrPath), __t(at), sourceIn, dur);
}
function tightenSilence(id, threshold, minSilence, padding) {
  const rest = [
    id,
    minSilence === undefined ? 0.35 : minSilence,
    padding === undefined ? 0.08 : padding,
  ];
  if (threshold === undefined || threshold === null) __tighten_silence(...rest);
  else __tighten_silence(...rest, threshold);
}
