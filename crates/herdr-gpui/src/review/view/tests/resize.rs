//! Resizing the review's notes panel by its edge.
use super::{changes, window};
use gpui::{Modifiers, MouseButton, MouseDownEvent, point, px};

fn draw(cx: &mut gpui::VisualTestContext) {
    cx.update(|window, cx| crate::sidebar::layout_tests::full_draw(window, cx).clear(cx));
}

#[gpui::test]
fn dragging_the_notes_edge_resizes_it_and_a_double_click_resets(cx: &mut gpui::TestAppContext) {
    let (view, cx) = window(cx, None);
    cx.update(|_, cx| view.update(cx, |view, cx| view.seed_review(changes(), cx)));
    draw(cx);
    let panel = cx.debug_bounds("review-notes").unwrap();
    assert_eq!(panel.size.width, px(300.));
    let edge = cx.debug_bounds("review-notes-resize").unwrap();
    assert!(
        (edge.left() - panel.left()).abs() <= px(1.),
        "on the inner edge: {edge:?} {panel:?}"
    );

    // Dragging the edge 120px left widens the panel by as much.
    let start = edge.center();
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        point(start.x - px(40.), start.y),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_move(
        point(start.x - px(120.), start.y),
        MouseButton::Left,
        Modifiers::default(),
    );
    draw(cx);
    let wider = cx.debug_bounds("review-notes").unwrap();
    assert!((wider.size.width - px(420.)).abs() <= px(4.), "{wider:?}");
    assert_eq!(wider.right(), panel.right(), "the far edge stays put");
    // Released, the new width is saved once and kept.
    cx.simulate_mouse_up(
        point(start.x - px(120.), start.y),
        MouseButton::Left,
        Modifiers::default(),
    );
    view.update(cx, |view, _| {
        assert!(view.notes_width.chosen().is_some());
        assert!(!view.notes_width.take_unsaved(), "saved on release");
    });

    // Never wider than its share of the window.
    let edge = cx.debug_bounds("review-notes-resize").unwrap();
    cx.simulate_mouse_down(edge.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        point(edge.center().x - px(40.), edge.center().y),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_move(
        point(px(0.), edge.center().y),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(
        point(px(0.), edge.center().y),
        MouseButton::Left,
        Modifiers::default(),
    );
    draw(cx);
    let viewport = view.read_with(cx, |view, _| view.viewport_width);
    let widest = cx.debug_bounds("review-notes").unwrap();
    assert!(
        f32::from(widest.size.width) <= viewport * 0.6 + 1.,
        "{widest:?}"
    );

    // A double-click on the edge goes back to the default.
    let edge = cx.debug_bounds("review-notes-resize").unwrap();
    cx.simulate_event(MouseDownEvent {
        position: edge.center(),
        button: MouseButton::Left,
        modifiers: Modifiers::default(),
        click_count: 2,
        first_mouse: false,
    });
    draw(cx);
    assert_eq!(
        cx.debug_bounds("review-notes").unwrap().size.width,
        px(300.)
    );
    view.read_with(cx, |view, _| assert_eq!(view.notes_width.chosen(), None));
}
