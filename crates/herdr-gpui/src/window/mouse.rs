//! Application mouse gestures take precedence over local selection and links.
//! Shift keeps a gesture local; a forwarded drag stays with its pressed target.

use super::HerdrWindow;
use crate::{
    connection::ConnectionBridge,
    navigation::NavigationTarget,
    terminal::{InputTarget, WheelTarget, wheel_target},
};
use gpui::{
    Context, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    Window,
};
use herdr_client::protocol::{ClientMouseButton, ClientMouseKind};

pub(crate) struct Gesture {
    hit: WheelTarget,
    button: MouseButton,
    boot: String,
    epoch: u64,
    generation: u64,
}

fn button(button: MouseButton) -> Option<ClientMouseButton> {
    match button {
        MouseButton::Left => Some(ClientMouseButton::Left),
        MouseButton::Right => Some(ClientMouseButton::Right),
        MouseButton::Middle => Some(ClientMouseButton::Middle),
        _ => None,
    }
}

impl HerdrWindow {
    pub(crate) fn mouse_focus_pending(&self) -> bool {
        self.terminal_mouse.as_ref().is_some_and(|gesture| {
            matches!(&gesture.hit.target, InputTarget::Pane(id)
                if self.live.snapshot.as_ref().and_then(|snapshot| snapshot.focused_pane_id.as_ref()) != Some(id))
        })
    }

    /// Retire only an input already sent to this connection. This is cleanup,
    /// not new input: a menu or resize must not leave the application dragging.
    pub(crate) fn cancel_terminal_mouse(&mut self, cx: &mut Context<Self>) {
        let Some(gesture) = self.terminal_mouse.take() else {
            return;
        };
        if gesture.epoch != self.selection_epoch
            || gesture.generation != self.selected_generation
            || self
                .live
                .snapshot
                .as_ref()
                .is_none_or(|snapshot| snapshot.boot_id != gesture.boot)
        {
            return;
        }
        if let Some(handle) = &self.endpoints[self.selected_endpoint].connection.handle
            && let Some(button) = button(gesture.button)
            && let Err(error) = ConnectionBridge::send_input(
                handle,
                &gesture.boot,
                &gesture.hit.target,
                gesture
                    .hit
                    .mouse_event(ClientMouseKind::Up(button), Modifiers::default()),
            )
        {
            self.local_error = Some(format!("Mouse release not sent: {error}"));
            cx.notify();
        }
    }

    pub(crate) fn terminal_mouse_at(&self, position: Point<Pixels>) -> Option<WheelTarget> {
        if self.menu.page.is_some()
            || !self.live.surface_ready()
            || !self.bounds.contains(&position)
        {
            return None;
        }
        wheel_target(
            self.live.surface.as_deref()?,
            f32::from(position.x - self.bounds.origin.x),
            f32::from(position.y - self.bounds.origin.y),
            self.cell_width,
            self.config.terminal.line_height(),
        )
    }

    fn send_mouse(
        &mut self,
        hit: &WheelTarget,
        kind: ClientMouseKind,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.menu.page.is_some() || !self.input_ready() {
            return false;
        }
        let (Some(handle), Some(snapshot)) = (
            &self.endpoints[self.selected_endpoint].connection.handle,
            &self.live.snapshot,
        ) else {
            return false;
        };
        match ConnectionBridge::send_input(
            handle,
            &snapshot.boot_id,
            &hit.target,
            hit.mouse_event(kind, modifiers),
        ) {
            Ok(()) => true,
            Err(error) => {
                self.local_error = Some(format!("Mouse input not sent: {error}"));
                cx.notify();
                false
            }
        }
    }

    /// Returns ownership, not queue success: an unavailable application must not
    /// turn a click into an unexpected clipboard write or browser launch.
    pub(crate) fn terminal_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.cancel_terminal_mouse(cx);
        let Some(hit) = self
            .terminal_mouse_at(event.position)
            .filter(|hit| hit.mouse_reporting && !event.modifiers.shift)
        else {
            return false;
        };
        self.pressed_terminal_link = None;
        self.selection = None;
        window.focus(&self.focus);
        cx.stop_propagation();
        if let Some(button) = button(event.button)
            && !cx.has_active_drag()
            && self.send_mouse(&hit, ClientMouseKind::Down(button), event.modifiers, cx)
            && let Some(snapshot) = &self.live.snapshot
        {
            self.terminal_mouse = Some(Gesture {
                hit,
                button: event.button,
                boot: snapshot.boot_id.clone(),
                epoch: self.selection_epoch,
                generation: self.selected_generation,
            });
        }
        true
    }

    fn gesture_hit(&self, gesture: &Gesture, position: Point<Pixels>) -> Option<WheelTarget> {
        if self.menu.page.is_some()
            || !self.input_ready()
            || gesture.epoch != self.selection_epoch
            || gesture.generation != self.selected_generation
            || self.live.snapshot.as_ref()?.boot_id != gesture.boot
        {
            return None;
        }
        let bounds = gesture.hit.bounds;
        let local = position - self.bounds.origin;
        let x = f32::from(local.x).clamp(
            f32::from(bounds.left()),
            f32::from(bounds.right()).next_down(),
        );
        let y = f32::from(local.y).clamp(
            f32::from(bounds.top()),
            f32::from(bounds.bottom()).next_down(),
        );
        let hit = wheel_target(
            self.live.surface.as_deref()?,
            x,
            y,
            self.cell_width,
            self.config.terminal.line_height(),
        )?;
        (hit.target == gesture.hit.target && hit.bounds == bounds && hit.mouse_reporting)
            .then_some(hit)
    }

    pub(crate) fn terminal_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        if cx.has_active_drag() {
            self.cancel_terminal_mouse(cx);
            self.selection = None;
            self.pressed_terminal_link = None;
            return false;
        }
        if let Some(gesture) = &self.terminal_mouse {
            if event.pressed_button != Some(gesture.button) {
                self.cancel_terminal_mouse(cx);
                return false;
            }
            let hit = self.gesture_hit(gesture, event.position);
            let kind = button(gesture.button).map(ClientMouseKind::Drag);
            if let (Some(hit), Some(kind)) = (hit, kind) {
                if self.send_mouse(&hit, kind, event.modifiers, cx)
                    && let Some(gesture) = &mut self.terminal_mouse
                {
                    gesture.hit = hit;
                }
            } else {
                self.cancel_terminal_mouse(cx);
            }
            return true;
        }
        false
    }

    pub(crate) fn terminal_mouse_hover(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        // Hover belongs to the hit-tested element, unlike an owned drag.
        if event.pressed_button.is_none()
            && !cx.has_active_drag()
            && !event.modifiers.shift
            && let Some(hit) = self
                .terminal_mouse_at(event.position)
                .filter(|hit| hit.mouse_reporting)
        {
            self.send_mouse(&hit, ClientMouseKind::Moved, event.modifiers, cx);
        }
    }

    pub(crate) fn terminal_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        if cx.has_active_drag() {
            self.cancel_terminal_mouse(cx);
            self.selection = None;
            self.pressed_terminal_link = None;
            return false;
        }
        if self
            .terminal_mouse
            .as_ref()
            .is_none_or(|gesture| gesture.button != event.button)
        {
            return false;
        }
        let hit = self
            .terminal_mouse
            .as_ref()
            .and_then(|gesture| self.gesture_hit(gesture, event.position));
        if hit.is_none() {
            self.cancel_terminal_mouse(cx);
            return true;
        }
        self.terminal_mouse = None;
        if let Some(hit) = hit
            && let Some(button) = button(event.button)
            && self.send_mouse(&hit, ClientMouseKind::Up(button), event.modifiers, cx)
            && let InputTarget::Pane(id) = &hit.target
        {
            // Finish targeted input before navigation fences the surface. This
            // also lets the first click in an inactive split reach its app.
            self.focus_clicked_pane(id, cx);
        }
        true
    }

    pub(crate) fn focus_clicked_pane(&mut self, id: &str, cx: &mut Context<Self>) {
        if self
            .live
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.focused_pane_id.as_deref())
            != Some(id)
        {
            self.navigate(NavigationTarget::Pane(id), cx);
        }
    }
}
