//! Touch exploration ("screen reader mode") gesture state machine.
//!
//! This is pure logic with no I/O so that it can be unit tested. The caller
//! feeds it touch events together with the button (if any) under the finger
//! and gets back a list of things to do: announce a newly focused button,
//! press its keys or release them.
//!
//! Gestures, modelled on VoiceOver's handling of the Touch Bar:
//!
//! * Touch or slide a finger: the button under the finger gets focus and is
//!   announced. Nothing is activated.
//! * Double-tap: activates the focused button. The second tap may land
//!   anywhere on the bar. Keep the finger down on the second tap to hold the
//!   key (for example to ramp the volume).
//! * Split-tap: while one finger rests on a button, tap anywhere with a
//!   second finger to activate it. Repeated taps repeat the activation.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// A button, identified by (layer index, button index).
pub type Target = (usize, usize);

/// Longest a finger may stay down and still count as a tap.
const TAP_MAX: Duration = Duration::from_millis(300);
/// Longest pause between the two taps of a double-tap.
const DOUBLE_TAP_GAP: Duration = Duration::from_millis(400);
/// A finger that travels further than this (in pixels) is sliding, not tapping.
const TAP_SLOP_PX: f64 = 40.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// The button gained focus and should be announced.
    Focus(Target),
    /// The button's keys should be pressed.
    Press(Target),
    /// The button's keys should be released.
    Release(Target),
}

struct Finger {
    down_at: Instant,
    start_x: f64,
    moved: bool,
}

#[derive(Default)]
pub struct Explorer {
    focus: Option<Target>,
    /// The finger currently exploring the bar, if any.
    primary: Option<u32>,
    fingers: HashMap<u32, Finger>,
    /// When the last quick tap ended.
    last_tap: Option<Instant>,
    /// The finger holding a button down, and that button.
    pressed: Option<(u32, Target)>,
}

impl Explorer {
    pub fn focus(&self) -> Option<Target> {
        self.focus
    }

    pub fn touch_down(
        &mut self,
        slot: u32,
        now: Instant,
        x: f64,
        hit: Option<Target>,
    ) -> Vec<Output> {
        let mut out = Vec::new();
        self.fingers.insert(
            slot,
            Finger {
                down_at: now,
                start_x: x,
                moved: false,
            },
        );
        if self.pressed.is_some() {
            // A button is already held down; extra fingers do nothing.
            return out;
        }
        let double_tap = self
            .last_tap
            .take()
            .is_some_and(|t| now.saturating_duration_since(t) <= DOUBLE_TAP_GAP);
        // Split-tap (another finger is exploring) or the second tap of a
        // double-tap: activate the focused button wherever this tap landed.
        if self.primary.is_some() || double_tap {
            if let Some(target) = self.focus {
                self.pressed = Some((slot, target));
                out.push(Output::Press(target));
                return out;
            }
            if self.primary.is_some() {
                return out;
            }
        }
        self.primary = Some(slot);
        if let Some(target) = hit {
            // Announce even if focus did not change: the user is asking
            // where their finger is.
            self.focus = Some(target);
            out.push(Output::Focus(target));
        }
        out
    }

    pub fn touch_motion(&mut self, slot: u32, x: f64, hit: Option<Target>) -> Vec<Output> {
        let mut out = Vec::new();
        let Some(finger) = self.fingers.get_mut(&slot) else {
            return out;
        };
        if (x - finger.start_x).abs() > TAP_SLOP_PX {
            finger.moved = true;
        }
        if self.primary != Some(slot) || self.pressed.is_some() {
            return out;
        }
        if let Some(target) = hit {
            if self.focus != Some(target) {
                self.focus = Some(target);
                out.push(Output::Focus(target));
            }
        }
        out
    }

    pub fn touch_up(&mut self, slot: u32, now: Instant) -> Vec<Output> {
        let mut out = Vec::new();
        let Some(finger) = self.fingers.remove(&slot) else {
            return out;
        };
        let is_primary = self.primary == Some(slot);
        if let Some((holder, target)) = self.pressed {
            // Lifting either the activating finger or the exploring finger
            // ends the activation, so a key can never be left stuck down.
            if holder == slot || is_primary {
                self.pressed = None;
                out.push(Output::Release(target));
            }
        }
        if is_primary {
            self.primary = None;
            let quick = now.saturating_duration_since(finger.down_at) <= TAP_MAX;
            self.last_tap = (quick && !finger.moved).then_some(now);
        }
        out
    }

    /// The buttons on the bar changed under the user's fingers. Releases any
    /// held button and drops focus, but keeps tracking the fingers so the
    /// next movement announces what is under them now.
    pub fn layer_changed(&mut self) -> Vec<Output> {
        self.focus = None;
        self.last_tap = None;
        self.pressed
            .take()
            .map(|(_, target)| Output::Release(target))
            .into_iter()
            .collect()
    }

    /// Forget everything, releasing any held button. Call when exploration
    /// is switched off.
    pub fn reset(&mut self) -> Vec<Output> {
        let out = self
            .pressed
            .take()
            .map(|(_, target)| Output::Release(target))
            .into_iter()
            .collect();
        *self = Explorer::default();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Target = (0, 1);
    const B: Target = (0, 2);

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    #[test]
    fn touching_announces_but_does_not_press() {
        let t = Instant::now();
        let mut e = Explorer::default();
        assert_eq!(e.touch_down(0, t, 100.0, Some(A)), vec![Output::Focus(A)]);
        assert_eq!(e.touch_up(0, ms(t, 1000)), vec![]);
    }

    #[test]
    fn sliding_announces_each_new_button_once() {
        let t = Instant::now();
        let mut e = Explorer::default();
        e.touch_down(0, t, 100.0, Some(A));
        assert_eq!(e.touch_motion(0, 110.0, Some(A)), vec![]);
        assert_eq!(e.touch_motion(0, 300.0, None), vec![]);
        assert_eq!(e.touch_motion(0, 320.0, Some(B)), vec![Output::Focus(B)]);
        assert_eq!(e.touch_motion(0, 330.0, Some(B)), vec![]);
        assert_eq!(e.focus(), Some(B));
    }

    #[test]
    fn double_tap_activates_and_holds_until_release() {
        let t = Instant::now();
        let mut e = Explorer::default();
        e.touch_down(0, t, 100.0, Some(A));
        e.touch_up(0, ms(t, 80));
        assert_eq!(
            e.touch_down(0, ms(t, 200), 100.0, Some(A)),
            vec![Output::Press(A)]
        );
        // Still held two seconds later.
        assert_eq!(e.touch_up(0, ms(t, 2200)), vec![Output::Release(A)]);
    }

    #[test]
    fn double_tap_elsewhere_activates_explored_button() {
        let t = Instant::now();
        let mut e = Explorer::default();
        // Explore slowly to A, lift.
        e.touch_down(0, t, 100.0, Some(A));
        e.touch_up(0, ms(t, 2000));
        // Double-tap in a gap between buttons.
        assert_eq!(e.touch_down(0, ms(t, 3000), 500.0, None), vec![]);
        e.touch_up(0, ms(t, 3050));
        assert_eq!(
            e.touch_down(0, ms(t, 3150), 500.0, None),
            vec![Output::Press(A)]
        );
        assert_eq!(e.touch_up(0, ms(t, 3200)), vec![Output::Release(A)]);
    }

    #[test]
    fn slow_second_tap_is_not_a_double_tap() {
        let t = Instant::now();
        let mut e = Explorer::default();
        e.touch_down(0, t, 100.0, Some(A));
        e.touch_up(0, ms(t, 80));
        assert_eq!(
            e.touch_down(0, ms(t, 1000), 100.0, Some(A)),
            vec![Output::Focus(A)]
        );
    }

    #[test]
    fn long_or_sliding_touch_does_not_arm_double_tap() {
        let t = Instant::now();
        let mut e = Explorer::default();
        e.touch_down(0, t, 100.0, Some(A));
        e.touch_up(0, ms(t, 600));
        assert_eq!(
            e.touch_down(0, ms(t, 700), 100.0, Some(A)),
            vec![Output::Focus(A)]
        );
        e.touch_up(0, ms(t, 1700));

        e.touch_down(0, ms(t, 2000), 100.0, Some(A));
        e.touch_motion(0, 400.0, Some(B));
        e.touch_up(0, ms(t, 2100));
        assert_eq!(
            e.touch_down(0, ms(t, 2200), 400.0, Some(B)),
            vec![Output::Focus(B)]
        );
    }

    #[test]
    fn third_tap_does_not_activate_again() {
        let t = Instant::now();
        let mut e = Explorer::default();
        e.touch_down(0, t, 100.0, Some(A));
        e.touch_up(0, ms(t, 50));
        e.touch_down(0, ms(t, 150), 100.0, Some(A));
        e.touch_up(0, ms(t, 200));
        assert_eq!(
            e.touch_down(0, ms(t, 300), 100.0, Some(A)),
            vec![Output::Focus(A)]
        );
    }

    #[test]
    fn split_tap_activates_and_repeats() {
        let t = Instant::now();
        let mut e = Explorer::default();
        e.touch_down(0, t, 100.0, Some(A));
        for i in 0..3 {
            let base = 1000 + i * 300;
            assert_eq!(
                e.touch_down(1, ms(t, base), 900.0, Some(B)),
                vec![Output::Press(A)]
            );
            assert_eq!(e.touch_up(1, ms(t, base + 60)), vec![Output::Release(A)]);
        }
        assert_eq!(e.focus(), Some(A));
        assert_eq!(e.touch_up(0, ms(t, 5000)), vec![]);
    }

    #[test]
    fn lifting_exploring_finger_releases_split_tap() {
        let t = Instant::now();
        let mut e = Explorer::default();
        e.touch_down(0, t, 100.0, Some(A));
        e.touch_down(1, ms(t, 500), 900.0, None);
        assert_eq!(e.touch_up(0, ms(t, 600)), vec![Output::Release(A)]);
        assert_eq!(e.touch_up(1, ms(t, 700)), vec![]);
    }

    #[test]
    fn extra_fingers_are_ignored_while_pressed() {
        let t = Instant::now();
        let mut e = Explorer::default();
        e.touch_down(0, t, 100.0, Some(A));
        e.touch_down(1, ms(t, 500), 900.0, None);
        assert_eq!(e.touch_down(2, ms(t, 550), 500.0, Some(B)), vec![]);
        assert_eq!(e.touch_up(2, ms(t, 600)), vec![]);
        assert_eq!(e.touch_up(1, ms(t, 650)), vec![Output::Release(A)]);
    }

    #[test]
    fn nothing_to_activate_without_focus() {
        let t = Instant::now();
        let mut e = Explorer::default();
        e.touch_down(0, t, 500.0, None);
        e.touch_up(0, ms(t, 50));
        assert_eq!(e.touch_down(0, ms(t, 150), 500.0, None), vec![]);
        assert_eq!(e.touch_up(0, ms(t, 200)), vec![]);
    }

    #[test]
    fn reset_releases_held_button() {
        let t = Instant::now();
        let mut e = Explorer::default();
        e.touch_down(0, t, 100.0, Some(A));
        e.touch_down(1, ms(t, 500), 900.0, None);
        assert_eq!(e.reset(), vec![Output::Release(A)]);
        assert_eq!(e.focus(), None);
        assert_eq!(e.touch_up(0, ms(t, 600)), vec![]);
        assert_eq!(e.touch_up(1, ms(t, 600)), vec![]);
    }

    #[test]
    fn layer_change_releases_and_reannounces() {
        let t = Instant::now();
        let mut e = Explorer::default();
        e.touch_down(0, t, 100.0, Some(A));
        e.touch_down(1, ms(t, 500), 900.0, None);
        assert_eq!(e.layer_changed(), vec![Output::Release(A)]);
        assert_eq!(e.focus(), None);
        // Same finger, same place, new layer: announced again.
        let c: Target = (1, 1);
        assert_eq!(e.touch_motion(0, 101.0, Some(c)), vec![Output::Focus(c)]);
        assert_eq!(e.touch_up(1, ms(t, 600)), vec![]);
    }

    #[test]
    fn unknown_slots_are_harmless() {
        let t = Instant::now();
        let mut e = Explorer::default();
        assert_eq!(e.touch_motion(7, 10.0, Some(A)), vec![]);
        assert_eq!(e.touch_up(7, t), vec![]);
    }
}
