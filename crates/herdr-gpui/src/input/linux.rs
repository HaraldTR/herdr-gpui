//! GPUI 0.3.6 retains Wayland input handlers past its entity leak check at quit.
//! The view's UI owner keeps it alive; the platform handler must not extend that
//! lifetime. Non-Linux platforms continue to use GPUI's own ElementInputHandler.
use gpui::*;
use std::ops::Range;

pub(crate) struct WeakInputHandler<V: EntityInputHandler> {
    view: WeakEntity<V>,
    bounds: Bounds<Pixels>,
}

impl<V: EntityInputHandler> WeakInputHandler<V> {
    pub(crate) fn new(bounds: Bounds<Pixels>, view: Entity<V>) -> Self {
        Self {
            view: view.downgrade(),
            bounds,
        }
    }

    fn with<R: Default>(&self, callback: impl FnOnce(&mut ElementInputHandler<V>) -> R) -> R {
        let Some(view) = self.view.upgrade() else {
            return R::default();
        };
        callback(&mut ElementInputHandler::new(self.bounds, view))
    }
}

impl<V: EntityInputHandler> InputHandler for WeakInputHandler<V> {
    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        self.with(|handler| handler.selected_text_range(ignore_disabled_input, window, cx))
    }

    fn marked_text_range(&mut self, window: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.with(|handler| handler.marked_text_range(window, cx))
    }

    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<String> {
        self.with(|handler| handler.text_for_range(range_utf16, adjusted_range, window, cx))
    }

    fn replace_text_in_range(
        &mut self,
        replacement_range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.with(|handler| handler.replace_text_in_range(replacement_range, text, window, cx))
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.with(|handler| {
            handler.replace_and_mark_text_in_range(
                range_utf16,
                new_text,
                new_selected_range,
                window,
                cx,
            )
        })
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut App) {
        self.with(|handler| handler.unmark_text(window, cx))
    }

    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut App) {
        self.with(|handler| handler.paste(item, window, cx))
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        self.with(|handler| handler.bounds_for_range(range_utf16, window, cx))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<usize> {
        self.with(|handler| handler.character_index_for_point(point, window, cx))
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.with(|handler| handler.set_selected_text_range(range_utf16, window, cx))
    }

    fn element_bounds(&mut self, window: &mut Window, cx: &mut App) -> Option<Bounds<Pixels>> {
        self.with(|handler| handler.element_bounds(window, cx))
    }

    fn text_length_utf16(&mut self, window: &mut Window, cx: &mut App) -> Option<usize> {
        self.with(|handler| handler.text_length_utf16(window, cx))
    }

    fn accepts_text_input(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.with(|handler| handler.accepts_text_input(window, cx))
    }

    fn prefers_ime_for_printable_keys(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.with(|handler| handler.prefers_ime_for_printable_keys(window, cx))
    }

    fn text_input_configuration(
        &mut self,
        window: &mut Window,
        cx: &mut App,
    ) -> TextInputConfiguration {
        self.with(|handler| handler.text_input_configuration(window, cx))
    }

    fn text_input_editable_range(
        &mut self,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Range<usize>> {
        self.with(|handler| handler.text_input_editable_range(window, cx))
    }
}

#[cfg(test)]
mod tests;
