//! Keyframes: a clip's position, scale, rotation, opacity or volume
//! changing over the clip. Each property has its own keys, at clip-local
//! seconds (from the clip's start); the value runs from one key to the next
//! along the first key's [`Ease`] and holds before the first key and after
//! the last.
//! Edits that move a clip's content against its start (a left trim, a
//! split, a speed change) move the keys with the content.

use crate::{MAX_SCALE, MIN_SCALE};

/// A property keys can change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Property {
    /// The picture's centre, as [`Transform::x`](crate::Transform) and `y`.
    X,
    Y,
    Scale,
    /// Clockwise degrees; keys past 360 keep turning.
    Rotation,
    Opacity,
    Volume,
}

/// The most keys one property may have.
pub const MAX_KEYS: usize = 256;
/// Keys closer than this are one key: setting one there replaces it.
pub const KEY_TOLERANCE: f64 = 0.0005;
/// The furthest a rotation key may turn either way, in degrees.
pub const MAX_ROTATION: f64 = 3600.0;

impl Property {
    pub const ALL: [Self; 6] = [
        Self::X,
        Self::Y,
        Self::Scale,
        Self::Rotation,
        Self::Opacity,
        Self::Volume,
    ];

    /// The name scripts and project files use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::X => "x",
            Self::Y => "y",
            Self::Scale => "scale",
            Self::Rotation => "rotation",
            Self::Opacity => "opacity",
            Self::Volume => "volume",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_lowercase();
        Self::ALL.into_iter().find(|p| p.as_str() == name)
    }

    /// Whether a clip's picture moves or fades with this property.
    pub fn is_picture(self) -> bool {
        self != Self::Volume
    }

    /// Whether `value` is one this property can take, as the renderers and
    /// [`Transform::is_valid`](crate::Transform::is_valid) accept it.
    pub fn accepts(self, value: f64) -> bool {
        value.is_finite()
            && match self {
                Self::X | Self::Y => value.abs() <= 10.0,
                Self::Scale => (MIN_SCALE..=MAX_SCALE).contains(&value),
                Self::Rotation => value.abs() <= MAX_ROTATION,
                Self::Opacity => (0.0..=1.0).contains(&value),
                Self::Volume => (0.0..=2.0).contains(&value),
            }
    }
}

/// How a value runs from one key to the next. None overshoots: every value
/// between two keys lies between theirs, so the keys hold the extremes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ease {
    /// A straight line, at one speed.
    #[default]
    Linear,
    /// Starts slowly and speeds up: t³.
    EaseIn,
    /// Starts fast and slows down to the next key: 1 - (1 - t)³.
    EaseOut,
    /// Slow at both keys, fastest halfway: ease in, then ease out.
    EaseInOut,
}

impl Ease {
    pub const ALL: [Self; 4] = [Self::Linear, Self::EaseIn, Self::EaseOut, Self::EaseInOut];

    /// The name scripts and project files use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::EaseIn => "ease-in",
            Self::EaseOut => "ease-out",
            Self::EaseInOut => "ease-in-out",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_lowercase();
        Self::ALL.into_iter().find(|e| e.as_str() == name)
    }

    pub fn is_linear(&self) -> bool {
        *self == Self::Linear
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::EaseIn => "Ease in",
            Self::EaseOut => "Ease out",
            Self::EaseInOut => "Ease in and out",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Self::Linear => "Change at one speed from this key to the next",
            Self::EaseIn => "Start slowly and speed up into the next key",
            Self::EaseOut => "Start fast and slow down into the next key",
            Self::EaseInOut => "Start and end slowly, fastest halfway",
        }
    }

    /// How far from the first key's value to the next's, 0 to 1, at `t` of
    /// the way between their times (0 to 1).
    pub fn apply(self, t: f64) -> f64 {
        match self {
            Self::Linear => t,
            Self::EaseIn => t * t * t,
            Self::EaseOut => 1.0 - (1.0 - t).powi(3),
            Self::EaseInOut if t < 0.5 => 4.0 * t * t * t,
            Self::EaseInOut => 1.0 - (2.0 - 2.0 * t).powi(3) / 2.0,
        }
    }
}

/// One key: `value` at `at` seconds after the clip's start, and how the
/// value runs from here to the next key.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Key {
    pub at: f64,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Ease::is_linear")]
    pub ease: Ease,
}

/// An `f32` value (opacity and volume are) as the `f64` a key holds: the
/// shortest decimal that reads back as it, so 0.3 stays 0.3 rather than
/// 0.30000001192092896.
pub fn widen(value: f32) -> f64 {
    value.to_string().parse().unwrap_or(f64::from(value))
}

/// The value keys in time order give at `at`: from each key to the next
/// along the first's [`Ease`], the first key's value before it and the
/// last's after it. `None` without keys.
pub fn key_value(keys: &[Key], at: f64) -> Option<f64> {
    let first = keys.first()?;
    if at <= first.at {
        return Some(first.value);
    }
    let next = keys.partition_point(|key| key.at <= at);
    let Some(b) = keys.get(next) else {
        return keys.last().map(|key| key.value);
    };
    let a = keys[next - 1];
    let t = (at - a.at) / (b.at - a.at);
    Some(a.value + (b.value - a.value) * a.ease.apply(t))
}

/// A clip's keys, per property, each list in time order.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Keyframes {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub x: Vec<Key>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub y: Vec<Key>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scale: Vec<Key>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rotation: Vec<Key>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub opacity: Vec<Key>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub volume: Vec<Key>,
}

impl Keyframes {
    pub fn is_empty(&self) -> bool {
        Property::ALL.iter().all(|&p| self.keys(p).is_empty())
    }

    /// Whether position, scale, rotation or opacity has keys.
    pub fn moves_picture(&self) -> bool {
        Property::ALL
            .iter()
            .any(|&p| p.is_picture() && !self.keys(p).is_empty())
    }

    /// Whether position, scale or rotation has keys.
    pub fn moves_transform(&self) -> bool {
        [
            Property::X,
            Property::Y,
            Property::Scale,
            Property::Rotation,
        ]
        .iter()
        .any(|&p| !self.keys(p).is_empty())
    }

    pub fn keys(&self, property: Property) -> &[Key] {
        match property {
            Property::X => &self.x,
            Property::Y => &self.y,
            Property::Scale => &self.scale,
            Property::Rotation => &self.rotation,
            Property::Opacity => &self.opacity,
            Property::Volume => &self.volume,
        }
    }

    fn keys_mut(&mut self, property: Property) -> &mut Vec<Key> {
        match property {
            Property::X => &mut self.x,
            Property::Y => &mut self.y,
            Property::Scale => &mut self.scale,
            Property::Rotation => &mut self.rotation,
            Property::Opacity => &mut self.opacity,
            Property::Volume => &mut self.volume,
        }
    }

    /// The value at clip-local `at`, or `None` when the property has no keys.
    pub fn value(&self, property: Property, at: f64) -> Option<f64> {
        key_value(self.keys(property), at)
    }

    /// The key within [`KEY_TOLERANCE`] of clip-local `at`.
    pub fn key_at(&self, property: Property, at: f64) -> Option<Key> {
        self.keys(property)
            .iter()
            .copied()
            .find(|key| (key.at - at).abs() <= KEY_TOLERANCE)
    }

    /// Set `property` to `value` at clip-local `at`, replacing the value of
    /// a key there and keeping its ease; a new key runs on linearly.
    pub fn set(&mut self, property: Property, at: f64, value: f64) {
        let keys = self.keys_mut(property);
        if let Some(key) = keys
            .iter_mut()
            .find(|key| (key.at - at).abs() <= KEY_TOLERANCE)
        {
            key.value = value;
            return;
        }
        let index = keys.partition_point(|key| key.at < at);
        let ease = Ease::Linear;
        keys.insert(index, Key { at, value, ease });
    }

    /// The key whose run to the next key holds clip-local `at`: the last key
    /// at or before it, when a key follows. A key within [`KEY_TOLERANCE`]
    /// after `at` counts as at it.
    pub fn run_at(&self, property: Property, at: f64) -> Option<usize> {
        let keys = self.keys(property);
        let next = keys.partition_point(|key| key.at <= at + KEY_TOLERANCE);
        (next > 0 && next < keys.len()).then(|| next - 1)
    }

    /// Give the key within [`KEY_TOLERANCE`] of clip-local `at` `ease`, for
    /// the run from it to the next key; false when there is none.
    pub fn ease_key(&mut self, property: Property, at: f64, ease: Ease) -> bool {
        match self
            .keys_mut(property)
            .iter_mut()
            .find(|key| (key.at - at).abs() <= KEY_TOLERANCE)
        {
            Some(key) => {
                key.ease = ease;
                true
            }
            None => false,
        }
    }

    /// Give the run holding clip-local `at` ([`Self::run_at`]) `ease`; false
    /// when no run holds it.
    pub fn set_ease(&mut self, property: Property, at: f64, ease: Ease) -> bool {
        match self.run_at(property, at) {
            Some(index) => {
                self.keys_mut(property)[index].ease = ease;
                true
            }
            None => false,
        }
    }

    /// Remove the key at clip-local `at`; false when there is none.
    pub fn remove(&mut self, property: Property, at: f64) -> bool {
        let keys = self.keys_mut(property);
        let before = keys.len();
        keys.retain(|key| (key.at - at).abs() > KEY_TOLERANCE);
        keys.len() != before
    }

    pub fn clear(&mut self, property: Property) {
        self.keys_mut(property).clear();
    }

    /// Move every key `seconds` later on the clip's clock: a clip whose
    /// start moves `seconds` earlier against its content keeps each key on
    /// the same moment of it.
    pub fn shift(&mut self, seconds: f64) {
        for &p in &Property::ALL {
            self.keys_mut(p)
                .iter_mut()
                .for_each(|key| key.at += seconds);
        }
    }

    /// Stretch every key's time by `factor`, as a speed change stretches
    /// the content.
    pub fn stretch(&mut self, factor: f64) {
        for &p in &Property::ALL {
            self.keys_mut(p).iter_mut().for_each(|key| key.at *= factor);
        }
    }

    /// Every key's clip-local time, once each, in order.
    pub fn times(&self) -> Vec<f64> {
        let mut times: Vec<f64> = Property::ALL
            .iter()
            .flat_map(|&p| self.keys(p).iter().map(|key| key.at))
            .collect();
        times.sort_by(f64::total_cmp);
        times.dedup_by(|a, b| (*a - *b).abs() <= KEY_TOLERANCE);
        times
    }

    /// Whether every key is one the renderers can use: finite, in time
    /// order with room between, at most [`MAX_KEYS`] a property, and each
    /// value one its property [`accepts`](Property::accepts).
    pub fn is_valid(&self) -> bool {
        Property::ALL.iter().all(|&p| {
            let keys = self.keys(p);
            keys.len() <= MAX_KEYS
                && keys
                    .iter()
                    .all(|key| key.at.is_finite() && key.at.abs() <= 1e6 && p.accepts(key.value))
                && keys.windows(2).all(|w| w[1].at - w[0].at > KEY_TOLERANCE)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(pairs: &[(f64, f64)]) -> Keyframes {
        let mut k = Keyframes::default();
        for &(at, value) in pairs {
            k.set(Property::Opacity, at, value);
        }
        k
    }

    #[test]
    fn values_run_straight_between_keys_and_hold_outside_them() {
        let k = keys(&[(3.0, 0.2), (1.0, 1.0), (4.0, 0.6)]);
        let v = |at| k.value(Property::Opacity, at).unwrap();
        assert_eq!(v(0.0), 1.0);
        assert_eq!(v(1.0), 1.0);
        // A quarter of the way from 1.0 to 0.2.
        assert!((v(1.5) - 0.8).abs() < 1e-12);
        assert!((v(3.5) - 0.4).abs() < 1e-12);
        assert_eq!(v(9.0), 0.6);
        assert_eq!(k.value(Property::Scale, 2.0), None);
    }

    /// Each ease against its formula at a quarter, half and three quarters
    /// of the way from 0.2 to 1.0 (over 2 s from 1 s), worked by hand: ease
    /// in 0.25³ = 0.015625, ease out 1 - 0.75³ = 0.578125, ease in and out
    /// 4 x 0.25³ = 0.0625 and 1 - (0.5)³/2 = 0.9375.
    #[test]
    fn eased_values_follow_their_curves_and_stay_between_the_keys() {
        for (ease, quarter, half, three) in [
            (Ease::Linear, 0.25, 0.5, 0.75),
            (Ease::EaseIn, 0.015625, 0.125, 0.421875),
            (Ease::EaseOut, 0.578125, 0.875, 0.984375),
            (Ease::EaseInOut, 0.0625, 0.5, 0.9375),
        ] {
            let mut k = keys(&[(1.0, 0.2), (3.0, 1.0), (4.0, 0.0)]);
            assert!(k.set_ease(Property::Opacity, 2.2, ease));
            let v = |at| k.value(Property::Opacity, at).unwrap();
            for (at, share) in [(1.5, quarter), (2.0, half), (2.5, three)] {
                let want = 0.2 + 0.8 * share;
                assert!((v(at) - want).abs() < 1e-12, "{ease:?} at {at}: {}", v(at));
            }
            // The next run stays straight, and nothing leaves 0..1.
            assert!((v(3.5) - 0.5).abs() < 1e-12, "{ease:?}");
            assert!((0..=400).all(|i| (0.0..=1.0).contains(&v(f64::from(i) / 100.0))));
            // A new value keeps the key's ease; only a key with a key after
            // it starts a run.
            k.set(Property::Opacity, 1.0, 0.4);
            assert_eq!(k.opacity[0].ease, ease);
            assert_eq!(k.run_at(Property::Opacity, 4.0), None);
            assert_eq!(k.run_at(Property::Opacity, 0.5), None);
            assert_eq!(k.run_at(Property::Opacity, 2.9998), Some(1));
        }
    }

    #[test]
    fn a_key_near_another_replaces_it_and_shifts_move_every_key() {
        let mut k = keys(&[(1.0, 0.5), (2.0, 0.25)]);
        k.set(Property::Opacity, 1.0002, 0.75);
        assert_eq!(k.opacity.len(), 2);
        assert_eq!(k.opacity[0].value, 0.75);
        assert!(k.remove(Property::Opacity, 2.0003));
        assert!(!k.remove(Property::Opacity, 5.0));
        k.set(Property::Volume, 3.0, 2.0);
        k.shift(-0.5);
        k.stretch(2.0);
        assert_eq!(k.opacity[0].at, 1.0);
        assert_eq!(k.volume[0].at, 5.0);
        assert_eq!(k.times(), vec![1.0, 5.0]);
    }

    #[test]
    fn values_a_renderer_cannot_take_are_invalid() {
        assert!(keys(&[(0.0, 1.0)]).is_valid());
        let key = |at, value| Key {
            at,
            value,
            ease: Ease::Linear,
        };
        for bad in [
            Keyframes {
                scale: vec![key(0.0, 0.0)],
                ..Keyframes::default()
            },
            Keyframes {
                x: vec![key(1.0, 0.0), key(0.5, 0.0)],
                ..Keyframes::default()
            },
            Keyframes {
                rotation: vec![key(f64::NAN, 0.0)],
                ..Keyframes::default()
            },
        ] {
            assert!(!bad.is_valid(), "{bad:?}");
        }
        assert!(!Property::Volume.accepts(2.5));
        assert!(Property::Rotation.accepts(720.0));
    }
}
