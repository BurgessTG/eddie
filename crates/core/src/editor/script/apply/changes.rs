//! A script's JSON object of changes laid over a clip's current
//! settings: unknown names and out-of-range values are refused, with the
//! names and ranges that exist.

use openreel_engine::{
    fill_scale, ChromaKey, ColorAdjust, Filter, Mask, MaskShape, Shape, ShapeKind, TextLook,
    Transform, MAX_OUTLINE, MAX_SHADOW_BLUR, MAX_SHADOW_DISTANCE, MAX_SHAPE_OUTLINE,
    MAX_SHAPE_RATIO, MAX_TEXT_OFFSET, MAX_TEXT_SIZE, MIN_SHAPE_OUTLINE, MIN_SHAPE_RATIO,
    MIN_TEXT_SIZE,
};

/// A script's JSON object of fields to change, or `None` for `null`.
fn changes_object(
    command: &str,
    example: &str,
    changes: &str,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, String> {
    serde_json::from_str(changes)
        .map_err(|_| format!("{command} needs an object such as {example}, or null"))
}

/// `current` with the fields in `changes`; a name it does not have is an
/// error that lists the ones it does.
fn merged<T: serde::Serialize + serde::de::DeserializeOwned>(
    command: &str,
    current: T,
    changes: serde_json::Map<String, serde_json::Value>,
) -> Result<T, String> {
    let mut value = serde_json::to_value(current).map_err(|error| error.to_string())?;
    let fields = value.as_object_mut().expect("settings are an object");
    for (key, change) in changes {
        if !fields.contains_key(&key) {
            let known: Vec<&str> = fields.keys().map(String::as_str).collect();
            return Err(format!(
                "{command} has no '{key}'; use {}",
                known.join(", ")
            ));
        }
        fields.insert(key, change);
    }
    serde_json::from_value(value).map_err(|error| {
        // JSON has no NaN or Infinity: JSON.stringify writes them as null.
        let hint = if error.to_string().contains("null") {
            "; numbers must be finite"
        } else {
            ""
        };
        format!("{command}: {error}{hint}")
    })
}

/// `current` with the fields of the JSON object `changes`, or the identity
/// for `null`. A `scale` of `"fit"` is 1 and `"fill"` covers `canvas`
/// with a `source`-sized picture after the new crop.
pub(super) fn changed_transform(
    current: Transform,
    changes: &str,
    canvas: (f64, f64),
    source: (f64, f64),
) -> Result<Transform, String> {
    let Some(mut changes) = changes_object("setTransform", "{scale: 1.5, x: 0.25}", changes)?
    else {
        return Ok(Transform::default());
    };
    let scale = match changes.get("scale") {
        Some(serde_json::Value::String(word)) => {
            let word = word.clone();
            changes.remove("scale");
            Some(word)
        }
        _ => None,
    };
    let mut transform = merged("setTransform", current, changes)?;
    match scale.as_deref() {
        None => {}
        Some("fit") => transform.scale = 1.0,
        Some("fill") => transform.scale = fill_scale(canvas, source, &transform),
        Some(other) => {
            return Err(format!(
                "setTransform scale is a number, 'fit' or 'fill', not '{other}'"
            ));
        }
    }
    if !transform.is_valid() {
        return Err(format!(
            "setTransform needs a scale of {}-{}, x and y within 10 canvases, and crops of 0 or more that keep {}% of each side",
            openreel_engine::MIN_SCALE,
            openreel_engine::MAX_SCALE,
            openreel_engine::MIN_CROP_KEEP * 100.0
        ));
    }
    Ok(transform)
}

/// `current` with the fields of the JSON object `changes`, or no grade for
/// `null`. A filter is named; an unknown one lists those that exist.
pub(super) fn changed_color(current: ColorAdjust, changes: &str) -> Result<ColorAdjust, String> {
    let Some(changes) = changes_object("setColor", "{saturation: 0.2, filter: 'warm'}", changes)?
    else {
        return Ok(ColorAdjust::default());
    };
    if let Some(name) = changes.get("filter") {
        if name.as_str().and_then(Filter::from_name).is_none() {
            let known: Vec<&str> = Filter::ALL.iter().map(|f| f.as_str()).collect();
            return Err(format!("unknown filter {name}; use {}", known.join(", ")));
        }
    }
    let mut changes = changes;
    if let Some(name) = changes.get_mut("filter") {
        let filter = name
            .as_str()
            .and_then(Filter::from_name)
            .unwrap_or_default();
        *name = serde_json::Value::from(filter.as_str());
    }
    let color = merged("setColor", current, changes)?;
    if !color.is_valid() {
        return Err(format!(
            "setColor takes brightness, contrast, saturation, temperature and tint from -1 to 1, exposure from -{0} to {0} stops and strength from 0 to 1",
            openreel_engine::MAX_EXPOSURE
        ));
    }
    Ok(color)
}

/// `current` (a circle when the clip has no mask) with the fields of the
/// JSON object `changes`, or no mask for `null`.
pub(super) fn changed_mask(current: Option<Mask>, changes: &str) -> Result<Option<Mask>, String> {
    let example = "{shape: 'heart', width: 0.5}";
    let Some(mut changes) = changes_object("setMask", example, changes)? else {
        return Ok(None);
    };
    if let Some(shape) = changes.get_mut("shape") {
        let Some(known) = shape.as_str().and_then(MaskShape::from_name) else {
            let names: Vec<&str> = MaskShape::ALL.iter().map(|s| s.as_str()).collect();
            return Err(format!("setMask shape is one of {}", names.join(", ")));
        };
        *shape = known.as_str().into();
    }
    let mask = merged("setMask", current.unwrap_or_default(), changes)?;
    if !mask.is_valid() {
        return Err(format!(
            "setMask takes x and y from -{0} to {0}, width and height from {1} to {2}, \
             rotation from -360 to 360, and feather and round from 0 to 1",
            openreel_engine::MASK_MAX_OFFSET,
            openreel_engine::MASK_MIN_SIZE,
            openreel_engine::MASK_MAX_SIZE
        ));
    }
    Ok(Some(mask))
}

/// `current` (green when the clip has no key) with the fields of the JSON
/// object `changes`, or no key for `null`.
pub(super) fn changed_chroma(
    current: Option<ChromaKey>,
    changes: &str,
) -> Result<Option<ChromaKey>, String> {
    let example = "{color: '#00B140', strength: 0.5}";
    let Some(changes) = changes_object("setChroma", example, changes)? else {
        return Ok(None);
    };
    let key = merged("setChroma", current.unwrap_or_default(), changes)?;
    if !key.is_valid() {
        return Err("setChroma takes strength, softness and spill from 0 to 1".to_owned());
    }
    Ok(Some(key))
}

/// `current` with the fields of the JSON object `changes`, or the style's
/// look for `null`.
pub(super) fn changed_text(current: TextLook, changes: &str) -> Result<TextLook, String> {
    let Some(mut changes) = changes_object("setText", "{size: 1.5, color: '#FFD600'}", changes)?
    else {
        return Ok(TextLook::default());
    };
    // A field that always has a value, such as bold, size or the shadow's
    // angle, goes back to its default for null: plain, the style's size,
    // the usual angle.
    let defaults = serde_json::to_value(TextLook::default()).expect("a text look is JSON");
    for (key, value) in &mut changes {
        if let Some(default) = defaults
            .get(key)
            .filter(|d| value.is_null() && !d.is_null())
        {
            *value = default.clone();
        }
    }
    let text = merged("setText", current, changes)?;
    if !text.is_valid() {
        return Err(format!(
            "setText takes a size of {MIN_TEXT_SIZE} to {MAX_TEXT_SIZE}, a boxOpacity and shadowOpacity of 0 to 1, an outline of 0 to {MAX_OUTLINE}, a shadowDistance of 0 to {MAX_SHADOW_DISTANCE}, a shadowAngle of -180 to 180, a shadowBlur of 0 to {MAX_SHADOW_BLUR} and x and y from -{MAX_TEXT_OFFSET} to {MAX_TEXT_OFFSET}"
        ));
    }
    Ok(text)
}
/// `current` with the fields of the JSON object `changes`, or a plain
/// white shape of its kind for `null`. A new `kind` brings its own
/// proportions and corners unless `ratio` or `round` is given.
pub(super) fn changed_shape(command: &str, current: Shape, changes: &str) -> Result<Shape, String> {
    let example = "{kind: 'arrow', fill: '#FFD600'}";
    let Some(mut changes) = changes_object(command, example, changes)? else {
        return Ok(Shape::new(current.kind));
    };
    let mut base = current;
    if let Some(kind) = changes.remove("kind") {
        let Some(kind) = kind.as_str().and_then(ShapeKind::from_name) else {
            let names: Vec<&str> = ShapeKind::ALL.iter().map(|kind| kind.as_str()).collect();
            return Err(format!("{command} kind is one of {}", names.join(", ")));
        };
        base = base.with_kind(kind);
    }
    let shape = merged(command, base, changes)?;
    if !shape.is_valid() {
        return Err(format!(
            "{command} needs a fill or an outline ('#RRGGBB'), a thickness of {MIN_SHAPE_OUTLINE} to {MAX_SHAPE_OUTLINE}, round from 0 to 1 and a ratio of {MIN_SHAPE_RATIO} to {MAX_SHAPE_RATIO}"
        ));
    }
    Ok(shape)
}
