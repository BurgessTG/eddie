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
function markers() { return state().markers; }
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
    "state() clips() markers() assets() sweepCandidates() sceneClips() transcriptSegments() inspect() audioReport()",
    "utterances(asset)  // [{start,end,text}] speech that should not be cut inside",
    "snapToSpeech(asset, start, end, handle?)  // {start,end} widened out of any utterance, 0.12s handles",
    "Times: seconds, or a string such as '3.2s' '120f' '1:05.5' '50%' 'end' 'clip:7@end' 'clip:7@1.5s' 'marker:3' (request-time clips and markers); time(spec) timeRange('a..b' | 'clip:7')",
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
    "setBlend(id, mode)  // video only: how the picture mixes with the lanes under it. modes: normal darken multiply color-burn linear-burn lighten screen color-dodge add overlay soft-light hard-light difference exclusion",
    "setKeyframe(id, property, at, value)  // animate: property is x y scale rotation opacity (video) or volume (video or audio), in setTransform's, setOpacity's and setVolume's units; at is a timeline time on the clip. Values run in a straight line from key to key and hold before the first and after the last; keys move with the clip and stay on their moment through trims, splits and speed changes. Rotation keys may pass 360 to keep turning",
    "removeKeyframe(id, property, at) clearKeyframes(id, property?)  // clearing keeps the value at the clip's start; no property clears them all. setOpacity, setVolume and setTransform set one value for the whole clip and remove that property's keys",
    "setMask(id, {shape, x, y, width, height, rotation, feather, round, invert})  // video only: keeps the picture inside a shape and clears the rest, so the lanes under show (a blurred background stays). {} is a circle; changes just the fields given, null removes the mask. shape split (keeps above a line), filmstrip (a band height tall), circle, rectangle, heart or star; x, y the shape's centre from the picture's, in picture widths and heights (-1 to 1); width, height in shorter sides of the picture (0.01 to 5, 0.6); rotation clockwise degrees, with the picture; feather 0 to 1, the soft edge (1 is half the shorter side); round 0 to 1, a rectangle's corners; invert keeps the outside",
    "setChroma(id, {color, strength, softness, spill})  // video only: clears the parts of the picture near color (a green or blue screen) so the lanes under show. {} keys green (#00B140); changes just the fields given, null removes the key. color '#RRGGBB' ('#0047BB' for blue); strength 0 to 1 (0.6), how far from the colour still clears, as a share of the way to grey; softness 0 to 1 (0.3), how gradual the edge is; spill 0 to 1 (0.5), how much of the colour's cast on what stays is taken off",
    "setColor(id, {brightness, contrast, saturation, exposure, temperature, tint, filter, strength})  // video only; changes just the fields given, null resets. -1 to 1 each (0 is unchanged), exposure in stops (-2 to 2); filter: none vivid dramatic warm cool fade vintage sepia mono noir, at strength 0 to 1 (1)",
    "setFormat(name)  // the canvas shape: original landscape portrait square vertical classic cinema, or a ratio such as '9:16'",
    "setTransition(id, kind, seconds?)  // video only: how the picture passes from the clip right before it on its lane, centred on the cut. kinds: mix dip-black dip-white wipe-left wipe-right wipe-up wipe-down slide-left slide-right slide-up slide-down; seconds 0.1 to 5 (0.5); null removes it",
    "setSpeed(id, 0.25..4)  // video or audio; keeps the source range, so the clip's length becomes it at that speed (a linked partner too); sound keeps its pitch",
    "setReverse(id, bool)  // video or audio; plays the clip (and a linked partner) backwards, or forwards again; the run waits while Eddie writes the reversed file",
    "setStabilize(id, level)  // video only: steadies a shaking camera, the linked sound following; levels minimal-crop recommended most-stable (each smooths more and crops more), true for recommended, null or false plays it as shot; the run waits while Eddie writes the steadied file",
    "setFade(id, inSec, outSec) setDuck(id, bool) setReduceNoise(id, bool) setCleanVoice(id, bool) setSmoothFades(id, bool)",
    "insertTime(at, seconds)  // splits clips crossing at and moves everything after later",
    "detachAudio(id) unlinkClip(id) setEmbeddedAudio(id, bool)",
    "tightenSilence(id, threshold?, minSilence?, padding?)  // removes pauses of minSilence (0.35 s) or more, keeping padding (0.08 s); no threshold uses the recording's own quiet level",
    "addTitle(text, at?, dur?, style?)  // defaults: playhead, 4 s, 'lower-third'",
    "setTitleStyle(id, style) addCaptionsForClip(id, style?)  // styles: lower-third caption pop-caption minimal-caption highlight-caption; captions default to 'caption'",
    "setText(id, {font, bold, italic, align, size, color, boxColor, boxOpacity, outline, outlineColor, x, y})  // titles only, over the style; changes just the fields given, null resets all, a field set to null goes back to the style's. font: inter, lora, playfair-display, oswald, bebas-neue, caveat, pacifico or roboto-mono; bold and italic true or false, drawn where the font has the face; align 'left', 'center' or 'right' lines up the lines and puts the text at the left margin, the middle or the right margin; size 0.25 to 4 times the style's; colours '#RRGGBB'; boxOpacity 0 (none) to 1; outline 0 (none) to 0.2 of the text size; x, y move it from the style's place in canvas widths and heights (-1 to 1, right and down positive)",
    "addShape(shape?, at?, dur?)  // a shape drawn as a picture on the lane above the video there, a third of the canvas, for 3 s at the playhead; it takes setTransform, keys, blends, masks and transitions like a photo. shape is a kind (rectangle circle triangle star heart arrow bubble; arrow points right, bubble is a speech bubble) or {kind, fill, outline, thickness, round, ratio}: fill and outline '#RRGGBB' or null for none (white fill, no outline); thickness 0.005 to 0.25 of the shape's shorter side (0.05), inside its edge; round 0 to 1, rectangle and bubble corners; ratio width over height, 0.1 to 10 (the kind's own)",
    "setShape(id, {kind, fill, outline, thickness, round, ratio})  // changes just the fields given; a new kind takes its own ratio unless one is given; null is plain white",
    "addSticker(name, at?, dur?)  // a built-in emoji sticker, placed as addShape places a shape. stickers() lists the names, such as thumbs-up, fire, red-heart, party-popper",
    "addAsset(nameOrPath, at?) addAssetRange(nameOrPath, at, sourceIn, dur)  // lands on the first lane free for its span",
    "  // adds, splits, duplicates and freezes return a handle for the new clip (a split's is its right piece); pass it to later calls in the same run",
    "importMedia(path) exportTo(path) clearTimeline()",
    "setMagnet(bool) setMusicBedMuted(bool) setZoom(pxPerSec) closeGaps()",
    "addMarker(at?, name?, color?)  // a note pinned to a timeline time (the playhead); colors: yellow orange red pink purple blue cyan green, default the newest marker's. Its ID is in notes and in markers(); markers stay at their time when clips move",
    "setMarker(id, {time, name, color})  // changes just the fields given. removeMarker(id) clearMarkers()",
    "return reports back as `output` (JSON text); console.log lines and command reports land in `notes`. The in-app console keeps variables between lines; each eddie run starts fresh."
  ].join("\n");
}
var console = { log: (...a) => __log(a.map(String).join(" ")) };
function __resolve(spec, range) {
  const s = state();
  const clips = (s.clips || []).flatMap((c) => [c.id, c.start, c.start + c.duration]);
  const marks = (s.markers || []).flatMap((m) => [m.id, m.time]);
  return __resolve_time(String(spec), s.fps, s.duration, clips, marks, range);
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
function setKeyframe(id, property, at, value) {
  if (typeof value !== "number" || !isFinite(value)) throw new Error("setKeyframe needs a finite number for the value");
  __set_keyframe(id, String(property), __t(at), value);
}
function removeKeyframe(id, property, at) { __remove_keyframe(id, String(property), __t(at)); }
function clearKeyframes(id, property) {
  __clear_keyframes(id, property === undefined || property === null ? "" : String(property));
}
function setBackground(id, name) { __set_background(id, String(name)); }
function setBlend(id, mode) { __set_blend(id, String(mode)); }
function setStabilize(id, level) {
  if (level === undefined) throw new Error("setStabilize needs a level such as 'recommended', or null to play the clip as shot");
  __set_stabilize(id, level === null || level === false ? "" : level === true ? "recommended" : String(level));
}
function setTransition(id, kind, seconds) {
  if (kind === undefined) throw new Error("setTransition needs a kind such as 'mix', or null to remove it");
  __set_transition(id, kind === null ? "" : String(kind), seconds === undefined ? 0.5 : seconds);
}
function setColor(id, changes) {
  if (changes !== null && (typeof changes !== "object" || Array.isArray(changes))) {
    throw new Error("setColor needs an object such as {saturation: 0.2, filter: 'warm'}, or null to reset");
  }
  __set_color(id, JSON.stringify(changes));
}
function setMask(id, changes) {
  if (changes !== null && (typeof changes !== "object" || Array.isArray(changes))) {
    throw new Error("setMask needs an object such as {shape: 'heart', width: 0.5}, {} for a circle, or null to remove the mask");
  }
  for (const key in changes || {}) {
    if (typeof changes[key] === "number" && !isFinite(changes[key])) {
      throw new Error("setMask " + key + " must be a finite number");
    }
  }
  __set_mask(id, JSON.stringify(changes));
}

function __look(name, look, example) {
  if (look === null || typeof look !== "object" || Array.isArray(look)) {
    throw new Error(name + " needs an object such as " + example);
  }
  for (const key in look) {
    if (typeof look[key] === "number" && !isFinite(look[key])) {
      throw new Error(name + " " + key + " must be a finite number");
    }
  }
  return JSON.stringify(look);
}
function addShape(shape, at, dur) {
  const look = shape === undefined ? {} : typeof shape === "string" ? { kind: shape } : shape;
  return __add_shape(__look("addShape", look, "{kind: 'arrow', fill: '#FFD600'} or a shape name such as 'circle'"), at === undefined ? state().playhead : __t(at), dur === undefined ? 3 : dur);
}
function addSticker(name, at, dur) {
  return __add_sticker(String(name), at === undefined ? state().playhead : __t(at), dur === undefined ? 3 : dur);
}
function stickers() { return __sticker_names.slice(); }
function setShape(id, changes) {
  __set_shape(id, changes === null ? "null" : __look("setShape", changes, "{fill: '#FF3B30', outline: null}, or null for a plain white shape"));
}

function setChroma(id, changes) {
  if (changes !== null && (typeof changes !== "object" || Array.isArray(changes))) {
    throw new Error("setChroma needs an object such as {color: '#00B140', strength: 0.5}, {} for green, or null to remove the key");
  }
  for (const key in changes || {}) {
    if (typeof changes[key] === "number" && !isFinite(changes[key])) {
      throw new Error("setChroma " + key + " must be a finite number");
    }
  }
  __set_chroma(id, JSON.stringify(changes));
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
function addMarker(at, name, color) {
  __add_marker(at === undefined || at === null ? state().playhead : __t(at),
    name === undefined || name === null ? "" : String(name),
    color === undefined || color === null ? "" : String(color));
}
function setMarker(id, changes) {
  if (changes === null || typeof changes !== "object" || Array.isArray(changes)) {
    throw new Error("setMarker needs an object such as {name: 'B-roll here', color: 'red'}");
  }
  const c = Object.assign({}, changes);
  if (c.time !== undefined) {
    c.time = __t(c.time);
    if (typeof c.time !== "number" || !isFinite(c.time)) throw new Error("setMarker time must be a finite number or a time spec");
  }
  __set_marker(id, JSON.stringify(c));
}
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
