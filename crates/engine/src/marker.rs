//! Markers: notes pinned to a moment of the timeline, as CapCut's markers
//! on the ruler. A marker names a time, not footage, so it stays put when
//! clips move, ripple or are deleted around it.

use crate::{Timeline, MAX_TIMELINE_SECONDS};

pub type MarkerId = u64;

/// A marker's colour, picked from CapCut-like flags. Yellow is the
/// default; "a marker with a new colour" takes the next one in this order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkerColor {
    #[default]
    Yellow,
    Orange,
    Red,
    Pink,
    Purple,
    Blue,
    Cyan,
    Green,
}

impl MarkerColor {
    pub const ALL: [Self; 8] = [
        Self::Yellow,
        Self::Orange,
        Self::Red,
        Self::Pink,
        Self::Purple,
        Self::Blue,
        Self::Cyan,
        Self::Green,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yellow => "yellow",
            Self::Orange => "orange",
            Self::Red => "red",
            Self::Pink => "pink",
            Self::Purple => "purple",
            Self::Blue => "blue",
            Self::Cyan => "cyan",
            Self::Green => "green",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim().to_lowercase();
        Self::ALL.into_iter().find(|color| color.as_str() == name)
    }

    /// The colour after this one, wrapping round.
    pub fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|color| *color == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Marker {
    pub id: MarkerId,
    /// Timeline seconds, on the frame grid.
    pub time: f64,
    /// The note, empty when there is none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default)]
    pub color: MarkerColor,
}

impl Timeline {
    /// Add a marker at `time`, moved onto the frame grid, and return it.
    pub fn add_marker(&mut self, time: f64, name: &str, color: MarkerColor) -> MarkerId {
        let id = self.next_marker;
        self.next_marker += 1;
        let time = self.snap(time.clamp(0.0, MAX_TIMELINE_SECONDS));
        self.markers.push(Marker {
            id,
            time,
            name: name.to_owned(),
            color,
        });
        id
    }

    pub fn marker(&self, id: MarkerId) -> Option<&Marker> {
        self.markers.iter().find(|marker| marker.id == id)
    }

    pub fn marker_mut(&mut self, id: MarkerId) -> Option<&mut Marker> {
        self.markers.iter_mut().find(|marker| marker.id == id)
    }

    /// Move a marker to `time`, on the frame grid. False when there is no
    /// such marker.
    pub fn move_marker(&mut self, id: MarkerId, time: f64) -> bool {
        let time = self.snap(time.clamp(0.0, MAX_TIMELINE_SECONDS));
        match self.marker_mut(id) {
            Some(marker) => {
                marker.time = time;
                true
            }
            None => false,
        }
    }

    /// False when there is no such marker.
    pub fn remove_marker(&mut self, id: MarkerId) -> bool {
        let before = self.markers.len();
        self.markers.retain(|marker| marker.id != id);
        self.markers.len() != before
    }

    /// A marker on the frame at `time`, the newest when several are.
    pub fn marker_at(&self, time: f64) -> Option<&Marker> {
        let slack = 0.5 / self.fps;
        self.markers
            .iter()
            .rev()
            .find(|marker| (marker.time - time).abs() < slack)
    }

    /// The nearest marker after the frame at `time`, or before it.
    pub fn marker_beside(&self, time: f64, forward: bool) -> Option<&Marker> {
        let slack = 0.5 / self.fps;
        let after = |marker: &&Marker| {
            if forward {
                marker.time > time + slack
            } else {
                marker.time < time - slack
            }
        };
        let distance = |marker: &&Marker| (marker.time - time).abs();
        self.markers
            .iter()
            .filter(after)
            .min_by(|a, b| distance(a).total_cmp(&distance(b)))
    }

    /// The colour a marker "with a new colour" takes: the one after the
    /// newest marker's.
    pub fn next_marker_color(&self) -> MarkerColor {
        self.markers
            .iter()
            .max_by_key(|marker| marker.id)
            .map_or(MarkerColor::default(), |marker| marker.color.next())
    }

    /// Every marker sorted by time, then by when it was added.
    pub fn markers_in_order(&self) -> Vec<&Marker> {
        let mut markers: Vec<&Marker> = self.markers.iter().collect();
        markers.sort_by(|a, b| a.time.total_cmp(&b.time).then(a.id.cmp(&b.id)));
        markers
    }

    pub(crate) fn markers_in_range(&self) -> bool {
        self.markers.iter().all(|marker| {
            marker.time.is_finite() && (0.0..=MAX_TIMELINE_SECONDS).contains(&marker.time)
        })
    }
}
