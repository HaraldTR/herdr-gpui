//! Two clean modifier taps, without delaying or consuming ordinary input.

use super::HerdrWindow;
use gpui::{Capslock, Context, Modifiers, ModifiersChangedEvent, Window};
use std::time::{Duration, Instant};

const TAP_DURATION: Duration = Duration::from_millis(250);
const DOUBLE_TAP: Duration = Duration::from_millis(400);

#[derive(Default)]
pub(crate) struct ShiftTaps {
    modifiers: Modifiers,
    capslock: Capslock,
    pressed: Option<Instant>,
    first: Option<Instant>,
}

impl ShiftTaps {
    pub(crate) fn cancel(&mut self) {
        self.pressed = None;
        self.first = None;
    }

    fn changed(&mut self, event: &ModifiersChangedEvent, eligible: bool, now: Instant) -> bool {
        let previous = self.modifiers;
        self.modifiers = event.modifiers;
        let caps_changed = self.capslock != event.capslock;
        self.capslock = event.capslock;
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        if !eligible
            || caps_changed
            || (event.modifiers != shift && event.modifiers != Modifiers::default())
        {
            self.cancel();
            return false;
        }
        if previous == event.modifiers {
            return false;
        }
        if event.modifiers == shift && previous == Modifiers::default() {
            self.pressed = Some(now);
            return false;
        }
        if previous != shift {
            self.cancel();
            return false;
        }
        let Some(pressed) = self.pressed.take() else {
            return false;
        };
        if now.saturating_duration_since(pressed) > TAP_DURATION {
            self.cancel();
            return false;
        }
        if self
            .first
            .take()
            .is_some_and(|first| now.saturating_duration_since(first) <= DOUBLE_TAP)
        {
            return true;
        }
        self.first = Some(now);
        false
    }
}

impl HerdrWindow {
    pub(super) fn double_shift_modifiers(
        &mut self,
        event: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let eligible = self.config.palette.double_shift
            && self.active
            && self.menu.page.is_none()
            && self.marked.is_empty()
            && !self.prefix_armed
            && !cx.has_active_drag()
            && self.terminal_mouse.is_none()
            && self.selection.is_none()
            && self.workspace_drag.is_none()
            && self.tab_drag.is_none()
            && self.sidebar_drag.is_none()
            && self.split_drag.is_none()
            && self.scrollbar_drag.is_none();
        if self.shift_taps.changed(event, eligible, Instant::now()) {
            self.open_palette(crate::palette::Filter::All, window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(taps: &mut ShiftTaps, shift: bool, millis: u64, now: Instant) -> bool {
        taps.changed(
            &ModifiersChangedEvent {
                modifiers: Modifiers {
                    shift,
                    ..Modifiers::default()
                },
                ..Default::default()
            },
            true,
            now + Duration::from_millis(millis),
        )
    }

    #[test]
    fn recognizes_two_completed_taps_and_ignores_duplicate_events() {
        let now = Instant::now();
        let mut taps = ShiftTaps::default();
        assert!(!change(&mut taps, true, 0, now));
        assert!(!change(&mut taps, true, 10, now));
        assert!(!change(&mut taps, false, 40, now));
        assert!(!change(&mut taps, false, 50, now));
        assert!(!change(&mut taps, true, 100, now));
        assert!(change(&mut taps, false, 140, now));
        assert!(!change(&mut taps, true, 160, now));
        assert!(!change(&mut taps, false, 200, now));
    }

    #[test]
    fn rejects_held_keys_slow_taps_and_intervening_input() {
        let now = Instant::now();
        for scenario in 0..4 {
            let mut taps = ShiftTaps::default();
            change(&mut taps, true, 0, now);
            change(&mut taps, false, if scenario == 0 { 300 } else { 40 }, now);
            match scenario {
                2 => taps.cancel(),
                3 => {
                    taps.changed(
                        &ModifiersChangedEvent {
                            modifiers: Modifiers {
                                control: true,
                                ..Modifiers::default()
                            },
                            ..Default::default()
                        },
                        true,
                        now + Duration::from_millis(60),
                    );
                    change(&mut taps, false, 70, now);
                }
                _ => {}
            }
            change(&mut taps, true, if scenario == 1 { 500 } else { 320 }, now);
            assert!(!change(
                &mut taps,
                false,
                if scenario == 1 { 540 } else { 360 },
                now
            ));
        }
    }

    #[test]
    fn blocked_focus_or_composition_cannot_leave_an_armed_tap() {
        let now = Instant::now();
        let mut taps = ShiftTaps::default();
        change(&mut taps, true, 0, now);
        change(&mut taps, false, 40, now);
        taps.changed(
            &ModifiersChangedEvent::default(),
            false,
            now + Duration::from_millis(50),
        );
        change(&mut taps, true, 100, now);
        assert!(!change(&mut taps, false, 140, now));
    }

    #[gpui::test]
    fn modifier_events_open_one_palette_and_shift_typing_does_not(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        view.update(cx, |view, _| view.active = true);
        cx.update(|window, cx| {
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
            window.draw(cx).clear(cx);
        });
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        for modifiers in [shift, Modifiers::default(), shift, Modifiers::default()] {
            cx.simulate_event(ModifiersChangedEvent {
                modifiers,
                ..Default::default()
            });
        }
        view.read_with(cx, |view, _| {
            assert!(view.menu.palette.is_some());
        });
        let token = view.read_with(cx, |view, _| {
            view.menu
                .palette
                .as_ref()
                .map(|palette| palette.search.clone())
        });
        for modifiers in [shift, Modifiers::default(), shift, Modifiers::default()] {
            cx.simulate_event(ModifiersChangedEvent {
                modifiers,
                ..Default::default()
            });
        }
        view.read_with(cx, |view, _| {
            assert_eq!(
                token,
                view.menu
                    .palette
                    .as_ref()
                    .map(|palette| palette.search.clone())
            )
        });
        cx.update(|window, cx| view.update(cx, |view, cx| view.dismiss_menu(window, cx)));
        cx.simulate_event(ModifiersChangedEvent {
            modifiers: shift,
            ..Default::default()
        });
        cx.simulate_keystrokes("shift-a");
        for modifiers in [Modifiers::default(), shift, Modifiers::default()] {
            cx.simulate_event(ModifiersChangedEvent {
                modifiers,
                ..Default::default()
            });
        }
        view.read_with(cx, |view, _| assert!(view.menu.palette.is_none()));
    }

    #[gpui::test]
    fn a_wheel_event_consumed_by_a_toast_still_cancels_shift_taps(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.show_toast_preview(
                    herdr_client::protocol::SemanticNotificationKind::Custom,
                    cx,
                )
            });
            window.draw(cx).clear(cx);
        });
        let Some(bounds) = cx.debug_bounds("toast-local-0") else {
            panic!("toast preview must be rendered");
        };
        let now = Instant::now();
        view.update(cx, |view, _| {
            change(&mut view.shift_taps, true, 0, now);
            change(&mut view.shift_taps, false, 40, now);
            assert!(view.shift_taps.first.is_some());
        });
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: bounds.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-20.))),
            ..Default::default()
        });
        view.read_with(cx, |view, _| assert!(view.shift_taps.first.is_none()));
    }
}
