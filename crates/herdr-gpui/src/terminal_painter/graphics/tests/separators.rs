use super::*;

#[test]
fn separator_shapes_reach_the_cell_edges_at_fractional_sizes() -> anyhow::Result<()> {
    for (symbol, shape) in [
        ("\u{e0b0}", CellSeparator::RightTriangle),
        ("\u{e0b2}", CellSeparator::LeftTriangle),
        ("\u{e0b4}", CellSeparator::RightRound),
        ("\u{e0b6}", CellSeparator::LeftRound),
    ] {
        assert_eq!(CellSeparator::from_symbol(symbol), Some(shape));
        assert!(Graphic::from_symbol(symbol).is_none());
        for scale in [1., 1.25, 1.5, 2., 3.] {
            let cell = Bounds::new(point(px(3.27), px(7.13)), size(px(22.75), px(51.2)));
            let snap = |value: Pixels| px((f32::from(value) * scale).round() / scale);
            let path = shape.path(cell, snap)?;
            assert_eq!(path.bounds.left(), snap(cell.left()), "{symbol} at {scale}");
            assert_eq!(
                path.bounds.right(),
                snap(cell.right()),
                "{symbol} at {scale}"
            );
            assert_eq!(path.bounds.top(), snap(cell.top()), "{symbol} at {scale}");
            assert_eq!(
                path.bounds.bottom(),
                snap(cell.bottom()),
                "{symbol} at {scale}"
            );
            assert!(!path.vertices.is_empty());
        }
    }
    for symbol in [
        "",
        "a",
        "\u{e0b0}\u{fe0f}",
        "\u{e0b6}\u{301}",
        "\u{e0b0}\u{e0b0}",
    ] {
        assert!(CellSeparator::from_symbol(symbol).is_none(), "{symbol:?}");
    }
    Ok(())
}

#[gpui::test]
fn separator_paths_match_gpui_backgrounds_at_half_device_pixels(cx: &mut gpui::TestAppContext) {
    use gpui::{Empty, Point, Styled, canvas, fill, rgb};
    use std::{cell::RefCell, rc::Rc};
    let (_, cx) = cx.add_window_view(|_, _| Empty);
    let paths = Rc::new(RefCell::new(Vec::new()));
    let painted = paths.clone();
    cx.draw(Point::default(), size(px(100.), px(100.)), |_, _| {
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                let scale = window.scale_factor();
                for offset in [0.5, -0.5, 1.5, -1.5] {
                    let cell = Bounds::new(
                        point(px(offset / scale), px((20. + offset) / scale)),
                        size(px(20. / scale), px(40. / scale)),
                    );
                    window.paint_quad(fill(cell, rgb(0x123456)));
                    let Ok(path) =
                        CellSeparator::LeftRound.path(cell, |value| window.pixel_snap(value))
                    else {
                        panic!("simple cap path must tessellate");
                    };
                    assert_eq!(path.bounds.left(), px(offset.trunc() / scale));
                    painted.borrow_mut().push(path.bounds.scale(scale));
                }
            },
        )
        .size_full()
    });
    cx.update(|window, _| {
        let quads = window.painted_quads();
        assert_eq!(quads.len(), 4);
        for (quad, path) in quads.iter().zip(paths.borrow().iter()) {
            assert_eq!(quad.bounds, *path);
        }
    });
}
