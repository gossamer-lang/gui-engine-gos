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
    fn ctrl_shift_letters_reach_the_window_commands_from_a_focused_terminal() {
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
            s.step().unwrap();
            assert_eq!(
                *ran.borrow(),
                vec!["tab.new".to_string(), "tab.right".to_string()]
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
            assert!(snapshot(id).unwrap().text.contains("row-19999"));
            close(id);
            assert_eq!(budget.usage().0, before);
            s.shutdown();
        })
    }
}
