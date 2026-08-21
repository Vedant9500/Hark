//! App-side window resize animation.
//!
//! Compositor layer animation is disabled (`no_anim` layerrule) because box
//! interpolation ghosts on surface resize (docs/hyprland-layer-corners.md),
//! so compact↔expanded size changes would snap. This tweener restores the
//! motion app-side: it steps the window+shell size requests across frames
//! and the compositor applies each intermediate commit instantly — the box
//! always matches its buffer, so nothing can lag behind.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib::ControlFlow;
use gtk::prelude::*;
use gtk::{ApplicationWindow, Box as GtkBox};

/// Below this height delta a glide is imperceptible — snap instead.
const RESIZE_MIN_PX: f64 = 2.0;
/// Compact↔expanded hop duration floor.
const RESIZE_MIN_MS: u64 = 160;
/// Cap for the largest size change.
const RESIZE_MAX_MS: u64 = 240;

fn ease_out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// Duration scales with travel distance relative to the taller endpoint.
fn resize_duration_ms(dist: f64, span: f64) -> u64 {
    if dist < RESIZE_MIN_PX {
        return 0;
    }
    let frac = (dist / span.max(1.0)).clamp(0.0, 1.0);
    RESIZE_MIN_MS + ((RESIZE_MAX_MS - RESIZE_MIN_MS) as f64 * frac) as u64
}

struct Tween {
    from: (i32, i32),
    target: (i32, i32),
    /// Frame-clock time of the first tick; `None` until the animation starts.
    start_us: Option<i64>,
    dur_ms: u64,
}

#[derive(Default)]
struct State {
    tween: RefCell<Option<Tween>>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
}

/// Interior-mutable so the tick callback (which must be `'static`) can drive
/// it while call sites hold immutable handles.
#[derive(Clone, Default)]
pub struct SizeTweener {
    state: Rc<State>,
}

impl SizeTweener {
    pub fn new() -> Self {
        Self::default()
    }

    fn apply(win: &ApplicationWindow, shell: &GtkBox, w: i32, h: i32) {
        win.set_size_request(w, h);
        shell.set_size_request(w, h);
    }

    /// Animated resize to `(w, h)`. Retargets from the current request when a
    /// tween is already running; snaps while hidden (`show()` lays out at the
    /// final size before mapping) and for sub-pixel deltas.
    pub fn glide(&self, win: &ApplicationWindow, shell: &GtkBox, w: i32, h: i32) {
        let from = (shell.width_request(), shell.height_request());
        let dist = (h - from.1).abs() as f64;
        if !win.is_visible() || dist < RESIZE_MIN_PX {
            self.snap(win, shell, w, h);
            return;
        }
        self.cancel();
        *self.state.tween.borrow_mut() = Some(Tween {
            from,
            target: (w, h),
            start_us: None,
            dur_ms: resize_duration_ms(dist, from.1.max(h) as f64),
        });
        // Weak state reference: the widget owns the tick closure, so a strong
        // Rc here (state → tick id → closure → state) would leak the tween.
        let state = Rc::downgrade(&self.state);
        let win_t = win.clone();
        let shell_t = shell.clone();
        let id = win.add_tick_callback(move |_, clock| {
            let Some(state) = state.upgrade() else {
                return ControlFlow::Break;
            };
            let mut cell = state.tween.borrow_mut();
            let Some(t) = cell.as_mut() else {
                return ControlFlow::Break;
            };
            let now = clock.frame_time();
            let start = *t.start_us.get_or_insert(now);
            let p = (now - start) as f64 / (t.dur_ms as f64 * 1000.0);
            if p >= 1.0 {
                let (tw, th) = t.target;
                drop(cell);
                Self::apply(&win_t, &shell_t, tw, th);
                *state.tick.borrow_mut() = None;
                return ControlFlow::Break;
            }
            let e = ease_out_cubic(p);
            let (from, target) = (t.from, t.target);
            drop(cell);
            let step = |f: i32, t: i32| (f as f64 + (t - f) as f64 * e).round() as i32;
            Self::apply(
                &win_t,
                &shell_t,
                step(from.0, target.0),
                step(from.1, target.1),
            );
            ControlFlow::Continue
        });
        *self.state.tick.borrow_mut() = Some(id);
    }

    /// Instant jump (hidden layout, sub-pixel deltas). Cancels any tween.
    pub fn snap(&self, win: &ApplicationWindow, shell: &GtkBox, w: i32, h: i32) {
        self.cancel();
        Self::apply(win, shell, w, h);
    }

    /// Stop mid-flight wherever the window currently is.
    pub fn cancel(&self) {
        if let Some(id) = self.state.tick.borrow_mut().take() {
            id.remove();
        }
        *self.state.tween.borrow_mut() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_out_cubic_endpoints_and_monotonic() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        let mut prev = -1.0;
        for i in 0..=100 {
            let e = ease_out_cubic(i as f64 / 100.0);
            assert!(e > prev, "easing must be strictly increasing");
            assert!((0.0..=1.0).contains(&e));
            prev = e;
        }
    }

    #[test]
    fn resize_duration_scales_and_clamps() {
        // Sub-pixel: instant.
        assert_eq!(resize_duration_ms(1.0, 480.0), 0);
        // Full compact↔expanded hop (110 → 480): near the cap.
        let hop = resize_duration_ms(370.0, 480.0);
        assert!((220..=240).contains(&hop), "hop {hop}ms");
        assert_eq!(resize_duration_ms(10_000.0, 480.0), RESIZE_MAX_MS);
        // Degenerate span must not divide by zero.
        assert_eq!(resize_duration_ms(50.0, 0.0), RESIZE_MAX_MS);
    }
}
