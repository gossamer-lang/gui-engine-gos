use super::*;

fn options(rows: i64, cols: i64, scrollback: i64) -> Options {
    Options {
        rows,
        cols,
        scrollback,
        byte_limit: 16 * 1024 * 1024,
    }
}

#[test]
fn a_recorded_stream_scrolls_selects_resizes_and_releases_its_storage() {
    let budget = Budget::new(64 * 1024 * 1024);
    let id = create(&options(4, 20, 50), budget.clone()).unwrap();
    // A recorded session: colors, a title, a wide character and a bell.
    let mut recorded = b"\x1b]0;recorded\x07".to_vec();
    for n in 0..30 {
        recorded.extend(format!("\x1b[3{}mline {n:02}\x1b[0m\r\n", n % 8).bytes());
    }
    recorded.extend("界 end\x07".bytes());
    feed(id, &recorded).unwrap();
    let shown = snapshot(id).unwrap();
    assert_eq!((shown.rows, shown.cols), (4, 20));
    assert!(shown.text.contains("界 end"), "{}", shown.text);
    assert_eq!(shown.title, "recorded");
    assert!(!shown.running);
    assert_eq!(scroll(id, 10).unwrap(), 10);
    assert!(snapshot(id).unwrap().text.contains("line 17"));
    assert_eq!(scroll(id, 1000).unwrap(), 27);
    assert_eq!(scroll(id, -1000).unwrap(), 0);
    select(id, Some((Cell { row: 2, col: 0 }, Cell { row: 2, col: 7 }))).unwrap();
    assert_eq!(copy(id).unwrap(), "line 29");
    // The buffer reads whole, oldest row first, wherever the view is.
    scroll(id, 5).unwrap();
    let buffer = rows(id).unwrap();
    assert_eq!(buffer.len(), 27 + 4);
    assert_eq!(snapshot(id).unwrap().history, 27);
    assert_eq!(buffer[0].text, "line 00");
    assert_eq!(buffer[buffer.len() - 1].text, "界 end");
    assert!(buffer.iter().all(|row| !row.wrapped));
    scroll(id, -5).unwrap();
    feed(id, b"\r\n0123456789012345678901234").unwrap();
    let wrapped = rows(id).unwrap();
    let n = wrapped.len();
    assert!(wrapped[n - 2].wrapped);
    assert_eq!(wrapped[n - 2].text, "01234567890123456789");
    assert_eq!(wrapped[n - 1].text, "01234");
    resize(id, 10, 30).unwrap();
    let resized = snapshot(id).unwrap();
    assert_eq!((resized.rows, resized.cols), (10, 30));
    assert_eq!(resized.selection, None);
    assert!(resize(id, 300, 30).is_err());
    assert!(feed(id, &vec![b'x'; 2 * 1024 * 1024]).is_err());
    assert!(budget.usage().0 >= ENTRY_BYTES);
    close(id);
    assert_eq!(budget.usage().0, 0);
    assert!(snapshot(id).is_err());
}

#[test]
fn input_without_a_program_is_refused_and_sessions_release_their_terminals() {
    let budget = Budget::new(64 * 1024 * 1024);
    let ids: Vec<i64> = (0..3)
        .map(|_| create(&Options::default(), budget.clone()).unwrap())
        .collect();
    assert!(input(ids[0], b"x").is_err());
    assert!(create(&options(0, 80, 10), budget.clone()).is_err());
    assert!(create(&options(24, 80, 200_000), budget.clone()).is_err());
    close_budget(&budget);
    assert_eq!(budget.usage().0, 0);
    assert!(ids.iter().all(|id| snapshot(*id).is_err()));
}

#[test]
fn a_screen_that_cannot_take_its_view_reports_why_once() {
    let budget = Budget::new(64 * 1024 * 1024);
    let small = Options {
        byte_limit: 1024 * 1024,
        ..options(4, 20, 50)
    };
    let id = create(&small, budget.clone()).unwrap();
    for _ in 0..2 {
        with(id, |entry| {
            follow_view(entry, 200, 500);
            Ok(())
        })
        .unwrap();
    }
    let (_, events) = pump(&budget);
    let failures: Vec<_> = events
        .iter()
        .filter(|m| matches!(m, UiMsg::Terminal(t, Event::Refused(_)) if *t == id))
        .collect();
    assert_eq!(failures.len(), 1, "{events:?}");
    assert_eq!(snapshot(id).unwrap().rows, 4);
    assert!(pump(&budget).1.is_empty());
    close_budget(&budget);
}

#[test]
fn links_are_found_across_wrapped_rows_and_marked_links_keep_their_target() {
    let id = create(&options(6, 20, 10), Budget::new(64 * 1024 * 1024)).unwrap();
    feed(
        id,
        b"see https://example.org/a/long/path, then\r\n\x1b]8;;https://marked.example/\x1b\\here\x1b]8;;\x1b\\ and https://x.example/q",
    )
    .unwrap();
    let links = with_entry(id, |e| e.engine.frame().unwrap().links().to_vec()).unwrap();
    let runs: Vec<(u16, u16, u16, &str)> = links
        .iter()
        .map(|l| (l.row, l.start, l.end, &*l.uri))
        .collect();
    assert_eq!(
        runs,
        [
            (3, 0, 4, "https://marked.example/"),
            (0, 4, 20, "https://example.org/a/long/path"),
            (1, 0, 15, "https://example.org/a/long/path"),
            (3, 9, 20, "https://x.example/q"),
            (4, 0, 8, "https://x.example/q"),
        ]
    );
    close(id);
}

#[cfg(unix)]
mod session {
    use super::*;
    use crate::accessibility::AccessNode;
    use crate::widgets::Widget;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn find<'a>(node: &'a AccessNode, id: &str) -> Option<&'a AccessNode> {
        if node.id == id {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, id))
    }

    fn step_until(s: &mut crate::Session, mut done: impl FnMut() -> bool) {
        while !done() {
            assert!(s.step().unwrap(), "the session ended");
        }
    }

    #[test]
    fn a_real_program_runs_in_a_view_with_typed_input_events_and_cleanup() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::app::start("Terminal", "dev.gossamer.gui-terminal-test").unwrap();
            let id = create(&options(8, 40, 100), crate::app::effects::current_budget()).unwrap();
            let events = Rc::new(RefCell::new(Vec::new()));
            let heard = events.clone();
            s.set_shell(
                move |m| {
                    if let UiMsg::Terminal(_, event) = m {
                        heard.borrow_mut().push(event);
                    }
                },
                move |_| Widget::Terminal(widget::View::new("term", id, "Shell")),
                |_| Vec::new(),
            );
            s.open_window("main", "Terminal", 420, 240).unwrap();
            let script = "printf '\\033]2;demo\\007ready\\r\\n'; read line; \
                      printf 'got:%s\\r\\n' \"$line\"; exit 3";
            spawn(
                id,
                &Command {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), script.into()],
                    ..Command::default()
                },
            )
            .unwrap();
            assert!(s.focus("term").unwrap());
            step_until(&mut s, || snapshot(id).unwrap().text.contains("ready"));
            // The view sized the screen to the window and told the program.
            let sized = snapshot(id).unwrap();
            assert!(sized.cols > 20 && sized.rows > 4, "{sized:?}");
            s.type_text("héllo 界").unwrap();
            s.press(windowing::Key::Return, windowing::Mods::default())
                .unwrap();
            step_until(&mut s, || events.borrow().contains(&Event::Exited(3)));
            assert!(events.borrow().contains(&Event::Title("demo".into())));
            let shown = snapshot(id).unwrap();
            assert!(shown.text.contains("got:héllo 界"), "{}", shown.text);
            assert!(!shown.running);
            let tree = s.semantics(0);
            let node = find(&tree, "term").unwrap();
            assert_eq!(node.role, crate::accessibility::Role::Terminal);
            assert!(node.value.contains("got:héllo 界"));
            assert_eq!(node.description, "demo");
            assert!(node.focused);
            s.shutdown();
            assert!(snapshot(id).is_err());
        })
    }

    #[test]
    fn a_program_in_another_character_set_reads_and_writes_through_it() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::app::start("Charset", "dev.gossamer.gui-terminal-charset").unwrap();
            let id = create(&options(8, 40, 100), crate::app::effects::current_budget()).unwrap();
            let exited = Rc::new(RefCell::new(false));
            let heard = exited.clone();
            s.set_shell(
                move |m| {
                    if let UiMsg::Terminal(_, Event::Exited(_)) = m {
                        *heard.borrow_mut() = true;
                    }
                },
                move |_| Widget::Terminal(widget::View::new("term", id, "Latin-1")),
                |_| Vec::new(),
            );
            s.open_window("main", "Charset", 420, 240).unwrap();
            assert!(set_encoding(id, "UTF-16LE").is_err());
            assert!(set_encoding(id, "no-such-set").is_err());
            set_encoding(id, "ISO-8859-1").unwrap();
            let script = "printf 'caf\\351\\r\\n'; read line; \
                      printf 'got:'; printf '%s' \"$line\" | od -An -tx1 | tr -d ' \\n'; exit 0";
            spawn(
                id,
                &Command {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), script.into()],
                    ..Command::default()
                },
            )
            .unwrap();
            assert!(s.focus("term").unwrap());
            step_until(&mut s, || snapshot(id).unwrap().text.contains("café"));
            s.type_text("é").unwrap();
            s.press(windowing::Key::Return, windowing::Mods::default())
                .unwrap();
            step_until(&mut s, || *exited.borrow());
            let shown = snapshot(id).unwrap().text;
            assert!(shown.contains("got:e9"), "{shown}");
            s.shutdown();
        })
    }

    #[test]
    fn the_layout_cache_keeps_the_screen_it_shows_and_not_the_output_that_scrolled_past() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::app::start("Flood", "dev.gossamer.gui-terminal-flood").unwrap();
            let id = create(&options(8, 40, 100), crate::app::effects::current_budget()).unwrap();
            s.set_shell(
                |_| {},
                move |_| Widget::Terminal(widget::View::new("term", id, "Flood")),
                |_| vec![],
            );
            s.open_window("main", "Flood", 900, 600).unwrap();
            // Each line of input prints the next batch of output.
            spawn(
                id,
                &Command {
                    program: "/bin/sh".into(),
                    args: vec![
                        "-c".into(),
                        "i=0; while read x; do j=0; while [ $j -lt 30 ]; do echo output line $i; \
                         i=$((i+1)); j=$((j+1)); done; echo batch $i; done"
                            .into(),
                    ],
                    ..Command::default()
                },
            )
            .unwrap();
            for batch in 1..=40 {
                input(id, b"\r").unwrap();
                let shown = format!("batch {}", batch * 30);
                while !snapshot(id).unwrap().text.contains(&shown) {
                    assert!(s.step().unwrap(), "the session ended");
                }
                let before = s.windows[0].frames;
                while s.windows[0].frames == before {
                    assert!(s.step().unwrap(), "the session ended");
                }
            }
            let rows = snapshot(id).unwrap().rows as usize;
            let held = with_entry(id, |e| e.layouts.len()).unwrap();
            assert!(
                held <= rows,
                "{held} run layouts kept for a {rows}-row screen"
            );
            close(id);
            s.shutdown();
        })
    }

    #[test]
    fn text_after_a_wide_cell_keeps_its_column_and_an_overwide_glyph_stays_in_its_cell() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::app::start("Cells", "dev.gossamer.gui-terminal-cells").unwrap();
            let id = create(&options(8, 40, 100), crate::app::effects::current_budget()).unwrap();
            s.set_shell(
                |_| {},
                move |_| Widget::Terminal(widget::View::new("term", id, "Cells")),
                |_| vec![],
            );
            s.open_window("main", "Cells", 420, 240).unwrap();
            // A wide CJK cell, then an arrow one cell wide whose glyph is
            // wider; each row is followed by one showing the bracket alone.
            spawn(
                id,
                &Command {
                    program: "/bin/sh".into(),
                    args: vec![
                        "-c".into(),
                        "printf '\\347\\225\\214]\\n  ]\\n\\342\\237\\267]\\n ]\\nshown\\n'; read x".into(),
                    ],
                    ..Command::default()
                },
            )
            .unwrap();
            while !snapshot(id).unwrap().text.contains("shown") {
                assert!(s.step().unwrap(), "the session ended");
            }
            let painted = s.windows[0].frames;
            while s.windows[0].frames == painted {
                assert!(s.step().unwrap(), "the session ended");
            }
            let canvas = s.capture().unwrap();
            let theme = s.windows[0].theme.clone();
            let (cw, lh, _) = widget::metrics(&widget::View::new("term", id, "Cells"), &theme);
            let r = s.rect_of("term").unwrap();
            let pad = theme.px(widget::PAD);
            let background = theme.field;
            // The mean x and the count of ink pixels in row `row` between
            // columns `from` and `to`.
            let ink = |row: i64, from: f64, to: f64| -> Option<(f64, f64)> {
                let top = r.y + pad + (row as f64 * lh) as i64;
                let (mut sum, mut n) = (0.0, 0.0);
                for y in top..top + lh as i64 {
                    let left = r.x + pad + (from * cw) as i64;
                    for x in left..r.x + pad + (to * cw) as i64 {
                        let at = ((y * canvas.width + x) * 4) as usize;
                        let p = &canvas.pixels[at..at + 3];
                        let far = (i64::from(p[0]) - i64::from(background.r)).abs()
                            + (i64::from(p[1]) - i64::from(background.g)).abs()
                            + (i64::from(p[2]) - i64::from(background.b)).abs();
                        if far > 96 {
                            sum += x as f64;
                            n += 1.0;
                        }
                    }
                }
                (n > 0.0).then(|| (sum / n, n))
            };
            let bracket = ink(1, 2.0, 3.0).expect("the lone bracket shows").0;
            let after_wide = ink(0, 2.0, 3.0)
                .expect("the bracket after the wide cell shows")
                .0;
            assert!(
                (bracket - after_wide).abs() <= 1.0,
                "{after_wide} against {bracket}"
            );
            assert!(ink(2, 0.0, 1.0).is_some(), "the arrow shows");
            // Past the arrow's own cell only the bracket has ink.
            let alone = ink(3, 1.0, 2.0).expect("the lone bracket shows").1;
            let beside = ink(2, 1.0, 2.0)
                .expect("the bracket beside the arrow shows")
                .1;
            assert!(
                beside <= alone + 2.0,
                "the arrow spills: {beside} ink pixels against {alone}"
            );
            close(id);
            s.shutdown();
        })
    }

    #[test]
    fn a_link_opens_on_its_click_chord_and_a_right_click_reaches_the_context_menu() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::app::start("Links", "dev.gossamer.gui-terminal-links").unwrap();
            let id = create(&options(8, 40, 100), crate::app::effects::current_budget()).unwrap();
            let events = Rc::new(RefCell::new(Vec::new()));
            let heard = events.clone();
            s.set_shell(
                move |m| {
                    if let UiMsg::Terminal(_, event) = m {
                        heard.borrow_mut().push(event);
                    }
                },
                move |_| {
                    Widget::Terminal(widget::View::new("term", id, "Links")).context_menu(
                        "ctx",
                        &[crate::widgets::menus::labeled("edit.copy", "Copy")],
                    )
                },
                |_| vec![crate::commands::Command::new("edit.copy", "Copy")],
            );
            s.open_window("main", "Links", 420, 240).unwrap();
            feed(id, b"  https://example.org/x now").unwrap();
            // A link is found where the screen shows it, so wait for a paint.
            step_until(&mut s, || {
                with_entry(id, |e| !e.links.is_empty()) == Some(true)
            });
            let shown = snapshot(id).unwrap();
            let r = s.rect_of("term").unwrap();
            // The grid fills the view inside a padding narrower than a cell.
            let cell_width = r.width as f64 / shown.cols as f64;
            let row_height = r.height as f64 / shown.rows as f64;
            let at = |col: f64, row: f64| {
                (
                    r.x + (col * cell_width) as i64 + 4,
                    r.y + (row * row_height) as i64 + 4 + (row_height / 2.0) as i64,
                )
            };
            let (lx, ly) = at(8.0, 0.0);
            s.hover(lx, ly).unwrap();
            assert_eq!(s.cursor(), Some(windowing::SystemCursor::Pointer));
            s.click_at(lx, ly).unwrap();
            let chord = windowing::Mods {
                ctrl: !cfg!(target_os = "macos"),
                gui: cfg!(target_os = "macos"),
                ..windowing::Mods::default()
            };
            s.click_with(lx, ly, chord).unwrap();
            let link = "https://example.org/x".to_string();
            assert_eq!(
                *events.borrow(),
                [Event::LinkPointed(link.clone()), Event::LinkActivated(link)]
            );
            let (ox, oy) = at(2.0, 4.0);
            s.right_click_at(ox, oy).unwrap();
            assert_eq!(
                events.borrow().last(),
                Some(&Event::LinkPointed(String::new()))
            );
            assert!(
                s.semantics(0).dump().contains("menuitem"),
                "{}",
                s.semantics(0).dump()
            );
            s.shutdown();
        })
    }

    #[test]
    fn ctrl_shift_chords_and_f11_reach_the_window_commands_from_a_focused_terminal() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::app::start("Chords", "dev.gossamer.gui-terminal-chords").unwrap();
            let id = create(&options(8, 40, 100), crate::app::effects::current_budget()).unwrap();
            let ran = Rc::new(RefCell::new(Vec::new()));
            let heard = ran.clone();
            s.set_shell(
                move |m| {
                    if let UiMsg::Command(command) = m {
                        heard.borrow_mut().push(command);
                    }
                },
                move |_| Widget::Terminal(widget::View::new("term", id, "Chords")),
                |_| {
                    vec![
                        crate::commands::Command::new("tab.new", "New Tab").key("Ctrl+Shift+T"),
                        crate::commands::Command::new("tab.right", "Move Right")
                            .key("Ctrl+Shift+PageDown"),
                        crate::commands::Command::new("view.zoom-in", "Zoom In")
                            .key("Ctrl+Shift+="),
                        crate::commands::Command::new("view.fullscreen", "Fullscreen").key("F11"),
                    ]
                },
            );
            s.open_window("main", "Chords", 420, 240).unwrap();
            // A running program, which a stroke the terminal took would reach.
            spawn(
                id,
                &Command {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), "read line".into()],
                    ..Command::default()
                },
            )
            .unwrap();
            assert!(s.focus("term").unwrap());
            let chord = windowing::Mods {
                ctrl: true,
                shift: true,
                ..windowing::Mods::default()
            };
            s.press(windowing::Key::T, chord).unwrap();
            s.press(windowing::Key::PageDown, chord).unwrap();
            s.press(windowing::Key::Char('='), chord).unwrap();
            s.press(windowing::Key::F11, windowing::Mods::default())
                .unwrap();
            s.step().unwrap();
            assert_eq!(
                *ran.borrow(),
                ["tab.new", "tab.right", "view.zoom-in", "view.fullscreen"]
            );
            s.shutdown();
        })
    }

    #[test]
    fn f10_reaches_the_menu_bar_and_typed_arrows_show_no_focus_ring() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::app::start("Menus", "dev.gossamer.gui-terminal-menus").unwrap();
            let id = create(&options(8, 40, 100), crate::app::effects::current_budget()).unwrap();
            s.set_shell(
                |_| {},
                move |_| {
                    Widget::column(
                        &[
                            Widget::joined_menu_bar(
                                "menu",
                                &[crate::widgets::menus::Menu::new("&File", &[])],
                            ),
                            Widget::Terminal(widget::View::new("term", id, "Menus")),
                        ],
                        0,
                    )
                },
                |_| Vec::new(),
            );
            s.open_window("main", "Menus", 420, 240).unwrap();
            spawn(
                id,
                &Command {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), "read line".into()],
                    ..Command::default()
                },
            )
            .unwrap();
            assert!(s.focus("term").unwrap());
            s.press(windowing::Key::Up, windowing::Mods::default())
                .unwrap();
            assert!(!s.windows[0].focus_visible);
            s.press(windowing::Key::F10, windowing::Mods::default())
                .unwrap();
            // A system menu bar (macOS) leaves no in-window bar to reach, so
            // F10 stays with the terminal's program.
            let expected = if windowing::menu::has_native_menu_bar() {
                "term"
            } else {
                "menu"
            };
            assert_eq!(s.focused(), expected);
            s.shutdown();
        })
    }

    #[test]
    fn sustained_output_stays_within_its_queues_and_the_screen_keeps_the_end() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s =
                crate::app::start("Terminal load", "dev.gossamer.gui-terminal-load").unwrap();
            let budget = crate::app::effects::current_budget();
            let id = create(&options(24, 80, 500), budget.clone()).unwrap();
            let exited = Rc::new(RefCell::new(false));
            let heard = exited.clone();
            s.set_shell(
                move |m| {
                    if let UiMsg::Terminal(_, Event::Exited(0)) = m {
                        *heard.borrow_mut() = true;
                    }
                },
                move |_| Widget::Terminal(widget::View::new("term", id, "Load")),
                |_| Vec::new(),
            );
            s.open_window("main", "Load", 700, 420).unwrap();
            // The window's own frames are charged once it has drawn; everything
            // charged after that belongs to the terminal and its program.
            let before =
                budget.usage().0 - snapshot(id).unwrap().reserved_bytes as usize - ENTRY_BYTES;
            spawn(
                id,
                &Command {
                    program: "/bin/sh".into(),
                    args: vec![
                        "-c".into(),
                        "i=0; while [ $i -lt 20000 ]; do echo row-$i; i=$((i+1)); done".into(),
                    ],
                    ..Command::default()
                },
            )
            .unwrap();
            let mut peak = 0;
            step_until(&mut s, || {
                let shown = snapshot(id).unwrap();
                peak = peak.max(shown.queued_output);
                *exited.borrow()
            });
            assert!(peak <= transport::OUTPUT as i64);
            let shown = snapshot(id).unwrap();
            assert!(shown.text.contains("row-19999"));
            // Twenty thousand rows that never come back are laid out in the
            // terminal's own cache, which stays within its screen's size.
            let kept = with_entry(id, |entry| entry.layouts.bytes()).unwrap();
            let screen = (shown.rows * shown.cols) as usize;
            assert!(
                kept > 0 && kept <= screen * widget::LAYOUT_BYTES_PER_CELL,
                "{kept}"
            );
            close(id);
            assert_eq!(budget.usage().0, before);
            s.shutdown();
        })
    }
}
