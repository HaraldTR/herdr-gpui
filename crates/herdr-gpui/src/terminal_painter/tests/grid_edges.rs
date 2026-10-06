use super::*;

fn row(colors: &[u32], modifier: u16) -> FrameData {
    FrameData {
        width: colors.len() as u16,
        height: 1,
        cells: colors
            .iter()
            .map(|&bg| CellData {
                fg: 0x02_123456,
                bg: 0x0200_0000 | bg,
                modifier,
                ..cell(" ")
            })
            .collect(),
        cursor: None,
        hyperlinks: vec![],
        graphics: vec![],
    }
}

#[test]
fn only_a_one_color_last_row_continues_below_the_grid() {
    let theme = Theme::default();
    assert!(last_row_is_uniform(&row(&[0xabcdef; 4], 0), &theme));
    // A prompt: colored segments, then the default background.
    let mut prompt = row(&[0xd79921, 0xd79921, 0x689d6a, 0], 0);
    prompt.cells[3].bg = 0;
    assert!(!last_row_is_uniform(&prompt, &theme));
    // Resolved colors decide: a reversed cell shows its foreground behind it.
    let mut reversed = row(&[0x123456, 0x654321], 0);
    reversed.cells[1].fg = 0x0212_3456;
    reversed.cells[1].modifier = REVERSED;
    assert!(last_row_is_uniform(&reversed, &theme));
    // Only the last row matters.
    let mut two_rows = row(&[0x111111, 0x222222, 0x333333, 0x333333], 0);
    two_rows.width = 2;
    two_rows.height = 2;
    assert!(last_row_is_uniform(&two_rows, &theme));
    let empty = FrameData {
        height: 0,
        cells: vec![],
        ..row(&[0], 0)
    };
    assert!(!last_row_is_uniform(&empty, &theme));
}

/// Paints one row at a fractional cell width where rebuilding a right edge as
/// left + width crosses a 2x rounding tie, then returns its painted quads.
fn paint_row(
    cx: &mut VisualTestContext,
    frame: FrameData,
    highlights: Vec<Highlight>,
    (x, cell_width): (f32, f32),
) -> Vec<Quad> {
    cx.draw(Point::default(), size(px(1920.), px(100.)), |_, _| {
        canvas(
            |_, _, _| (),
            move |_, _, window, cx| {
                let mut painter = TerminalPainter::default();
                painter.set_appearance(14., 40., Theme::default());
                painter.paint_frame(
                    &frame,
                    point(px(x), px(8.)),
                    None,
                    cell_width,
                    &font("Menlo"),
                    &highlights,
                    &[],
                    None,
                    None,
                    window,
                    cx,
                );
            },
        )
        .size_full()
    });
    cx.update(|window, _| window.painted_quads())
}

#[gpui::test]
fn highlight_tints_share_the_background_edges(cx: &mut TestAppContext) {
    let (_, cx) = cx.add_window_view(|_, _| Empty);
    let colors: Vec<u32> = (0..51)
        .map(|x| if x < 12 { 0x57858b } else { 0xd2a241 })
        .collect();
    let tint = Tint::Selection.color(&Theme::default());
    let quads = paint_row(
        cx,
        row(&colors, 0),
        vec![Highlight {
            row: 0,
            columns: 12..50,
            tint: Tint::Selection,
        }],
        (17., 8.015),
    );
    let (tints, backgrounds): (Vec<_>, Vec<_>) = quads
        .iter()
        .partition(|quad| quad.background == tint.into());
    assert_eq!(tints.len(), 1);
    let second = backgrounds
        .iter()
        .find(|quad: &&&Quad| quad.background == rgb(0xd2a241).into())
        .map(|quad| quad.bounds.left());
    assert_eq!(Some(tints[0].bounds.left()), second);
    // Column 50's absolute edge, the one the old left + width arithmetic missed.
    cx.update(|window, _| {
        let edge = window
            .pixel_snap(px(17. + 50. * 8.015))
            .scale(window.scale_factor());
        assert_eq!(tints[0].bounds.right(), edge);
    });
}

#[gpui::test]
fn underline_runs_have_no_seams(cx: &mut TestAppContext) {
    let (_, cx) = cx.add_window_view(|_, _| Empty);
    for x in [0., 3., 3.25, 17.] {
        for cell_width in [8.015, 23.750_002] {
            // As many cells as the test window's 1920 px show.
            let cells = vec![0; ((1900. - x) / cell_width) as usize];
            let count = cells.len();
            let mut quads = paint_row(cx, row(&cells, UNDERLINE), vec![], (x, cell_width));
            quads.retain(|quad| quad.background == rgb(0x123456).into());
            quads.sort_by_key(|quad| quad.bounds.left());
            assert_eq!(quads.len(), count);
            for pair in quads.windows(2) {
                assert_eq!(
                    pair[0].bounds.right(),
                    pair[1].bounds.left(),
                    "origin={x} width={cell_width}"
                );
            }
        }
    }
}
