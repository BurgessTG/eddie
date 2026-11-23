//! Keyframes: the Details panel's key buttons, the values its fields and
//! the monitor write at the playhead once a property has keys, and the
//! script commands. Keys live on the clip's own clock
//! (`openreel_engine::Keyframes`); here they are set at timeline times.

use openreel_engine::{Clip, ClipId, ClipKind, Ease, Property, KEY_TOLERANCE, MAX_KEYS};

use super::Editor;

/// Whether `clip` can key `property`: a picture's place, size, turn and
/// opacity on video clips, and volume on anything with sound.
pub fn keyable(clip: &Clip, property: Property) -> bool {
    match property {
        Property::Volume => clip.kind != ClipKind::Title,
        _ => clip.kind == ClipKind::Video,
    }
}

/// `clip`'s plain value of `property`, the one it has without keys.
fn set_plain(clip: &mut Clip, property: Property, value: f64) {
    match property {
        Property::X => clip.transform.x = value,
        Property::Y => clip.transform.y = value,
        Property::Scale => clip.transform.scale = value,
        Property::Rotation => clip.transform.rotation = value,
        Property::Opacity => clip.opacity = value as f32,
        Property::Volume => clip.volume = value as f32,
    }
}

impl Editor {
    /// The playhead on clip `id`'s own clock, held inside the clip.
    pub fn clip_time(&self, id: ClipId) -> Option<f64> {
        let clip = self.timeline.clip(id)?;
        Some((self.playhead - clip.start).clamp(0.0, clip.duration))
    }

    /// Whether each of `properties` has a key at the playhead.
    pub fn keyed_here(&self, id: ClipId, properties: &[Property]) -> bool {
        let (Some(clip), Some(at)) = (self.timeline.clip(id), self.clip_time(id)) else {
            return false;
        };
        properties
            .iter()
            .all(|&p| clip.keyframes.key_at(p, at).is_some())
    }

    /// Set `property` of clip `id` to `value`: a key at the playhead once
    /// it has keys, else its plain value.
    pub(super) fn write_property(&mut self, id: ClipId, property: Property, value: f64) {
        let Some(at) = self.clip_time(id) else {
            return;
        };
        let Some(clip) = self.timeline.clip_mut(id) else {
            return;
        };
        if clip.keyframes.keys(property).is_empty() {
            set_plain(clip, property, value);
        } else {
            clip.keyframes.set(property, at, value);
        }
    }

    /// The key button: add a key for each of `properties` at the playhead,
    /// holding the value each has there, or take them away when every one
    /// has a key there. A property left without keys keeps the value it had
    /// at the playhead.
    pub fn toggle_keys(&mut self, id: ClipId, properties: &[Property]) {
        let Some(clip) = self.timeline.clip(id) else {
            return;
        };
        if !properties.iter().all(|&p| keyable(clip, p)) {
            self.status = "This clip cannot take that key.".to_owned();
            return;
        }
        let slack = 0.5 / self.timeline.fps;
        if self.playhead < clip.start - slack || self.playhead > clip.end() + slack {
            self.status = "Move the playhead onto the clip to set a key.".to_owned();
            return;
        }
        let time = self.playhead;
        let remove = self.keyed_here(id, properties);
        let at = self.clip_time(id).unwrap_or(0.0);
        let full = properties
            .iter()
            .any(|&p| clip.keyframes.keys(p).len() >= MAX_KEYS);
        if !remove && full {
            self.status = format!("A property takes at most {MAX_KEYS} keys.");
            return;
        }
        let values: Vec<f64> = properties
            .iter()
            .map(|&p| clip.property_at(p, time))
            .collect();
        let Some(clip) = self.timeline.clip_mut(id) else {
            return;
        };
        for (&property, value) in properties.iter().zip(values) {
            if remove {
                clip.keyframes.remove(property, at);
                if clip.keyframes.keys(property).is_empty() {
                    set_plain(clip, property, value);
                }
            } else {
                clip.keyframes.set(property, at, value);
            }
        }
        self.status = if remove { "Key removed." } else { "Key added." }.to_owned();
    }

    /// How clip `id`'s values run on from the playhead: the ease of every
    /// property's run between two keys there (the key at or before the
    /// playhead to the next). `None` when no run holds the playhead,
    /// `Some(None)` when the runs there differ.
    pub fn ease_here(&self, id: ClipId) -> Option<Option<Ease>> {
        let (clip, at) = (self.timeline.clip(id)?, self.clip_time(id)?);
        let mut eases = Property::ALL.iter().filter_map(|&p| {
            let index = clip.keyframes.run_at(p, at)?;
            Some(clip.keyframes.keys(p)[index].ease)
        });
        let first = eases.next()?;
        Some(eases.all(|ease| ease == first).then_some(first))
    }

    /// Give every run of clip `id` that holds the playhead ([`Self::ease_here`])
    /// `ease`.
    pub fn set_ease_here(&mut self, id: ClipId, ease: Ease) {
        let Some(at) = self.clip_time(id) else {
            return;
        };
        let Some(clip) = self.timeline.clip_mut(id) else {
            return;
        };
        let mut changed = false;
        for property in Property::ALL {
            changed |= clip.keyframes.set_ease(property, at, ease);
        }
        if !changed {
            self.status = "Move the playhead between two keys to ease the change.".to_owned();
        }
    }

    /// The timeline time of clip `id`'s previous or next key, of any
    /// property, inside the clip.
    pub fn key_beside(&self, id: ClipId, forward: bool) -> Option<f64> {
        let clip = self.timeline.clip(id)?;
        let here = self.playhead;
        let times = clip
            .keyframes
            .times()
            .into_iter()
            .filter(|at| (0.0..=clip.duration).contains(at))
            .map(|at| clip.start + at);
        if forward {
            times
                .filter(|&time| time > here + KEY_TOLERANCE)
                .reduce(f64::min)
        } else {
            times
                .filter(|&time| time < here - KEY_TOLERANCE)
                .reduce(f64::max)
        }
    }

    /// Move the playhead to clip `id`'s previous or next key. False when
    /// there is none that way.
    pub fn seek_key(&mut self, id: ClipId, forward: bool) -> bool {
        match self.key_beside(id, forward) {
            Some(time) => {
                self.seek_and_reveal(time);
                true
            }
            None => false,
        }
    }

    /// `setKeyframe`: `property` of clip `id` is `value` at timeline time
    /// `time`, which must be on the clip, running on to the next key along
    /// `ease` (a name; empty keeps a key's ease, and a new key is linear).
    pub(super) fn set_keyframe(
        &mut self,
        id: ClipId,
        name: &str,
        time: f64,
        value: f64,
        ease: &str,
    ) -> Result<(), String> {
        let (clip, property) = self.key_target(id, name, "setKeyframe")?;
        let ease = match ease {
            "" => None,
            name => Some(Ease::from_name(name).ok_or_else(|| {
                let names: Vec<&str> = Ease::ALL.iter().map(|e| e.as_str()).collect();
                format!(
                    "setKeyframe: an ease is one of {}, not '{name}'",
                    names.join(", ")
                )
            })?),
        };
        let slack = 0.5 / self.timeline.fps;
        if time < clip.start - slack || time > clip.end() + slack {
            return Err(format!(
                "setKeyframe: {time} s is not on clip {id} ({:.3} to {:.3} s)",
                clip.start,
                clip.end()
            ));
        }
        if !property.accepts(value) {
            return Err(format!(
                "setKeyframe: {value} is out of range for {}; {}",
                property.as_str(),
                match property {
                    Property::X | Property::Y => "-10 to 10 canvases",
                    Property::Scale => "0.05 to 20 times the fitted size",
                    Property::Rotation => "-3600 to 3600 degrees",
                    Property::Opacity => "0 to 1",
                    Property::Volume => "gain 0 to 2",
                }
            ));
        }
        let at = (time - clip.start).clamp(0.0, clip.duration);
        let clip = self.timeline.clip_mut(id).expect("checked above");
        if clip.keyframes.key_at(property, at).is_none()
            && clip.keyframes.keys(property).len() >= MAX_KEYS
        {
            return Err(format!(
                "setKeyframe: clip {id}'s {} has {MAX_KEYS} keys, the most it takes",
                property.as_str()
            ));
        }
        clip.keyframes.set(property, at, value);
        if let Some(ease) = ease {
            clip.keyframes.ease_key(property, at, ease);
        }
        Ok(())
    }

    /// `removeKeyframe`: take away the key at timeline time `time`.
    pub(super) fn remove_keyframe(
        &mut self,
        id: ClipId,
        name: &str,
        time: f64,
    ) -> Result<(), String> {
        let (clip, property) = self.key_target(id, name, "removeKeyframe")?;
        let at = time - clip.start;
        let value = clip.property_at(property, time);
        let clip = self.timeline.clip_mut(id).expect("checked above");
        if !clip.keyframes.remove(property, at) {
            return Err(format!(
                "removeKeyframe: clip {id} has no {} key at {time} s",
                property.as_str()
            ));
        }
        if clip.keyframes.keys(property).is_empty() {
            set_plain(clip, property, value);
        }
        Ok(())
    }

    /// `clearKeyframes`: take away `name`'s keys, or every property's for
    /// an empty name. Each keeps the value it had at the clip's start.
    pub(super) fn clear_keyframes(&mut self, id: ClipId, name: &str) -> Result<(), String> {
        let properties = if name.is_empty() {
            Property::ALL.to_vec()
        } else {
            vec![self.key_target(id, name, "clearKeyframes")?.1]
        };
        let Some(clip) = self.timeline.clip_mut(id) else {
            return Err(format!("clearKeyframes: there is no clip {id}"));
        };
        for property in properties {
            if let Some(first) = clip.keyframes.value(property, 0.0) {
                set_plain(clip, property, first);
                clip.keyframes.clear(property);
            }
        }
        Ok(())
    }

    /// The clip and property a key command names, checked.
    fn key_target(
        &self,
        id: ClipId,
        name: &str,
        command: &str,
    ) -> Result<(Clip, Property), String> {
        let clip = self
            .timeline
            .clip(id)
            .ok_or_else(|| format!("{command}: there is no clip {id}"))?;
        let property = Property::from_name(name).ok_or_else(|| {
            let known: Vec<&str> = Property::ALL.iter().map(|p| p.as_str()).collect();
            format!(
                "{command}: unknown property '{name}'; use {}",
                known.join(", ")
            )
        })?;
        if !keyable(clip, property) {
            return Err(format!(
                "{command}: clip {id} is {} and has no {} to key",
                clip.kind.as_str(),
                property.as_str()
            ));
        }
        Ok((clip.clone(), property))
    }
}
