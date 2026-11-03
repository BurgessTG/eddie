//! Bounded subject observations for the editorial review, not an editorial judge.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use openreel_media::{EvidenceSampler, MediaInfo, MotionSample};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    api_error_for_status, apply_sam_evidence, apply_sam_face_evidence, base64_encode,
    parse_sam_boxes, sam_output_text, sam_request_body_with_prompt, sam_responses_url,
    ChatSettings, SamBox, SweepCandidate, SAM_MODEL,
};

pub(super) const MAX_CHECKS: usize = 2;
const SAMPLE_RATE: u32 = 8;
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Automatic,
    #[cfg(test)]
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Anchor {
    pub time: f64,
    pub bounds: [f32; 4],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Check {
    pub concept: String,
    pub reason: String,
    pub start: f64,
    pub end: f64,
    pub anchor: Option<Anchor>,
}

impl Check {
    pub fn parse(value: &Value, start: f64, end: f64) -> Result<Self, String> {
        let concept = value["concept"].as_str().unwrap_or("").trim();
        let reason = value["reason"].as_str().unwrap_or("").trim();
        let from = value["start"]
            .as_f64()
            .ok_or("Missing subject check start")?;
        let to = value["end"].as_f64().ok_or("Missing subject check end")?;
        if concept.is_empty()
            || concept.len() > 80
            || concept.split_whitespace().count() > 8
            || concept
                .chars()
                .any(|c| c.is_control() || matches!(c, '<' | '>' | ',' | ';'))
            || reason.is_empty()
            || reason.len() > 1024
            || !from.is_finite()
            || !to.is_finite()
            || from < start - 1e-6
            || to > end + 1e-6
            || to <= from
        {
            return Err("Invalid subject check".to_owned());
        }
        Ok(Self {
            concept: concept.to_owned(),
            reason: reason.to_owned(),
            start: from.max(start),
            end: to.min(end),
            anchor: None,
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Observation {
    frame: usize,
    time: f64,
    bounds: SamBox,
}

#[derive(Clone, Serialize, Deserialize)]
struct Detail {
    time: f64,
    object: String,
    sharpness: f32,
    luminance: f32,
    clipped: f32,
}

#[derive(Clone, Serialize, Deserialize)]
pub(in crate::app) struct Evidence {
    check: Check,
    times: Vec<f64>,
    observations: Vec<Observation>,
    details: Vec<Detail>,
    error: Option<String>,
}

impl Evidence {
    pub(super) fn failed(check: Check, error: String) -> Arc<Self> {
        Arc::new(Self {
            check,
            times: Vec::new(),
            observations: Vec::new(),
            details: Vec::new(),
            error: Some(error),
        })
    }

    pub fn is_ready(&self) -> bool {
        self.error.is_none()
    }

    pub fn json(&self, start: f64, end: f64) -> Value {
        let samples = self
            .times
            .iter()
            .filter(|time| **time >= start && **time < end)
            .count();
        let observations: Vec<_> = self
            .observations
            .iter()
            .filter(|o| o.time >= start && o.time < end)
            .collect();
        let frames: BTreeSet<_> = observations.iter().map(|o| o.frame).collect();
        let mut tracks: BTreeMap<&str, Vec<&Observation>> = BTreeMap::new();
        for observation in &observations {
            tracks
                .entry(&observation.bounds.object_id)
                .or_default()
                .push(observation);
        }
        let tracks: Vec<_> = tracks
            .into_iter()
            .map(|(id, observations)| {
                let min_area = observations
                    .iter()
                    .map(|o| o.bounds.coverage())
                    .fold(1.0_f32, f32::min);
                let max_area = observations
                    .iter()
                    .map(|o| o.bounds.coverage())
                    .fold(0.0_f32, f32::max);
                let confidence = observations
                    .iter()
                    .filter_map(|o| o.bounds.confidence)
                    .reduce(f32::min);
                let mut travel = 0.0_f32;
                let mut gaps = 0;
                for pair in observations.windows(2) {
                    if pair[1].frame != pair[0].frame + 1 {
                        gaps += 1;
                        continue;
                    }
                    travel += (pair[1].bounds.center_x() - pair[0].bounds.center_x())
                        .hypot(pair[1].bounds.center_y() - pair[0].bounds.center_y());
                }
                json!({
                    "requestLocalId":id, "observedSamples":observations.len(),
                    "firstObserved":observations.first().map(|o| o.time),
                    "lastObserved":observations.last().map(|o| o.time),
                    "boxAreaMin":min_area, "boxAreaMax":max_area,
                    "minReportedConfidence":confidence,
                    "imagePlaneTravel":travel, "observationGaps":gaps,
                })
            })
            .collect();
        let details: Vec<_> = self
            .details
            .iter()
            .filter(|d| d.time >= start && d.time < end)
            .map(|d| {
                json!({"sourceTime":d.time, "requestLocalId":d.object,
                "regionSharpness":d.sharpness, "regionLuminance":d.luminance,
                "regionClippedFraction":d.clipped})
            })
            .collect();
        json!({
            "concept":self.check.concept, "question":self.check.reason,
            "promptKind":if self.check.anchor.is_some() { "object-anchor" } else { "concept" },
            "status":if self.is_ready() { "ready" } else { "error" },
            "error":self.error,
            "checkedStart":self.check.start, "checkedEnd":self.check.end,
            "sampledFrames":samples, "observedFrames":frames.len(),
            "observedFraction":if samples > 0 { Some(frames.len() as f64 / samples as f64) } else { None },
            "firstObserved":observations.first().map(|o| o.time),
            "lastObserved":observations.last().map(|o| o.time),
            "maxSampleSpacing":self.times.windows(2).map(|p| p[1]-p[0]).fold(0.0_f64, f64::max),
            "tracks":tracks, "details":details,
            "measurement":"Sampled boxes and rectangular-region proxies, not masks, emotion, action completion or active-speaker proof. Missing detections can be tracking failures. Track IDs are request-local.",
        })
    }

    pub fn apply(&self, candidate: &mut SweepCandidate) {
        if candidate.end() <= self.check.start || candidate.source_in >= self.check.end {
            return;
        }
        if let Some(error) = &self.error {
            if let Some(visual) = candidate.visual.as_mut() {
                visual.sam_status = openreel_media::SubjectSegmentationStatus::Error;
                visual.sam_error = Some(error.clone());
            }
            return;
        }
        let middle = candidate.source_in + candidate.duration / 2.0;
        let selected = self
            .times
            .iter()
            .enumerate()
            .filter(|(_, time)| **time >= candidate.source_in && **time < candidate.end())
            .min_by(|(_, a), (_, b)| (*a - middle).abs().total_cmp(&(*b - middle).abs()));
        let Some((frame, time)) = selected else {
            return;
        };
        let boxes: Vec<_> = self
            .observations
            .iter()
            .filter(|o| o.frame == frame)
            .map(|o| o.bounds.clone())
            .collect();
        let previous_error = candidate.visual.as_ref().and_then(|visual| {
            (visual.sam_status == openreel_media::SubjectSegmentationStatus::Error)
                .then(|| visual.sam_error.clone())
                .flatten()
        });
        if self.check.anchor.is_none() && self.check.concept.eq_ignore_ascii_case("face") {
            apply_sam_face_evidence(candidate, &boxes);
            if let Some(visual) = candidate.visual.as_mut() {
                visual.source = openreel_media::VisualEvidenceSource::Sam;
                visual.sam_status = openreel_media::SubjectSegmentationStatus::Ready;
                visual.sam_frame_time = Some(*time);
            }
        } else {
            apply_sam_evidence(candidate, &boxes, *time);
        }
        // A successful second check must not erase an earlier incomplete one.
        if let (Some(error), Some(visual)) = (previous_error, candidate.visual.as_mut()) {
            visual.sam_status = openreel_media::SubjectSegmentationStatus::Error;
            visual.sam_error = Some(error);
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(in crate::app) struct Usage {
    pub requests: usize,
    pub completed: usize,
    pub failed: usize,
    pub metered: usize,
    pub requested_frames: usize,
    pub billed_frames: u64,
    pub billed_images: u64,
}

impl Usage {
    pub fn json(&self) -> Value {
        json!({"requests":self.requests, "completed":self.completed, "failed":self.failed,
            "meteredRequests":self.metered, "requestedFrames":self.requested_frames,
            "reportedBilledFrames":self.billed_frames, "reportedBilledImages":self.billed_images})
    }
}

pub(super) struct Analysis {
    pub evidence: Arc<Evidence>,
    pub images: Vec<Value>,
}

pub(super) struct Analyzer<'a> {
    settings: &'a ChatSettings,
    info: &'a MediaInfo,
    agent: ureq::Agent,
    sampler: Option<EvidenceSampler>,
    detail_sampler: Option<EvidenceSampler>,
    unavailable: Option<String>,
}

impl<'a> Analyzer<'a> {
    pub fn new(settings: &'a ChatSettings, info: &'a MediaInfo) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .max_redirects(0)
            .http_status_as_error(false)
            .build();
        Self {
            settings,
            info,
            agent: ureq::Agent::new_with_config(config),
            sampler: None,
            detail_sampler: None,
            unavailable: None,
        }
    }

    pub fn check(
        &mut self,
        check: &Check,
        motion: &[MotionSample],
        usage: &mut Usage,
    ) -> Result<Analysis, String> {
        let requests = usage.requests;
        let result = self.analyze(check, motion, usage);
        if usage.requests > requests {
            if result.is_ok() {
                usage.completed += 1;
            } else {
                usage.failed += 1;
            }
        }
        result
    }

    fn analyze(
        &mut self,
        check: &Check,
        motion: &[MotionSample],
        usage: &mut Usage,
    ) -> Result<Analysis, String> {
        if let Some(error) = &self.unavailable {
            return Err(error.clone());
        }
        if self.sampler.is_none() {
            self.sampler = Some(EvidenceSampler::open(
                &self.info.path,
                self.info.width.min(640),
            )?);
        }
        let mut first = motion.partition_point(|s| s.time < check.start);
        if first > 0 && motion.get(first).is_none_or(|s| s.time > check.start) {
            first -= 1;
        }
        let last = motion.partition_point(|s| s.time < check.end);
        let source_frames = &motion[first..last];
        if source_frames.is_empty() {
            return Err("Subject check contains no source frame".to_owned());
        }
        let mut times = Vec::new();
        for sample in source_frames {
            let time = sample.time;
            if times
                .last()
                .is_none_or(|last| time - last >= 1.0 / SAMPLE_RATE as f64 - 1e-9)
            {
                times.push(time);
            }
        }
        let last_time = source_frames.last().expect("source frames").time;
        if times.last() != Some(&last_time) {
            times.push(last_time);
        }
        if let Some(anchor) = &check.anchor {
            times.push(anchor.time);
            times.sort_by(f64::total_cmp);
            times.dedup();
        }
        let sampler = self.sampler.as_mut().expect("opened sampler");
        let (body, times, size) = if times.len() == 1 {
            let frame = sampler.frame_at(times[0])?;
            let width = self.info.width.min(640).clamp(64, 1280) / 2 * 2;
            let height = openreel_media::scaled_height(self.info.width, self.info.height, width);
            let prompt = subject_prompt(check, &[frame.time], width, height)?;
            (
                sam_request_body_with_prompt(&frame.jpeg, &prompt),
                vec![frame.time],
                Some((width, height)),
            )
        } else {
            let video = sampler.video(&times, SAMPLE_RATE)?;
            let prompt = subject_prompt(check, &video.frame_times, video.width, video.height)?;
            let body = json!({"model":SAM_MODEL, "stream":true,
                "input":[{"type":"message", "role":"user", "content":[
                    {"type":"input_text", "text":prompt},
                    {"type":"input_video", "video_url":format!("data:video/mp4;base64,{}",base64_encode(&video.mp4))}
                ]}], "metadata":{"include_confidence":"true", "mask_encoding":"one_bit", "propagation_direction":"both"}});
            (body, video.frame_times, Some((video.width, video.height)))
        };
        usage.requests += 1;
        usage.requested_frames += times.len();
        let observations = self
            .request(&body, usage)
            .and_then(|output| parse_observations(&output, &times, size))?;
        if check.anchor.is_some() && observations.iter().any(|o| o.bounds.object_id != "1") {
            return Err("SAM returned a different object than the supplied anchor".to_owned());
        }
        let mut evidence = Evidence {
            check: check.clone(),
            times,
            observations,
            details: Vec::new(),
            error: None,
        };
        let mut images = Vec::new();
        // An anchor identifies the intended subject, even beside larger distractors.
        // A concept-only check has no such identity: sample its matching objects
        // over time without describing the largest one as the relevant subject.
        let observations: Vec<_> = evidence
            .observations
            .iter()
            .filter(|o| check.anchor.is_none() || o.bounds.object_id == "1")
            .collect();
        let mut choices = Vec::new();
        for fraction in [0.0, 0.5, 1.0] {
            let target = check.start + fraction * (check.end - check.start);
            if let Some(observation) = observations.iter().copied().min_by(|a, b| {
                (a.time - target)
                    .abs()
                    .total_cmp(&(b.time - target).abs())
                    .then_with(|| a.bounds.object_id.cmp(&b.bounds.object_id))
            }) {
                if !choices.iter().any(|previous: &&Observation| {
                    previous.frame == observation.frame
                        && previous.bounds.object_id == observation.bounds.object_id
                }) {
                    choices.push(observation);
                }
            }
        }
        if !choices.is_empty() && self.detail_sampler.is_none() {
            self.detail_sampler = Some(EvidenceSampler::open(
                &self.info.path,
                self.info.width.min(1280),
            )?);
        }
        for observation in choices {
            let b = &observation.bounds;
            let bounds = [
                b.x1 as f32 / b.width as f32,
                b.y1 as f32 / b.height as f32,
                (b.x2 + 1) as f32 / b.width as f32,
                (b.y2 + 1) as f32 / b.height as f32,
            ];
            let detail = self
                .detail_sampler
                .as_mut()
                .expect("opened detail sampler")
                .detail_at(observation.time, bounds)?;
            if (detail.frame.time - observation.time).abs() > 1e-9 {
                return Err("Subject detail did not match its source picture".to_owned());
            }
            evidence.details.push(Detail {
                time: detail.frame.time,
                object: b.object_id.clone(),
                sharpness: detail.sharpness,
                luminance: detail.luminance,
                clipped: detail.clipped_fraction,
            });
            images.push(json!({"type":"text", "text":format!(
                    "Temporal detail for {}, request-local object {} at source PTS {:.9}. Rectangular crop, not a mask. Judge the question: {}. Keep full-frame context and do not infer unobserved continuity between these pictures.",
                    check.concept, b.object_id, detail.frame.time, check.reason)}));
            images.push(json!({"type":"image_url", "image_url":{"url":format!(
                    "data:image/jpeg;base64,{}",base64_encode(&detail.frame.jpeg))}}));
        }
        Ok(Analysis {
            evidence: Arc::new(evidence),
            images,
        })
    }

    fn request(&mut self, body: &Value, usage: &mut Usage) -> Result<String, String> {
        let mut request = self
            .agent
            .post(sam_responses_url(self.settings))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream");
        if !self.settings.api_key.trim().is_empty() {
            request = request.header(
                "Authorization",
                format!("Bearer {}", self.settings.api_key.trim()),
            );
        }
        let mut response = request
            .send(body.to_string())
            .map_err(|_| "Couldn't reach SAM.".to_owned())?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let error = api_error_for_status(status);
            if matches!(status, 401 | 403 | 404 | 429 | 500 | 502 | 503) {
                self.unavailable = Some(error.clone());
            }
            return Err(error);
        }
        let mut text = String::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|_| "Couldn't read the SAM response.".to_owned())?;
        if text.len() as u64 > MAX_RESPONSE_BYTES {
            return Err("SAM response exceeded the evidence limit.".to_owned());
        }
        for value in text
            .lines()
            .filter_map(|line| line.trim().strip_prefix("data:"))
            .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
            .chain(serde_json::from_str::<Value>(&text).ok())
        {
            let response = value.get("response").unwrap_or(&value);
            let frames = response["usage"]["video_frames_processed"].as_u64();
            let images = response["usage"]["images_processed"].as_u64();
            if frames.is_some() || images.is_some() {
                usage.metered += 1;
                usage.billed_frames = usage.billed_frames.saturating_add(frames.unwrap_or(0));
                usage.billed_images = usage.billed_images.saturating_add(images.unwrap_or(0));
                break;
            }
        }
        sam_output_text(&text)
    }
}

fn parse_observations(
    text: &str,
    times: &[f64],
    size: Option<(u32, u32)>,
) -> Result<Vec<Observation>, String> {
    let mut observations = Vec::new();
    let mut frames = BTreeSet::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let (marker, mut records) = line
            .strip_prefix('<')
            .and_then(|s| s.split_once('>'))
            .ok_or("Malformed SAM frame marker")?;
        let number = marker
            .split(';')
            .next()
            .and_then(|s| s.strip_suffix('f'))
            .ok_or("Malformed SAM frame marker")?;
        if number.is_empty() || !number.bytes().all(|c| c.is_ascii_digit()) {
            return Err("Malformed SAM frame index".to_owned());
        }
        let frame = number
            .parse::<usize>()
            .map_err(|_| "Malformed SAM frame index")?;
        if frame >= times.len() || !frames.insert(frame) {
            return Err("SAM frame index is duplicate or outside the analysis copy".to_owned());
        }
        let mut ids = BTreeSet::new();
        loop {
            records = records
                .trim()
                .strip_prefix(',')
                .unwrap_or(records.trim())
                .trim();
            let digits = records.bytes().take_while(u8::is_ascii_digit).count();
            let id = &records[..digits];
            if id.is_empty() || id.len() > 16 || !ids.insert(id.to_owned()) || ids.len() > 16 {
                return Err("SAM object identity or count is invalid".to_owned());
            }
            records = &records[digits..];
            let mut bounds = None;
            let mut mask = false;
            while let Some(after) = records.trim_start().strip_prefix("<|") {
                let (token, rest) = after.split_once("|>").ok_or("Incomplete SAM token")?;
                let (name, fields) = token.split_once(';').unwrap_or((token, ""));
                if name == "box" {
                    if bounds.is_some() {
                        return Err("Duplicate SAM box".to_owned());
                    }
                    fields_map(fields)?;
                    let mut boxes = parse_sam_boxes(&format!("{id}<|{token}|>"))?;
                    bounds = boxes.pop();
                } else if name == "mask" {
                    if mask {
                        return Err("Duplicate SAM mask".to_owned());
                    }
                    let fields = fields_map(fields)?;
                    if fields.get("x") != Some(&"0") || fields.get("y") != Some(&"0") {
                        return Err("Invalid SAM mask position".to_owned());
                    }
                    let mut data = fields.get("data").ok_or("Missing SAM mask")?.splitn(3, ',');
                    let height = data
                        .next()
                        .and_then(|s| s.parse::<u32>().ok())
                        .ok_or("Invalid mask height")?;
                    let width = data
                        .next()
                        .and_then(|s| s.parse::<u32>().ok())
                        .ok_or("Invalid mask width")?;
                    let payload = data.next().ok_or("Missing mask payload")?;
                    if height == 0
                        || width == 0
                        || height > 4096
                        || width > 4096
                        || payload.len() > 1024 * 1024
                        || payload.len() < 6
                        || !payload.starts_with('!')
                        || !payload
                            .bytes()
                            .all(|c| (b'!'..=b'{').contains(&c) && !b"\"\\,;<>|".contains(&c))
                    {
                        return Err("Invalid SAM mask envelope".to_owned());
                    }
                    // The arithmetic-coded raster stays opaque; only boxes are measured.
                    mask = true;
                } else if name.is_empty()
                    || !name
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
                {
                    return Err("Invalid SAM token".to_owned());
                }
                records = rest;
            }
            let bounds = bounds
                .filter(|_| mask)
                .ok_or("Incomplete SAM object record")?;
            if bounds.width > 4096
                || bounds.height > 4096
                || size.is_some_and(|size| size != (bounds.width, bounds.height))
            {
                return Err("SAM object dimensions are invalid".to_owned());
            }
            observations.push(Observation {
                frame,
                time: times[frame],
                bounds,
            });
            if records.trim().is_empty() {
                break;
            }
            if !records.trim_start().starts_with(',') {
                return Err("Malformed SAM object separator".to_owned());
            }
        }
    }
    observations.sort_by(|a, b| {
        a.frame
            .cmp(&b.frame)
            .then_with(|| a.bounds.object_id.cmp(&b.bounds.object_id))
    });
    Ok(observations)
}

fn subject_prompt(check: &Check, times: &[f64], width: u32, height: u32) -> Result<String, String> {
    let Some(anchor) = &check.anchor else {
        return Ok(check.concept.clone());
    };
    let frame = times
        .iter()
        .position(|time| (*time - anchor.time).abs() < 1e-9)
        .ok_or("Subject anchor is missing from the analysis copy")?;
    let [left, top, right, bottom] = anchor.bounds;
    let x1 = (left as f64 * width as f64).floor() as u32;
    let y1 = (top as f64 * height as f64).floor() as u32;
    let x2 = ((right as f64 * width as f64).ceil() as u32)
        .saturating_sub(1)
        .min(width - 1);
    let y2 = ((bottom as f64 * height as f64).ceil() as u32)
        .saturating_sub(1)
        .min(height - 1);
    if x1 > x2 || y1 > y2 {
        return Err("Subject anchor has no analysis pixels".to_owned());
    }
    Ok(format!(
        "<{frame}f>1<|box;x1={x1};y1={y1};x2={x2};y2={y2};w={width};h={height}|>"
    ))
}

fn fields_map(fields: &str) -> Result<BTreeMap<&str, &str>, String> {
    let mut parsed = BTreeMap::new();
    for field in fields.split(';').filter(|field| !field.trim().is_empty()) {
        let (name, value) = field.split_once('=').unwrap_or((field, ""));
        if parsed.insert(name.trim(), value.trim()).is_some() {
            return Err("Duplicate SAM token field".to_owned());
        }
    }
    Ok(parsed)
}
