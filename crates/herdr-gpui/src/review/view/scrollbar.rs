//! The diff's scrollbar: a thumb along the list's right edge that shows where
//! the view sits and how much of the diff it holds, and drags it. Its size
//! and travel are the tab strip's thumb math, turned on its side.
use crate::{HerdrWindow, browser::Thumb};
use gpui::{prelude::*, *};

/// How wide the thumb draws, and its margin from the list's edge.
const WIDTH: f32 = 8.;
const INSET: f32 = 2.;

/// How opaque the thumb draws over the muted color, then hovered or held.
const ALPHA: u32 = 0x60;
const HOVER_ALPHA: u32 = 0x90;
const HELD_ALPHA: u32 = 0xc0;

/// What the diff's thumb drags.
#[derive(Clone, Copy, Debug)]
struct DiffThumb;

impl HerdrWindow {
    /// The thumb as the list last laid out; none while it all fits.
    fn review_thumb(&self) -> Option<(Thumb, Bounds<Pixels>, f32)> {
        let review = self.menu.review.as_ref()?;
        let handle = review.scroll.0.borrow().base_handle.clone();
        let bounds = handle.bounds();
        let max = f32::from(handle.max_offset().y);
        let thumb = Thumb::of(
            f32::from(bounds.size.height),
            max,
            -f32::from(handle.offset().y),
        )?;
        Some((thumb, bounds, max))
    }

    pub(super) fn grab_review_thumb(&mut self, y: Pixels) {
        let Some((thumb, bounds, _)) = self.review_thumb() else {
            return;
        };
        if let Some(review) = self.menu.review.as_mut() {
            review.grab = f32::from(y - bounds.top()) - thumb.left;
        }
    }

    /// Moves the thumb with the pointer at `y`; whether the list scrolled.
    pub(super) fn drag_review_thumb(&mut self, y: Pixels) -> bool {
        let Some((thumb, bounds, max)) = self.review_thumb() else {
            return false;
        };
        let Some(review) = self.menu.review.as_ref() else {
            return false;
        };
        let top = f32::from(y - bounds.top()) - review.grab;
        let scrolled = thumb.scrolled_at(f32::from(bounds.size.height), max, top);
        let handle = review.scroll.0.borrow().base_handle.clone();
        let offset = handle.offset();
        if (f32::from(offset.y) + scrolled).abs() < 0.5 {
            return false;
        }
        handle.set_offset(point(offset.x, px(-scrolled)));
        true
    }

    /// `list` with the scrollbar over its right edge.
    pub(super) fn review_scroll_area(
        &self,
        list: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let muted = self.theme.muted;
        let color = move |alpha: u32| rgba((muted << 8) | alpha);
        let thumb = self.review_thumb().map(|(thumb, _, _)| {
            div()
                .id("review-scroll-thumb")
                .debug_selector(|| "review-scroll-thumb".into())
                .absolute()
                .right(px(INSET))
                .top(px(thumb.left))
                .w(px(WIDTH))
                .h(px(thumb.width))
                .rounded_full()
                .bg(color(ALPHA))
                // The wheel still scrolls the diff over the thumb.
                .block_mouse_except_scroll()
                .hover(|s| s.bg(color(HOVER_ALPHA)))
                .active(|s| s.bg(color(HELD_ALPHA)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.grab_review_thumb(event.position.y);
                    }),
                )
                .on_drag(DiffThumb, |_, _, _, cx| cx.new(|_| EmptyView))
        });
        div()
            .id("review-scroll-area")
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DiffThumb>, _, cx| {
                    if this.drag_review_thumb(event.event.position.y) {
                        cx.notify();
                    }
                }),
            )
            // A wheel scroll moves the list itself; redraw so the thumb follows.
            .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
            .child(list)
            .children(thumb)
    }
}
