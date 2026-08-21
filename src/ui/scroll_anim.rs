//! Animated vertical scrolling for keyboard-driven list navigation.
//!
//! `ensure_row_visible` computes the target scroll offset; this module glides
//! the viewport's vadjustment there with an ease-out curve instead of GTK's
//! default instant snap — the macOS/Raycast feel. Rapid keypresses retarget
//! from the *current* offset so chained hops read as one continuous motion.
//! Query rebuilds pass `dir == 0` and snap instantly (content changed; there
//! is nothing meaningful to animate from).

use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib::ControlFlow;
use gtk::prelude::*;
use gtk::Adjustment;

/// Below this distance a glide is imperceptible — snap instead of animating.
const MIN_GLIDE_PX: f64 = 2.0;
/// One-row hop duration.
const GLIDE_MIN_MS: u64 = 180;
/// Full-page / wrap-around cap.
const GLIDE_MAX_MS: u64 = 300;

/// macOS-style decelerate: fast start, glide to a stop.
fn ease_out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// Duration scales with travel distance relative to the viewport page so a
/// one-row hop stays snappy while wrap-around still reads as one motion.
fn glide_duration_ms(dist: f64, page: f64) -> u64 {
    if dist < MIN_GLIDE_PX {
        return 0;
    }
    let frac = (dist / page.max(1.0)).clamp(0.0, 1.0);
    GLIDE_MIN_MS + ((GLIDE_MAX_MS - GLIDE_MIN_MS) as f64 * frac) as u64
}

fn clamp_scroll_target(target: f64, lower: f64, upper: f64, page: f64) -> f64 {
    target.clamp(lower, (upper - page).max(lower))
}

struct Glide {
    from: f64,
    target: f64,
    /// Frame-clock time of the first tick; `None` until the animation starts.
    start_us: Option<i64>,
    dur_ms: u64,
}

/// Shared tween state. Interior-mutable so the tick callback (which must be
/// `'static`) can drive it while `Launcher` holds an immutable handle.
#[derive(Default)]
struct State {
    glide: RefCell<Option<Glide>>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
}

#[derive(Clone, Default)]
pub struct ScrollTweener {
    state: Rc<State>,
}

impl ScrollTweener {
    pub fn new() -> Self {
        Self::default()
    }

    /// Animated scroll to `target`. If a glide is already running it
    /// retargets from the current offset — no restart jump between hops.
    pub fn glide<W: IsA<gtk::Widget>>(&self, widget: &W, adj: &Adjustment, target: f64) {
        let target = clamp_scroll_target(target, adj.lower(), adj.upper(), adj.page_size());
        let dist = (target - adj.value()).abs();
        if dist < MIN_GLIDE_PX {
            self.snap(adj, target);
            return;
        }
        let glide = Glide {
            from: adj.value(),
            target,
            start_us: None,
            dur_ms: glide_duration_ms(dist, adj.page_size()),
        };
        self.cancel();
        *self.state.glide.borrow_mut() = Some(glide);
        // Weak state reference: the widget owns the tick closure, so a strong
        // Rc here (state → tick id → closure → state) would leak the tween.
        let state = Rc::downgrade(&self.state);
        let adj = adj.clone();
        let id = widget.add_tick_callback(move |_, clock| {
            let Some(state) = state.upgrade() else {
                return ControlFlow::Break;
            };
            let mut cell = state.glide.borrow_mut();
            let Some(g) = cell.as_mut() else {
                return ControlFlow::Break;
            };
            let now = clock.frame_time();
            let start = *g.start_us.get_or_insert(now);
            let t = (now - start) as f64 / (g.dur_ms as f64 * 1000.0);
            if t >= 1.0 {
                adj.set_value(g.target);
                *cell = None;
                drop(cell);
                // Break makes GTK remove the callback; drop our stale id.
                *state.tick.borrow_mut() = None;
                return ControlFlow::Break;
            }
            let v = g.from + (g.target - g.from) * ease_out_cubic(t);
            adj.set_value(v);
            ControlFlow::Continue
        });
        *self.state.tick.borrow_mut() = Some(id);
    }

    /// Instant jump (query rebuilds, sub-pixel corrections). Cancels any
    /// in-flight glide.
    pub fn snap(&self, adj: &Adjustment, target: f64) {
        self.cancel();
        adj.set_value(clamp_scroll_target(
            target,
            adj.lower(),
            adj.upper(),
            adj.page_size(),
        ));
    }

    /// Stop mid-flight wherever the list currently is.
    pub fn cancel(&self) {
        if let Some(id) = self.state.tick.borrow_mut().take() {
            id.remove();
        }
        *self.state.glide.borrow_mut() = None;
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
            let t = i as f64 / 100.0;
            let e = ease_out_cubic(t);
            assert!(e > prev, "easing must be strictly increasing at t={t}");
            assert!((0.0..=1.0).contains(&e));
            prev = e;
        }
        // Overshoot input clamps, never overshoots output.
        assert_eq!(ease_out_cubic(-0.5), 0.0);
        assert_eq!(ease_out_cubic(1.5), 1.0);
    }

    #[test]
    fn glide_duration_scales_and_clamps() {
        // Sub-pixel: instant.
        assert_eq!(glide_duration_ms(1.0, 260.0), 0);
        // One-row hop (~50px on a 260px viewport): snappy, near the floor.
        let hop = glide_duration_ms(50.0, 260.0);
        assert!((200..=220).contains(&hop), "hop {hop}ms");
        // Full page and beyond: capped.
        assert_eq!(glide_duration_ms(260.0, 260.0), GLIDE_MAX_MS);
        assert_eq!(glide_duration_ms(10_000.0, 260.0), GLIDE_MAX_MS);
        // Degenerate page size must not divide by zero.
        assert_eq!(glide_duration_ms(50.0, 0.0), GLIDE_MAX_MS);
    }

    #[test]
    fn clamp_scroll_target_respects_scrollable_range() {
        // upper 1000, page 260 → max offset 740.
        assert_eq!(clamp_scroll_target(900.0, 0.0, 1000.0, 260.0), 740.0);
        assert_eq!(clamp_scroll_target(-50.0, 0.0, 1000.0, 260.0), 0.0);
        assert_eq!(clamp_scroll_target(300.0, 0.0, 1000.0, 260.0), 300.0);
        // Content shorter than the page: pinned to lower.
        assert_eq!(clamp_scroll_target(50.0, 0.0, 100.0, 260.0), 0.0);
    }
}
