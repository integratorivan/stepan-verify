//! Pan and zoom of the change canvas (Figma-style): world coordinates map to
//! the screen as `screen = pan + world * zoom`.

pub const MIN_ZOOM: f32 = 0.15;
pub const MAX_ZOOM: f32 = 2.5;
/// Space kept around the content when fitting.
pub const FIT_MARGIN: f32 = 36.;
/// Top of the content after fitting: below the hint line.
pub const FIT_TOP: f32 = 48.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

impl Default for View {
    fn default() -> Self {
        View { x: FIT_MARGIN, y: FIT_MARGIN, zoom: 1. }
    }
}

impl View {
    /// The whole world in view: centered across, from the top down (under
    /// the hint line), never magnified past 100%.
    pub fn fit(world: (f32, f32), view: (f32, f32)) -> View {
        let (ww, wh) = (world.0.max(1.), world.1.max(1.));
        let (vw, vh) = ((view.0 - FIT_MARGIN * 2.).max(1.), (view.1 - FIT_TOP - FIT_MARGIN).max(1.));
        let zoom = (vw / ww).min(vh / wh).clamp(MIN_ZOOM, 1.);
        View { x: (view.0 - ww * zoom) / 2., y: FIT_TOP, zoom }
    }

    /// The view to open on: 100%, readable. When the whole world fits it is
    /// centered; otherwise `focus` (the main frame's top left) sits at the top left.
    pub fn readable(world: (f32, f32), focus: Option<(f32, f32)>, view: (f32, f32)) -> View {
        let fits = world.0 + FIT_MARGIN * 2. <= view.0 && world.1 + FIT_TOP + FIT_MARGIN <= view.1;
        if fits {
            return View { x: (view.0 - world.0) / 2., y: FIT_TOP, zoom: 1. };
        }
        let (fx, fy) = focus.unwrap_or((0., 0.));
        View { x: FIT_MARGIN - fx, y: FIT_TOP - fy, zoom: 1. }
    }

    /// Zoom by `factor` keeping the world point under `at` (screen, relative to the viewport) still.
    pub fn zoom_at(self, factor: f32, at: (f32, f32)) -> View {
        let zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let k = zoom / self.zoom;
        View { x: at.0 - (at.0 - self.x) * k, y: at.1 - (at.1 - self.y) * k, zoom }
    }

    pub fn pan(self, dx: f32, dy: f32) -> View {
        View { x: self.x + dx, y: self.y + dy, ..self }
    }

    #[cfg(test)]
    pub fn screen(self, p: (f32, f32)) -> (f32, f32) {
        (self.x + p.0 * self.zoom, self.y + p.1 * self.zoom)
    }

    #[cfg(test)]
    pub fn world(self, s: (f32, f32)) -> (f32, f32) {
        ((s.0 - self.x) / self.zoom, (s.1 - self.y) / self.zoom)
    }
}

/// A drag in progress: where it started and the view at that moment.
#[derive(Clone, Copy, Debug)]
pub struct Drag {
    pub origin: (f32, f32),
    pub start: View,
    /// Moved past the click threshold: it pans, it is not a click.
    pub moved: bool,
}

/// Movement under this many pixels is still a click.
pub const CLICK_SLOP: f32 = 3.;

impl Drag {
    pub fn new(origin: (f32, f32), start: View) -> Self {
        Drag { origin, start, moved: false }
    }

    /// The view for the pointer at `to`; marks the drag as moved past the slop.
    pub fn update(&mut self, to: (f32, f32)) -> View {
        let (dx, dy) = (to.0 - self.origin.0, to.1 - self.origin.1);
        if !self.moved && dx.hypot(dy) > CLICK_SLOP {
            self.moved = true;
        }
        if self.moved { self.start.pan(dx, dy) } else { self.start }
    }
}

/// Zoom factor for a ⌘/Ctrl + wheel step of `dy` pixels (negative: zoom in).
pub fn wheel_zoom(dy: f32) -> f32 {
    (-dy * 0.0035).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[core::prelude::v1::test]
    fn opens_readable_at_100_on_the_main_frame() {
        // Everything fits: centered at 100%.
        let v = View::readable((800., 300.), Some((0., 0.)), (1200., 800.));
        assert!(close(v.zoom, 1.) && close(v.x, 200.) && close(v.y, FIT_TOP));
        // Too big: 100% with the main frame at the top left, not shrunk to fit.
        let v = View::readable((3000., 2000.), Some((400., 600.)), (1200., 800.));
        assert!(close(v.zoom, 1.));
        assert_eq!(v.screen((400., 600.)), (FIT_MARGIN, FIT_TOP));
    }

    #[core::prelude::v1::test]
    fn fit_centers_and_never_magnifies() {
        let v = View::fit((1000., 500.), (1200., 800.));
        assert!(close(v.zoom, 1.));
        assert!(close(v.x, 100.) && close(v.y, FIT_TOP));
        let v = View::fit((4000., 1000.), (1200., 800.));
        assert!(close(v.zoom, (1200. - 2. * FIT_MARGIN) / 4000.));
        let (l, _) = v.screen((0., 0.));
        let (r, _) = v.screen((4000., 0.));
        assert!(close(l, 1200. - r), "centered horizontally");
        // Huge worlds stop at the minimum zoom.
        assert!(close(View::fit((1e6, 10.), (800., 600.)).zoom, MIN_ZOOM));
    }

    #[core::prelude::v1::test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let v = View { x: 40., y: -20., zoom: 0.8 };
        let at = (300., 200.);
        let before = v.world(at);
        let z = v.zoom_at(1.5, at);
        assert!(close(z.zoom, 1.2));
        let after = z.world(at);
        assert!(close(before.0, after.0) && close(before.1, after.1));
        // Clamped at both ends.
        assert!(close(v.zoom_at(100., at).zoom, MAX_ZOOM) && close(v.zoom_at(0.0001, at).zoom, MIN_ZOOM));
        assert!(wheel_zoom(-50.) > 1. && wheel_zoom(50.) < 1.);
    }

    #[core::prelude::v1::test]
    fn drag_pans_only_past_the_slop() {
        let start = View { x: 10., y: 10., zoom: 1. };
        let mut d = Drag::new((100., 100.), start);
        assert_eq!(d.update((101., 102.)), start);
        assert!(!d.moved);
        let v = d.update((150., 80.));
        assert!(d.moved && close(v.x, 60.) && close(v.y, -10.));
        // Back near the origin it still pans (a drag stays a drag).
        let v = d.update((101., 100.));
        assert!(close(v.x, 11.));
    }
}
