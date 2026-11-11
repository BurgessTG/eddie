//! Where the timeline lanes look: zoom, scroll and the magnet. The editor
//! keeps the playhead in view with it and scripts set zoom and magnet; the
//! front end draws with it and reports the lanes' width and held gestures.

/// As far out as the lanes zoom, unless the timeline takes more to fit.
pub const MIN_ZOOM: f32 = 4.0;
pub const MAX_ZOOM: f32 = 600.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimelineViewport {
    pub px_per_sec: f32,
    pub scroll_x: f32,
    /// Dragged edges snap to other clips' edges and the playhead.
    pub magnet: bool,
    /// Width of the lanes right of the track headers, once drawn. Until
    /// then nothing scrolls: there is no view to keep the playhead in.
    pub width: Option<f32>,
    /// The ruler is being scrubbed or a clip dragged this frame.
    pub interacting: bool,
}

impl Default for TimelineViewport {
    fn default() -> Self {
        Self {
            px_per_sec: 48.0,
            scroll_x: 0.0,
            magnet: true,
            width: None,
            interacting: false,
        }
    }
}

impl TimelineViewport {
    /// The zoom at which `duration` seconds fill the lanes.
    fn fit(&self, duration: f64) -> Option<f32> {
        let width = self.width?;
        (duration > 0.0).then(|| (width - 24.0).max(40.0) / duration as f32)
    }

    /// As far out as a timeline of `duration` seconds zooms: `MIN_ZOOM`,
    /// or further when it takes that to show all of it.
    pub fn min_zoom(&self, duration: f64) -> f32 {
        self.fit(duration).map_or(MIN_ZOOM, |fit| fit.min(MIN_ZOOM))
    }

    /// `duration` is the timeline's, which sets how far out it goes.
    pub fn set_zoom(&mut self, px_per_sec: f32, duration: f64) {
        self.px_per_sec = px_per_sec.clamp(self.min_zoom(duration), MAX_ZOOM);
    }

    /// Zoom by `factor`, keeping `anchor` at the same screen position.
    pub fn zoom(&mut self, factor: f32, anchor: f64, duration: f64) {
        let offset = anchor as f32 * self.px_per_sec - self.scroll_x;
        self.set_zoom(self.px_per_sec * factor, duration);
        self.scroll_x = (anchor as f32 * self.px_per_sec - offset).max(0.0);
    }

    /// Zoom so `duration` seconds fill the lanes.
    pub fn zoom_to_fit(&mut self, duration: f64) {
        let Some(fit) = self.fit(duration) else {
            return;
        };
        self.set_zoom(fit, duration);
        self.scroll_x = 0.0;
    }

    /// Scroll just enough to keep `time` inside the visible lanes.
    pub fn reveal(&mut self, time: f64) {
        let Some(width) = self.width else {
            return;
        };
        let margin = (width * 0.1).min(60.0);
        let x = time as f32 * self.px_per_sec - self.scroll_x;
        if x < margin {
            self.scroll_x = (time as f32 * self.px_per_sec - margin).max(0.0);
        } else if x > width - margin {
            self.scroll_x = time as f32 * self.px_per_sec - (width - margin);
        }
    }

    /// Playback follow: once the playhead reaches the middle of the lanes
    /// it stays there and the clips scroll under it. A view the user
    /// scrolled away from the playhead (`was` off screen) is left alone.
    pub fn follow(&mut self, was: f64, now: f64) {
        let Some(width) = self.width else {
            return;
        };
        let x = |time: f64| time as f32 * self.px_per_sec - self.scroll_x;
        if (0.0..=width).contains(&x(was)) && x(now) > width * 0.5 {
            self.scroll_x = now as f32 * self.px_per_sec - width * 0.5;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_scrolls_smoothly_but_leaves_a_scrolled_away_view_alone() {
        let mut view = TimelineViewport {
            px_per_sec: 10.0,
            width: Some(1000.0),
            ..Default::default()
        };
        view.follow(10.0, 10.5);
        assert_eq!(view.scroll_x, 0.0, "left of the middle: the playhead moves");
        view.follow(50.0, 50.5);
        assert_eq!(view.scroll_x, 5.0, "pinned at the middle (500 px)");
        view.follow(50.5, 51.0);
        assert_eq!(view.scroll_x, 10.0, "each tick scrolls by what it played");
        view.scroll_x = 3000.0;
        view.follow(51.0, 51.5);
        assert_eq!(view.scroll_x, 3000.0, "the user scrolled away");
    }

    #[test]
    fn fit_shows_a_long_timeline_whole_and_zooming_out_stops_there() {
        let mut view = TimelineViewport {
            width: Some(1024.0),
            ..Default::default()
        };
        let hour = 3600.0;
        view.zoom_to_fit(hour);
        let fit = view.px_per_sec;
        let span = hour as f32 * fit;
        assert!((span - 1000.0).abs() < 0.01, "an hour spans {span} px");
        // In and back out returns to the fit; there is nothing further out.
        view.zoom(1.5, 0.0, hour);
        assert!(view.px_per_sec > fit);
        view.zoom(0.1, 0.0, hour);
        assert_eq!(view.px_per_sec, fit);
        // A timeline that already fits stops at the usual floor.
        view.zoom(0.0001, 0.0, 60.0);
        assert_eq!(view.px_per_sec, MIN_ZOOM);
    }

    #[test]
    fn zoom_keeps_the_anchor_in_place_and_stays_in_range() {
        let mut view = TimelineViewport {
            px_per_sec: 50.0,
            scroll_x: 100.0,
            ..Default::default()
        };
        // 4 s sits at 200 px content, 100 px on screen.
        view.zoom(2.0, 4.0, 10.0);
        assert_eq!((view.px_per_sec, view.scroll_x), (100.0, 300.0));
        view.zoom(100.0, 4.0, 10.0);
        assert_eq!(view.px_per_sec, MAX_ZOOM);
        view.zoom(0.0001, 4.0, 10.0);
        assert_eq!(view.px_per_sec, MIN_ZOOM);
    }
}
