use super::*;
use core::prelude::v1::test;

#[test]
fn link_underlines_stay_inside_the_frame() {
    let frame = FrameData {
        width: 10,
        height: 3,
        cells: vec![cell("a"); 30],
        cursor: None,
        hyperlinks: vec![],
        graphics: vec![],
    };
    let rect = |x, y, width, height| SurfaceRect {
        x,
        y,
        width,
        height,
    };
    // A wrapped link's two rows, the second past the frame's right edge.
    assert_eq!(
        link_bounds(&frame, &[(1, 4..10), (2, 0..14)]),
        Some(rect(0, 1, 10, 2))
    );
    // Rows a resize has already removed, and empty ranges, paint nothing.
    assert_eq!(link_bounds(&frame, &[(3, 0..4), (0, 5..5)]), None);
    assert_eq!(link_bounds(&frame, &[(0, 12..14)]), None);
    assert_eq!(link_bounds(&frame, &[]), None);
}

#[gpui::test]
fn edge_backgrounds_reach_the_canvas_without_stretching_popups(cx: &mut TestAppContext) {
    let (_, cx) = cx.add_window_view(|_, _| Empty);
    for extend in [false, true] {
        cx.draw(Point::default(), size(px(100.), px(100.)), |_, _| {
            canvas(
                |_, _, _| (),
                move |_, _, window, cx| {
                    let frame = FrameData {
                        width: 2,
                        height: 2,
                        cells: [0x123456, 0x654321, 0xabcdef, 0xfedcba]
                            .into_iter()
                            .map(|bg| CellData {
                                bg: 0x02000000 | bg,
                                ..cell(" ")
                            })
                            .collect(),
                        cursor: None,
                        hyperlinks: vec![],
                        graphics: vec![],
                    };
                    let mut painter = TerminalPainter::default();
                    painter.set_appearance(14., 20., Theme::default());
                    painter.paint_frame(
                        &frame,
                        point(px(17.), px(23.)),
                        extend.then(|| size(px(23.), px(47.))),
                        10.,
                        &font("Menlo"),
                        &[],
                        &[],
                        None,
                        window,
                        cx,
                    );
                },
            )
            .size_full()
        });
        cx.update(|window, _| {
            let quads = window.painted_quads();
            assert_eq!(quads.len(), 4);
            for (x, y, width, height, color) in [
                (17., 23., 10., 20., 0x123456),
                (27., 23., if extend { 13. } else { 10. }, 20., 0x654321),
                (17., 43., 10., if extend { 27. } else { 20. }, 0xabcdef),
                (
                    27.,
                    43.,
                    if extend { 13. } else { 10. },
                    if extend { 27. } else { 20. },
                    0xfedcba,
                ),
            ] {
                let bounds = Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
                    .scale(window.scale_factor());
                assert!(
                    quads
                        .iter()
                        .any(|quad| quad.bounds == bounds && quad.background == rgb(color).into())
                );
            }
        });
    }
}

#[test]
fn backgrounds_fill_only_fractional_cell_remainders() {
    let cell = size(px(10.), px(20.));
    let available = size(px(103.), px(67.));
    let viewport = viewport(103., 67., 10., 20.);
    let grid = size(
        px(f32::from(viewport.cols) * 10.),
        px(f32::from(viewport.rows) * 20.),
    );
    assert_eq!(grid, size(px(100.), px(60.)));
    assert_eq!(background_extent(grid, available, cell), available);
    for (available, expected) in [
        (size(px(100.), px(60.)), grid),
        (size(px(99.), px(59.)), grid),
        (size(px(110.), px(80.)), grid),
        (size(px(111.), px(67.)), size(px(100.), px(67.))),
        (size(px(103.), px(81.)), size(px(103.), px(60.))),
    ] {
        assert_eq!(background_extent(grid, available, cell), expected);
    }
}

#[test]
#[allow(clippy::unwrap_used)]
fn paint_timing_threshold_interval_and_reset() {
    let start = Instant::now();
    let mut diagnostics = PaintDiagnostics::new(start);
    assert!(diagnostics.record(start, SLOW_PAINT).is_none());
    assert!(
        diagnostics
            .record(
                start + REPORT_INTERVAL - Duration::from_nanos(1),
                Duration::from_millis(17)
            )
            .is_none()
    );
    let timing = diagnostics
        .record(start + REPORT_INTERVAL, Duration::from_millis(3))
        .unwrap();
    assert_eq!(timing.count, 3);
    assert_eq!(timing.total, Duration::from_millis(36));
    assert_eq!(timing.max, Duration::from_millis(17));
    assert_eq!(timing.slow_count, 1);
    let timing = diagnostics
        .record(start + REPORT_INTERVAL * 2, Duration::from_millis(2))
        .unwrap();
    assert_eq!(timing.count, 1);
    assert_eq!(timing.total, Duration::from_millis(2));
    assert_eq!(timing.max, Duration::from_millis(2));
    assert_eq!(timing.slow_count, 0);
}

#[test]
fn paint_errors_report_immediately_then_coalesce() {
    let start = Instant::now();
    let mut diagnostics = PaintDiagnostics::new(start);
    assert_eq!(diagnostics.take_errors(start, 0), None);
    assert_eq!(diagnostics.take_errors(start, 2), Some(2));
    assert_eq!(diagnostics.take_errors(start, 3), None);
    assert_eq!(
        diagnostics.take_errors(start + REPORT_INTERVAL - Duration::from_nanos(1), 4),
        None
    );
    assert_eq!(diagnostics.take_errors(start + REPORT_INTERVAL, 0), Some(7));
    assert_eq!(
        diagnostics.take_errors(start + REPORT_INTERVAL * 2, 0),
        None
    );
}

fn cell(symbol: &str) -> CellData {
    CellData {
        symbol: symbol.into(),
        fg: 0,
        bg: 0,
        modifier: 0,
        skip: false,
        hyperlink: None,
    }
}

#[test]
fn decorations_cover_spaces_empty_and_wide_continuation_cells() {
    for (symbol, skip) in [("x", false), (" ", false), ("", false), ("", true)] {
        let mut cell = CellData {
            skip,
            ..cell(symbol)
        };
        assert_eq!(decoration_offsets(&cell, CELL_HEIGHT).count(), 0);
        cell.modifier = UNDERLINE;
        assert_eq!(
            decoration_offsets(&cell, CELL_HEIGHT).collect::<Vec<_>>(),
            vec![18.]
        );
        cell.modifier = STRIKETHROUGH;
        assert_eq!(
            decoration_offsets(&cell, CELL_HEIGHT).collect::<Vec<_>>(),
            vec![10.]
        );
        cell.modifier = UNDERLINE | STRIKETHROUGH;
        assert_eq!(
            decoration_offsets(&cell, CELL_HEIGHT).collect::<Vec<_>>(),
            vec![18., 10.]
        );
        assert_eq!(
            decoration_offsets(&cell, 30.5).collect::<Vec<_>>(),
            vec![28.5, 15.25]
        );
    }
}

#[cfg(feature = "integration-test")]
#[gpui::test]
fn blank_cells_paint_decorations_without_shaping(cx: &mut TestAppContext) {
    let (_, cx) = cx.add_window_view(|_, _| Empty);
    cx.draw(Point::default(), size(px(800.), px(600.)), |_, _| {
        canvas(
            |_, _, _| (),
            |bounds, _, window, cx| {
                let frame = FrameData {
                    width: 3,
                    height: 1,
                    cells: [(" ", false), ("", false), ("", true)]
                        .into_iter()
                        .map(|(symbol, skip)| CellData {
                            modifier: UNDERLINE | STRIKETHROUGH,
                            skip,
                            ..cell(symbol)
                        })
                        .collect(),
                    cursor: None,
                    hyperlinks: vec![],
                    graphics: vec![],
                };
                let mut painter = TerminalPainter::default();
                for (font_size, cell_height, theme) in [
                    (FONT_SIZE, CELL_HEIGHT, Theme::default()),
                    (
                        21.35,
                        30.5,
                        Theme {
                            foreground: 0xabcdef,
                            background: 0x123456,
                            ..Theme::default()
                        },
                    ),
                ] {
                    painter.set_appearance(font_size, cell_height, theme);
                    let before = cx
                        .default_global::<crate::performance::Counts>()
                        .decorations;
                    painter.paint_frame(
                        &frame,
                        bounds.origin,
                        None,
                        8.5,
                        &font("Menlo"),
                        &[],
                        &[],
                        None,
                        window,
                        cx,
                    );
                    assert_eq!(painter.glyphs.len(), 0);
                    assert_eq!(
                        cx.default_global::<crate::performance::Counts>()
                            .decorations
                            - before,
                        6
                    );
                }
            },
        )
        .size_full()
    });
}

#[test]
fn spans_cover_skip_cells_and_resolved_colors_without_crossing_rows() {
    let theme = Theme::default();
    let mut row = vec![cell("\u{754c}"), cell(""), cell("x"), cell("x")];
    row[1].skip = true;
    row[2].fg = 0x02123456;
    row[2].modifier = 64;
    row[3].bg = 0x02123456;
    assert_eq!(
        background_spans(&row, &theme).collect::<Vec<_>>(),
        vec![(0, 2, BACKGROUND), (2, 4, 0x123456)]
    );
    assert_eq!(background_spans(&[], &theme).count(), 0);
    for cells in row.chunks(2) {
        let expanded: Vec<_> = background_spans(cells, &theme)
            .flat_map(|(a, b, color)| (a..b).map(move |_| color))
            .collect();
        assert_eq!(
            expanded,
            cells
                .iter()
                .map(|c| cell_colors(c, &theme).1)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn wide_continuation_cells_take_the_glyph_background() {
    let theme = Theme::default();
    // The daemon sends continuation cells with a background of their own.
    let mut row = vec![cell("\u{3053}"), cell(""), cell("x"), cell("")];
    row[0].bg = 0x02373737;
    row[1].bg = 0x02000000;
    row[3].bg = 0x02000000;
    assert_eq!(
        background_spans(&row, &theme).collect::<Vec<_>>(),
        vec![(0, 2, 0x373737), (2, 3, BACKGROUND), (3, 4, 0)]
    );
    // Halfwidth katakana with a voiced or semi-voiced mark is two columns
    // wide in Herdr although its Unicode width is one.
    for kana in ["\u{ff76}\u{ff9e}", "\u{ff8a}\u{ff9f}"] {
        let mut row = vec![cell(kana), cell(""), cell("\u{ff76}"), cell("")];
        row[0].bg = 0x02373737;
        row[1].bg = 0x02000000;
        row[3].bg = 0x02000000;
        assert_eq!(
            background_spans(&row, &theme).collect::<Vec<_>>(),
            vec![(0, 2, 0x373737), (2, 3, BACKGROUND), (3, 4, 0)],
            "{kana}"
        );
    }
}

#[test]
fn cache_style_is_the_font_face_alone() {
    // Color, dim, reverse, hidden and grid decorations are painted, not shaped.
    for modifier in [0, 2, 8, 64, 128, 256, 2 | 64 | 128 | 256] {
        assert_eq!(glyphs::style(modifier), 0, "{modifier}");
    }
    let styles = [BOLD, ITALIC, BOLD | ITALIC].map(glyphs::style);
    assert_eq!(styles, [1, 2, 3]);
    for style in 0..4 {
        assert_eq!(glyphs::style(glyphs::style_modifier(style)), style);
    }
}

#[cfg(feature = "integration-test")]
#[gpui::test]
fn terminal_graphics_bypass_fonts_but_keep_decorations_and_skip_cells(cx: &mut TestAppContext) {
    let (_, cx) = cx.add_window_view(|_, _| Empty);
    cx.draw(Point::default(), size(px(800.), px(600.)), |_, _| {
        canvas(
            |_, _, _| (),
            |bounds, _, window, cx| {
                let mut frame = FrameData {
                    width: 5,
                    height: 1,
                    cells: vec![
                        CellData {
                            modifier: UNDERLINE | STRIKETHROUGH,
                            ..cell("▏")
                        },
                        CellData {
                            modifier: REVERSED,
                            ..cell("█")
                        },
                        CellData {
                            modifier: DIM,
                            ..cell("▀")
                        },
                        CellData {
                            modifier: HIDDEN,
                            ..cell("┼")
                        },
                        CellData {
                            skip: true,
                            ..cell("█")
                        },
                    ],
                    cursor: None,
                    hyperlinks: vec![],
                    graphics: vec![],
                };
                let mut painter = TerminalPainter::default();
                for family in ["Menlo", "Courier"] {
                    painter.set_appearance(21.35, 30.5, Theme::default());
                    let before = *cx.default_global::<crate::performance::Counts>();
                    painter.paint_frame(
                        &frame,
                        bounds.origin,
                        None,
                        12.81,
                        &font(family),
                        &[],
                        &[],
                        None,
                        window,
                        cx,
                    );
                    let after = cx.default_global::<crate::performance::Counts>();
                    assert_eq!(painter.glyphs.len(), 0);
                    assert_eq!(after.shapes, before.shapes);
                    assert_eq!(after.glyphs, before.glyphs);
                    assert_eq!(after.decorations - before.decorations, 2);
                    let backgrounds = background_spans(&frame.cells, &painter.theme).count();
                    assert_eq!(after.quads - before.quads, backgrounds + 7);
                }
                frame.cells[0] = cell("a");
                painter.paint_frame(
                    &frame,
                    bounds.origin,
                    None,
                    12.81,
                    &font("Menlo"),
                    &[],
                    &[],
                    None,
                    window,
                    cx,
                );
                assert_eq!(
                    painter.glyphs.len(),
                    1,
                    "ordinary text still uses the glyph cache"
                );
            },
        )
        .size_full()
    });
}

#[gpui::test]
fn placed_images_decode_off_thread_then_paint_in_z_order(cx: &mut TestAppContext) {
    use herdr_client::{
        SurfaceImages,
        protocol::{
            SurfaceGraphicsAsset, SurfaceGraphicsAssetKey, SurfaceGraphicsFormat,
            SurfaceGraphicsPlacement, SurfaceGraphicsSource, SurfaceGraphicsTarget,
        },
    };
    let (_, cx) = cx.add_window_view(|_, _| Empty);
    let painter = std::rc::Rc::new(std::cell::RefCell::new(TerminalPainter::default()));
    painter
        .borrow_mut()
        .set_appearance(14., 20., Theme::default());
    let key = |image_id, target| SurfaceGraphicsAssetKey {
        source: SurfaceGraphicsSource::Terminal { target, image_id },
        image_width: 1,
        image_height: 1,
        format: SurfaceGraphicsFormat::Rgba,
        data_len: 4,
        data_fingerprint: u64::from(image_id),
    };
    let pane = || SurfaceGraphicsTarget::Pane {
        pane_id: "p1".into(),
    };
    let (above, below) = (key(1, pane()), key(2, pane()));
    let popup = key(
        3,
        SurfaceGraphicsTarget::Popup {
            terminal_id: "t".into(),
        },
    );
    let images: Arc<SurfaceImages> = Arc::new(
        [&above, &below, &popup]
            .into_iter()
            .map(|key| SurfaceGraphicsAsset {
                key: key.clone(),
                data: vec![1, 2, 3, 255],
            })
            .collect(),
    );
    let place = |key: &SurfaceGraphicsAssetKey, x, z| SurfaceGraphicsPlacement {
        asset: key.clone(),
        logical_placement_id: 1,
        x,
        y: 0,
        cols: 1,
        rows: 1,
        source_x: 0,
        source_y: 0,
        source_width: 0,
        source_height: 0,
        x_offset: 0,
        y_offset: 0,
        z,
        scrollback_offset: 0,
    };
    // Scene order is not paint order: z decides.
    let placements: Arc<[SurfaceGraphicsPlacement]> = Arc::from([
        place(&above, 0, 5),
        place(&below, 1, -1),
        place(&popup, 2, 0),
    ]);
    let frame = FrameData {
        width: 3,
        height: 1,
        cells: vec![cell("x"); 3],
        cursor: None,
        hyperlinks: vec![],
        graphics: vec![],
    };
    let draw = |cx: &mut VisualTestContext| {
        let (painter, images, placements, frame) = (
            painter.clone(),
            images.clone(),
            placements.clone(),
            frame.clone(),
        );
        cx.draw(Point::default(), size(px(800.), px(600.)), |_, _| {
            canvas(
                |_, _, _| (),
                move |_, _, window, cx| {
                    painter.borrow_mut().paint_frame(
                        &frame,
                        point(px(10.), px(10.)),
                        None,
                        10.,
                        &font("Menlo"),
                        &[],
                        &[],
                        Some(PlacedImages {
                            placements: &placements,
                            images: &images,
                            target: ImageTarget::Main,
                        }),
                        window,
                        cx,
                    );
                },
            )
            .size_full()
        });
    };
    draw(cx);
    assert!(
        painter.borrow().painted_images.is_empty(),
        "nothing paints before its decode finishes"
    );
    cx.run_until_parked();
    draw(cx);
    let cell_at = |x: f32| Bounds::new(point(px(x), px(10.)), size(px(10.), px(20.)));
    // The popup's image belongs to the popup frame, not the main grid.
    assert_eq!(
        painter.borrow().painted_images,
        [(-1, cell_at(20.)), (5, cell_at(10.))]
    );
}

#[gpui::test]
fn cache_reuses_cells_invalidates_fonts_and_bounds_storage(cx: &mut TestAppContext) {
    let (_, cx) = cx.add_window_view(|_, _| Empty);
    let painter = std::rc::Rc::new(std::cell::RefCell::new(TerminalPainter::default()));
    let frame = FrameData {
        width: 5,
        height: 1,
        cells: vec![
            cell("x"),
            cell("x"),
            cell("e\u{301}"),
            cell("\u{754c}"),
            CellData {
                skip: true,
                ..cell("")
            },
        ],
        cursor: None,
        hyperlinks: vec![],
        graphics: vec![],
    };
    let mut draw = |frame: FrameData, font: Font| {
        let painter = painter.clone();
        cx.draw(Point::default(), size(px(800.), px(600.)), |_, _| {
            canvas(
                |_, _, _| (),
                move |bounds, _, window, cx| {
                    let cell_width = painter.borrow_mut().cell_width(&font, window, cx);
                    painter.borrow_mut().paint_frame(
                        &frame,
                        bounds.origin,
                        None,
                        cell_width,
                        &font,
                        &[],
                        &[],
                        None,
                        window,
                        cx,
                    );
                },
            )
            .size_full()
        });
    };
    draw(frame.clone(), font("Menlo"));
    assert_eq!(painter.borrow().glyphs.len(), 3);
    let original_width = painter.borrow().cell_width.unwrap_or_default();
    painter
        .borrow_mut()
        .set_appearance(FONT_SIZE, CELL_HEIGHT, Theme::default());
    assert_eq!(
        painter.borrow().glyphs.len(),
        3,
        "unchanged appearance retains glyphs"
    );
    assert_eq!(painter.borrow().cell_width, Some(original_width));
    let mut theme = Theme::default();
    for (font_size, cell_height) in [(28., CELL_HEIGHT), (28., 36.), (28., 36.)] {
        // The final iteration changes only the palette.
        if painter.borrow().cell_height == 36. {
            theme.palette[1] = 0x123456;
        }
        painter
            .borrow_mut()
            .set_appearance(font_size, cell_height, theme.clone());
        assert_eq!(painter.borrow().glyphs.len(), 0);
        assert!(painter.borrow().cell_width.is_none());
        draw(frame.clone(), font("Menlo"));
        assert_eq!(painter.borrow().glyphs.len(), 3);
        assert!(painter.borrow().cell_width.unwrap_or_default() > original_width * 1.5);
        let painter = painter.borrow();
        for (_, _, line) in painter.glyphs.iter() {
            assert_eq!(line.font_size, px(font_size));
        }
    }
    painter
        .borrow_mut()
        .set_appearance(FONT_SIZE, CELL_HEIGHT, Theme::default());
    draw(frame.clone(), font("Menlo"));
    assert_eq!(painter.borrow().glyphs.len(), 3);
    let mut changed = frame.clone();
    changed.cells[0].fg = 0x02123456;
    changed.cells[1].modifier = 1 | 4;
    draw(changed, font("Menlo"));
    assert_eq!(
        painter.borrow().glyphs.len(),
        4,
        "a new color reuses the glyph; only bold italic shapes again"
    );
    draw(frame.clone(), font("Courier"));
    assert_eq!(
        painter.borrow().glyphs.len(),
        3,
        "new font discards old glyphs"
    );
    // A changed icon cascade reshapes every cell: the same family can now
    // resolve Private Use Area glyphs a text face does not carry.
    let with_fallbacks = |families: &[&str]| {
        crate::config::FontConfig {
            family: "Courier".into(),
            size: FONT_SIZE,
            fallbacks: Some(families.iter().map(|family| (*family).to_owned()).collect()),
        }
        .font()
    };
    let cascaded = with_fallbacks(&["Symbols Nerd Font Mono"]);
    assert_ne!(cascaded, font("Courier"));
    draw(frame.clone(), cascaded.clone());
    assert_eq!(
        painter.borrow().glyphs.len(),
        3,
        "an added cascade discards old glyphs"
    );
    assert_eq!(painter.borrow().config.as_ref(), Some(&cascaded));
    draw(frame.clone(), with_fallbacks(&["Hack Nerd Font Mono"]));
    assert_eq!(
        painter.borrow().glyphs.len(),
        3,
        "a reordered cascade discards old glyphs"
    );
    draw(frame, font("Courier"));
    let colors = FrameData {
        width: 100,
        height: 50,
        cells: (0..5000)
            .map(|i| CellData {
                fg: 0x02000000 | i,
                ..cell("x")
            })
            .collect(),
        cursor: None,
        hyperlinks: vec![],
        graphics: vec![],
    };
    draw(colors, font("Menlo"));
    assert_eq!(
        painter.borrow().glyphs.len(),
        1,
        "truecolor output shares one glyph"
    );
    let symbols = FrameData {
        width: 100,
        height: 50,
        cells: (0..5000)
            .filter_map(|i| char::from_u32(0x4e00 + i))
            .map(|symbol| cell(&symbol.to_string()))
            .collect(),
        cursor: None,
        hyperlinks: vec![],
        graphics: vec![],
    };
    draw(symbols, font("Menlo"));
    assert_eq!(painter.borrow().glyphs.len(), glyphs::CACHE_LIMIT);
    assert_eq!(painter.borrow().glyphs.iter().count(), glyphs::CACHE_LIMIT);
}

#[test]
fn composition_stays_at_the_cursor_inside_the_grid() {
    let grid = Bounds::new(point(px(10.), px(20.)), size(px(100.), px(40.)));
    let cursor = point(px(30.), px(40.));
    assert_eq!(composition_origin(cursor, px(50.), grid), cursor);
    assert_eq!(
        composition_origin(point(px(90.), px(40.)), px(50.), grid),
        point(px(60.), px(40.)),
        "shifted left just enough to end at the grid's edge"
    );
    assert_eq!(
        composition_origin(cursor, px(200.), grid),
        point(px(-90.), px(40.)),
        "wider than the grid, it keeps its end, where the IME edits"
    );
}

#[test]
fn composition_ranges_count_utf16_units() {
    let text = "a\u{304b}\u{1f600}b";
    assert_eq!(
        [0, 1, 2, 3, 4, 5, 6].map(|utf16| byte_index(text, utf16)),
        [0, 1, 4, 8, 8, 9, 9]
    );
    assert_eq!(byte_index("", 1), 0);
}

#[gpui::test]
fn composition_bounds_follow_the_converted_clause(cx: &mut TestAppContext) {
    let (_, cx) = cx.add_window_view(|_, _| Empty);
    cx.draw(Point::default(), size(px(800.), px(600.)), |_, _| {
        canvas(
            |_, _, _| (),
            |bounds, _, window, cx| {
                let mut painter = TerminalPainter::default();
                let font = font("Menlo");
                let cell_width = painter.cell_width(&font, window, cx);
                let cursor = Bounds::new(
                    bounds.origin + point(px(cell_width * 4.), px(CELL_HEIGHT)),
                    size(px(cell_width), px(CELL_HEIGHT)),
                );
                // Romaji mid-composition: the headless text system gives
                // CJK glyphs next to no advance, which would hide the
                // right-edge geometry. UTF-16 mapping is tested separately.
                let text = "kan";
                let bounds_of = |range, cursor| {
                    painter.composition_bounds(text, range, cursor, bounds, &font, window)
                };
                assert_eq!(
                    painter.composition_bounds("", 0..0, cursor, bounds, &font, window),
                    cursor,
                    "without a composition the IME anchors at the cursor"
                );
                let first = bounds_of(0..1, cursor);
                let second = bounds_of(1..2, cursor);
                let caret = bounds_of(3..3, cursor);
                assert_eq!(first.origin, cursor.origin);
                assert_eq!(second.origin.y, cursor.origin.y);
                assert!(second.origin.x > first.origin.x);
                assert!(caret.origin.x > second.origin.x);
                assert_eq!(caret.size, cursor.size);
                // At the right edge the text, and so every clause, shifts left.
                let edge = Bounds::new(
                    point(bounds.right() - px(cell_width), cursor.origin.y),
                    cursor.size,
                );
                let end = bounds_of(3..3, edge);
                assert_eq!(end.right(), bounds.right(), "the caret stays inside");
                assert_eq!(end.size, cursor.size);
                assert!(bounds_of(0..1, edge).origin.x < edge.origin.x);
                // Wider than the grid, the start scrolls off the left while
                // every clause still anchors inside the grid.
                let long = "k".repeat(200);
                let long_bounds =
                    |range| painter.composition_bounds(&long, range, cursor, bounds, &font, window);
                assert_eq!(long_bounds(0..1).origin.x, bounds.left());
                assert_eq!(long_bounds(200..200).right(), bounds.right());
            },
        )
        .size_full()
    });
}

#[test]
fn spans_and_styles_use_custom_theme() {
    let mut theme = Theme {
        background: 0x123456,
        foreground: 0xabcdef,
        ..Theme::default()
    };
    theme.palette[200] = theme.background;
    theme.palette[1] = 0x654321;
    let row = [
        cell("x"),
        CellData {
            bg: 0x010000c8,
            fg: 2,
            ..cell("y")
        },
    ];
    assert_eq!(
        background_spans(&row, &theme).collect::<Vec<_>>(),
        vec![(0, 2, theme.background)]
    );
    assert_eq!(cell_colors(&row[0], &theme).0, theme.foreground);
    assert_eq!(cell_colors(&row[1], &theme).0, theme.palette[1]);
}
