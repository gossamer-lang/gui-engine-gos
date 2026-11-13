use super::*;

fn options(rows: i64, cols: i64, scrollback: i64) -> Options {
    Options {
        rows,
        cols,
        scrollback,
        byte_limit: 16 * 1024 * 1024,
    }
}

#[cfg(unix)]
fn shell(script: &str) -> Command {
    Command {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        ..Command::default()
    }
}

// Runs `body` with the budget of a running session, where a program waits
// for its view to size its screen before it starts.
#[cfg(unix)]
fn in_session(name: &'static str, body: impl FnOnce(&Budget) + Send + 'static) {
    crate::testing::on_ui_thread(move || {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
        let mut s = crate::app::start(name, &format!("dev.gossamer.gui-terminal-{name}")).unwrap();
        body(&crate::app::effects::current_budget());
        s.shutdown();
    })
}

#[cfg(unix)]
fn pump_until_text(budget: &Budget, id: i64, wanted: &str) {
    while !snapshot(id).unwrap().text.contains(wanted) {
        pump(budget, false);
        std::thread::yield_now();
    }
}

#[cfg(unix)]
#[test]
fn a_program_waiting_for_its_size_starts_at_the_size_resize_sets_and_reads_earlier_input() {
    in_session("sized-by-resize", |budget| {
        let id = create(&options(4, 30, 10), budget.clone()).unwrap();
        spawn(id, &shell("stty size; read x; echo got:$x; read y")).unwrap();
        let waiting = snapshot(id).unwrap();
        assert!(waiting.running);
        assert_eq!(
            waiting.pid, 0,
            "the program started before its size was set"
        );
        input(id, b"early\n").unwrap();
        assert!(spawn(id, &shell("true")).is_err());
        assert!(pump(budget, false).1.is_empty());
        assert_eq!(snapshot(id).unwrap().pid, 0);
        resize(id, 6, 33).unwrap();
        assert_ne!(snapshot(id).unwrap().pid, 0);
        pump_until_text(budget, id, "got:early");
        assert!(snapshot(id).unwrap().text.contains("6 33"));
        close(id);
    })
}

#[cfg(unix)]
#[test]
fn a_program_no_view_sizes_starts_at_the_screens_size_once_its_wait_is_over() {
    in_session("start-wait", |budget| {
        let id = create(&options(5, 31, 10), budget.clone()).unwrap();
        spawn(id, &shell("stty size; read x")).unwrap();
        let due = start_due().unwrap();
        assert!(due > Instant::now());
        with(id, |entry| {
            entry.starting.as_mut().unwrap().due = Instant::now();
            Ok(())
        })
        .unwrap();
        pump_until_text(budget, id, "5 31");
        assert!(with(id, |entry| Ok(entry.starting.is_none())).unwrap());
        close(id);
    })
}

#[cfg(unix)]
#[test]
fn a_program_no_view_sizes_starts_at_the_size_a_view_gave_the_sessions_terminals() {
    in_session("start-like-shown", |budget| {
        // The tab a window shows, sized by its view.
        let shown = create(&options(5, 31, 10), budget.clone()).unwrap();
        with(shown, |entry| {
            follow_view(entry, 7, 33, (0, 0));
            Ok(())
        })
        .unwrap();
        // A tab behind it, which no view sizes before its wait is over.
        let behind = create(&options(5, 31, 10), budget.clone()).unwrap();
        spawn(behind, &shell("stty size; read x")).unwrap();
        with(behind, |entry| {
            entry.starting.as_mut().unwrap().due = Instant::now();
            Ok(())
        })
        .unwrap();
        pump_until_text(budget, behind, "7 33");
        close(behind);
        close(shown);
    })
}

#[cfg(unix)]
#[test]
fn a_program_waiting_for_its_size_is_refused_at_once_when_it_cannot_be_found() {
    in_session("start-refused", |budget| {
        let id = create(&options(4, 30, 10), budget.clone()).unwrap();
        for program in [
            "gui-engine-no-such-program",
            "/nonexistent/program",
            "./nope",
            "/tmp",
        ] {
            let command = Command {
                program: program.into(),
                ..Command::default()
            };
            assert!(spawn(id, &command).is_err(), "{program} was accepted");
        }
        assert!(!snapshot(id).unwrap().running);
        close(id);
    })
}

#[cfg(unix)]
#[test]
fn the_pump_reports_the_foreground_job_and_its_directory_as_they_change() {
    let budget = Budget::new(64 * 1024 * 1024);
    let id = create(&options(4, 40, 10), budget.clone()).unwrap();
    let dir = std::env::temp_dir().canonicalize().unwrap();
    let command = Command {
        cwd: dir.to_string_lossy().into_owned(),
        ..shell("set -m; printf idle; read x; cat; read y")
    };
    spawn(id, &command).unwrap();
    let mut heard = Vec::new();
    let mut pump_until_heard = |wanted: &Event| {
        while !heard.contains(wanted) {
            for message in pump(&budget, false).1 {
                if let UiMsg::Terminal(t, event @ Event::Foreground(..)) = message
                    && t == id
                {
                    heard.push(event);
                }
            }
            std::thread::yield_now();
        }
    };
    let cwd = dir.to_string_lossy().into_owned();
    pump_until_heard(&Event::Foreground(String::new(), cwd.clone()));
    // `cat` prints nothing until it reads a line, which shows it in front.
    input(id, b"\n").unwrap();
    input(id, b"line\n").unwrap();
    // Output written between the shell's fork and its exec of `cat` names
    // the forked shell, so the job may be heard as `sh` on the way.
    pump_until_heard(&Event::Foreground("cat".into(), cwd.clone()));
    assert_eq!(
        heard.first(),
        Some(&Event::Foreground(String::new(), cwd.clone()))
    );
    assert_eq!(
        heard.last(),
        Some(&Event::Foreground("cat".into(), cwd.clone()))
    );
    close(id);
}

#[cfg(unix)]
#[test]
fn a_job_that_writes_nothing_is_reported_once_it_takes_the_foreground() {
    let budget = Budget::new(64 * 1024 * 1024);
    let id = create(&options(4, 40, 10), budget.clone()).unwrap();
    let dir = std::env::temp_dir().canonicalize().unwrap();
    let command = Command {
        cwd: dir.to_string_lossy().into_owned(),
        ..shell("set -m; printf idle; read x; sleep 30")
    };
    spawn(id, &command).unwrap();
    let cwd = dir.to_string_lossy().into_owned();
    let mut heard = Vec::new();
    let mut pump_until_heard = |wanted: &Event| {
        while !heard.contains(wanted) {
            for message in pump(&budget, false).1 {
                if let UiMsg::Terminal(t, event @ Event::Foreground(..)) = message
                    && t == id
                {
                    heard.push(event);
                }
            }
            std::thread::yield_now();
        }
    };
    pump_until_heard(&Event::Foreground(String::new(), cwd.clone()));
    // The echo of the line is the last output: `sleep` writes nothing.
    input(id, b"\n").unwrap();
    pump_until_heard(&Event::Foreground("sleep".into(), cwd.clone()));
    close(id);
}

#[cfg(unix)]
#[test]
fn a_program_that_cannot_start_once_sized_is_reported_from_the_pump() {
    in_session("start-failed", |budget| {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("gui-start-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("vanishing");
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let id = create(&options(4, 30, 10), budget.clone()).unwrap();
        let command = Command {
            program: program.to_str().unwrap().into(),
            ..Command::default()
        };
        spawn(id, &command).unwrap();
        std::fs::remove_file(&program).unwrap();
        resize(id, 6, 33).unwrap();
        let (changed, events) = pump(budget, false);
        assert!(changed);
        assert!(
            events
                .iter()
                .any(|m| matches!(m, UiMsg::Terminal(t, Event::NotStarted(_)) if *t == id)),
            "{events:?}"
        );
        assert!(!snapshot(id).unwrap().running);
        assert!(input(id, b"x").is_err());
        close(id);
        let _ = std::fs::remove_dir_all(&dir);
    })
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
            follow_view(entry, 200, 500, (0, 0));
            Ok(())
        })
        .unwrap();
    }
    let (_, events) = pump(&budget, false);
    let failures: Vec<_> = events
        .iter()
        .filter(|m| matches!(m, UiMsg::Terminal(t, Event::Refused(_)) if *t == id))
        .collect();
    assert_eq!(failures.len(), 1, "{events:?}");
    assert_eq!(snapshot(id).unwrap().rows, 4);
    assert!(pump(&budget, false).1.is_empty());
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

/// Parses `budget`'s terminals until `done`, answering whether any pump
/// that parsed output reported a change while `partial` held.
#[cfg(unix)]
fn pump_until(
    budget: &Budget,
    mut partial: impl FnMut() -> bool,
    mut done: impl FnMut() -> bool,
) -> bool {
    let mut changed_while_partial = false;
    while !done() {
        let (changed, _) = pump(budget, false);
        if changed && partial() {
            changed_while_partial = true;
        }
        std::thread::yield_now();
    }
    changed_while_partial
}

#[cfg(unix)]
#[test]
fn a_held_pump_leaves_the_output_wake_for_the_frame_that_takes_it() {
    let budget = Budget::new(64 * 1024 * 1024);
    let id = create(&options(4, 30, 10), budget.clone()).unwrap();
    spawn(
        id,
        &Command {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "printf shown; read x".into()],
            ..Command::default()
        },
    )
    .unwrap();
    let pending = || {
        with(id, |e| {
            Ok(e.transport.as_ref().is_some_and(|t| t.has_pending_output()))
        })
        .unwrap()
    };
    while !pending() {
        std::thread::yield_now();
    }
    // A frame waits: the wake stays taken, so output does not wake the loop.
    pump(&budget, true);
    assert!(pending());
    // The frame takes it, and later output wakes the loop again.
    pump(&budget, false);
    assert!(!pending());
    assert!(snapshot(id).unwrap().text.contains("shown"));
    close(id);
}

#[cfg(unix)]
#[test]
fn a_synchronized_frame_is_shown_once_the_program_ends_it() {
    let budget = Budget::new(64 * 1024 * 1024);
    let id = create(&options(4, 30, 10), budget.clone()).unwrap();
    let dir = std::env::temp_dir().join(format!("gui-sync-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let gate = dir.join("gate");
    let path = std::ffi::CString::new(gate.to_str().unwrap()).unwrap();
    // SAFETY: `path` is a NUL-terminated path in a directory this test owns.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let script = format!(
        "printf '\\033[?2026hpartial'; read x < '{}'; printf ' whole\\033[?2026l'; read y",
        gate.display()
    );
    spawn(
        id,
        &Command {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script],
            ..Command::default()
        },
    )
    .unwrap();
    let text = || snapshot(id).unwrap().text;
    let held = pump_until(
        &budget,
        || !text().contains("whole"),
        || text().contains("partial"),
    );
    assert!(!held, "a frame still open was reported as a change");
    std::fs::write(&gate, "\n").unwrap();
    // The transport parses the frame's end; the next pump reports it.
    while !text().contains("whole") {
        std::thread::yield_now();
    }
    assert!(
        pump(&budget, false).0,
        "the ended frame was not reported as a change"
    );
    close(id);
    let _ = std::fs::remove_dir_all(&dir);
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
                // Each batch is painted, as output that arrives across frames is.
                s.windows[0].dirty = true;
                s.flush().unwrap();
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
            s.windows[0].dirty = true;
            s.flush().unwrap();
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
            // A link is found where the screen shows it, so paint the screen.
            s.windows[0].dirty = true;
            s.flush().unwrap();
            assert_eq!(with_entry(id, |e| e.links.len()), Some(1));
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
            // The first click also gives the view keyboard focus.
            assert_eq!(
                *events.borrow(),
                [
                    Event::LinkPointed(link.clone()),
                    Event::Focused,
                    Event::LinkActivated(link)
                ]
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
            // Output is parsed as the reader takes it; none waits in a queue.
            assert_eq!(peak, 0);
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

    #[test]
    fn a_program_waiting_for_its_view_starts_at_the_size_the_view_gives_it() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::app::start("Terminal", "dev.gossamer.gui-terminal-sized").unwrap();
            let id = create(&options(8, 40, 10), crate::app::effects::current_budget()).unwrap();
            s.set_shell(
                |_| {},
                move |_| Widget::Terminal(widget::View::new("term", id, "Shell")),
                |_| Vec::new(),
            );
            spawn(id, &shell("printf 'born:'; stty size; read x")).unwrap();
            assert_eq!(snapshot(id).unwrap().pid, 0);
            s.open_window("main", "Terminal", 420, 240).unwrap();
            step_until(&mut s, || snapshot(id).unwrap().text.contains("born:"));
            step_until(&mut s, || {
                let text = snapshot(id).unwrap().text;
                text.lines()
                    .any(|l| l.starts_with("born:") && l.trim_end().len() > 5)
            });
            let shown = snapshot(id).unwrap();
            assert_ne!(
                (shown.rows, shown.cols),
                (8, 40),
                "the view kept the opening size"
            );
            let born = format!("born:{} {}", shown.rows, shown.cols);
            assert!(shown.text.contains(&born), "{born} not in {}", shown.text);
            s.shutdown();
        })
    }

    /// Runs `script` in a terminal shown in a session window, focused, once
    /// it has printed "ready"; `events` collects what the terminal reports.
    fn shown_program(name: &str, script: &str) -> (crate::Session, i64, Rc<RefCell<Vec<Event>>>) {
        shown_with(name, script, widget::ViewOptions::default())
    }

    fn shown_with(
        name: &str,
        script: &str,
        view_options: widget::ViewOptions,
    ) -> (crate::Session, i64, Rc<RefCell<Vec<Event>>>) {
        let mut s = crate::app::start(name, &format!("dev.gossamer.gui-terminal-{name}")).unwrap();
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
                let mut view = widget::View::new("term", id, "Shell");
                view.options = view_options.clone();
                Widget::Terminal(view)
            },
            |_| Vec::new(),
        );
        s.open_window("main", "Terminal", 420, 240).unwrap();
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
        (s, id, events)
    }

    #[test]
    fn a_large_paste_reaches_the_program_whole_and_typing_follows_it() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
            // The program reads nothing until the gate opens, so the paste
            // is still going out when the typing arrives behind it.
            let dir = std::env::temp_dir().join(format!("gui-paste-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let gate = dir.join("gate");
            let path = std::ffi::CString::new(gate.to_str().unwrap()).unwrap();
            // SAFETY: `path` is a NUL-terminated path in a directory this test owns.
            assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            let script = format!(
                "stty raw -echo; printf 'ready\\r\\n'; read g < '{}'; \
                 v=$(head -c 300000 | tail -c 5); printf 'got:%s\\r\\n' \"$v\"; read x",
                gate.display()
            );
            let (mut s, id, _) = shown_program("large-paste", &script);
            paste(id, &"a".repeat(299_995)).unwrap();
            assert!(pasting(id));
            input(id, b"XYZ12").unwrap();
            std::fs::write(&gate, "\n").unwrap();
            step_until(&mut s, || snapshot(id).unwrap().text.contains("got:XYZ12"));
            assert!(!pasting(id));
            close(id);
            s.shutdown();
            let _ = std::fs::remove_dir_all(&dir);
        })
    }

    // The pixels where frames `a` and `b` differ, as (left, top, right, bottom).
    fn changed(
        a: &crate::render::Canvas,
        b: &crate::render::Canvas,
    ) -> Option<(i64, i64, i64, i64)> {
        let mut bounds: Option<(i64, i64, i64, i64)> = None;
        for (i, (p, q)) in a.pixels.chunks(4).zip(b.pixels.chunks(4)).enumerate() {
            if p != q {
                let (x, y) = (i as i64 % a.width, i as i64 / a.width);
                bounds = Some(match bounds {
                    None => (x, y, x, y),
                    Some((l, t, r, b)) => (l.min(x), t.min(y), r.max(x), b.max(y)),
                });
            }
        }
        bounds
    }

    fn pixel(c: &crate::render::Canvas, x: i64, y: i64) -> &[u8] {
        let at = ((y * c.width + x) * 4) as usize;
        &c.pixels[at..at + 4]
    }

    #[test]
    fn hiding_and_showing_a_focused_window_keeps_its_cursor_and_losing_focus_hollows_it() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
            let steady = widget::ViewOptions {
                blink: false,
                ..widget::ViewOptions::default()
            };
            let (mut s, id, _) = shown_with("cursor-focus", "printf ready; read x", steady);
            let window = s.windows[0].native.id();
            s.inject(&windowing::Event::FocusGained { window }).unwrap();
            let focused = s.capture().unwrap();
            // macOS reports a hidden window occluded and a shown one exposed,
            // while its keyboard focus stays with it.
            s.inject(&windowing::Event::Occluded { window }).unwrap();
            s.inject(&windowing::Event::RedrawRequested { window })
                .unwrap();
            let shown = s.capture().unwrap();
            assert_eq!(changed(&focused, &shown), None, "the cursor changed");
            s.inject(&windowing::Event::FocusLost { window }).unwrap();
            let unfocused = s.capture().unwrap();
            let (left, top, right, bottom) =
                changed(&focused, &unfocused).expect("losing focus left the cursor as it was");
            let middle = pixel(&unfocused, (left + right) / 2, (top + bottom) / 2);
            assert_ne!(pixel(&unfocused, left, top), middle, "no hollow cursor");
            s.inject(&windowing::Event::FocusGained { window }).unwrap();
            assert_eq!(changed(&focused, &s.capture().unwrap()), None);
            close(id);
            s.shutdown();
        })
    }

    #[test]
    fn a_blinking_cursor_blinks_while_its_window_has_focus() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
            let (mut s, id, _) = shown_program("cursor-blink", "printf ready; read x");
            let window = s.windows[0].native.id();
            s.inject(&windowing::Event::FocusGained { window }).unwrap();
            let blink = s.windows[0].theme.motion.caret_blink;
            let on = s.capture().unwrap();
            s.advance(blink).unwrap();
            let off = s.capture().unwrap();
            assert!(changed(&on, &off).is_some(), "the cursor did not blink");
            s.advance(blink).unwrap();
            assert_eq!(changed(&on, &s.capture().unwrap()), None);
            close(id);
            s.shutdown();
        })
    }

    #[test]
    fn an_untouched_cursor_rests_lit_until_output_arrives() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
            let (mut s, id, _) =
                shown_program("cursor-rest", "printf ready; read x; printf more; read y");
            let window = s.windows[0].native.id();
            s.inject(&windowing::Event::FocusGained { window }).unwrap();
            let blink = s.windows[0].theme.motion.caret_blink;
            let lit = s.capture().unwrap();
            s.advance(crate::app::CARET_REST_MS + 2 * blink).unwrap();
            let rested = s.capture().unwrap();
            assert_eq!(
                changed(&lit, &rested),
                None,
                "the resting cursor is not lit"
            );
            s.advance(blink).unwrap();
            assert_eq!(
                changed(&rested, &s.capture().unwrap()),
                None,
                "the cursor kept blinking"
            );
            assert!(s.windows[0].caret_resting);

            input(id, b"\n").unwrap();
            while !snapshot(id).unwrap().text.contains("more") {
                s.inject(&windowing::Event::Wake).unwrap();
                std::thread::yield_now();
            }
            s.inject(&windowing::Event::Wake).unwrap();
            assert!(
                !s.windows[0].caret_resting,
                "output did not wake the cursor"
            );
            let awake = s.capture().unwrap();
            s.advance(blink).unwrap();
            assert!(
                changed(&awake, &s.capture().unwrap()).is_some(),
                "the cursor did not blink again"
            );
            close(id);
            s.shutdown();
        })
    }

    #[test]
    fn focus_wakes_a_resting_cursor() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
            let (mut s, id, _) = shown_program("cursor-focus", "printf ready; read x");
            let window = s.windows[0].native.id();
            s.inject(&windowing::Event::FocusGained { window }).unwrap();
            let blink = s.windows[0].theme.motion.caret_blink;
            s.advance(crate::app::CARET_REST_MS + 2 * blink).unwrap();
            assert!(s.windows[0].caret_resting);
            s.inject(&windowing::Event::FocusLost { window }).unwrap();
            s.inject(&windowing::Event::FocusGained { window }).unwrap();
            assert!(!s.windows[0].caret_resting, "focus did not wake the cursor");
            close(id);
            s.shutdown();
        })
    }

    #[test]
    fn turning_blinking_on_in_a_focused_terminal_starts_the_blink() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
            let mut s =
                crate::app::start("Blink", "dev.gossamer.gui-terminal-blink-toggle").unwrap();
            let id = create(&options(8, 40, 100), crate::app::effects::current_budget()).unwrap();
            let blinking = Rc::new(std::cell::Cell::new(false));
            let asked = blinking.clone();
            s.set_shell(
                |_| {},
                move |_| {
                    let mut view = widget::View::new("term", id, "Shell");
                    view.options.blink = asked.get();
                    Widget::Terminal(view)
                },
                |_| Vec::new(),
            );
            s.open_window("main", "Terminal", 420, 240).unwrap();
            spawn(
                id,
                &Command {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), "printf ready; read x".into()],
                    ..Command::default()
                },
            )
            .unwrap();
            assert!(s.focus("term").unwrap());
            step_until(&mut s, || snapshot(id).unwrap().text.contains("ready"));
            let window = s.windows[0].native.id();
            s.inject(&windowing::Event::FocusGained { window }).unwrap();
            let blink = s.windows[0].theme.motion.caret_blink;
            blinking.set(true);
            s.rebuild(0);
            let on = s.capture().unwrap();
            s.advance(blink).unwrap();
            assert!(
                changed(&on, &s.capture().unwrap()).is_some(),
                "the cursor did not blink"
            );
            close(id);
            s.shutdown();
        })
    }

    #[test]
    fn escape_stops_a_paste_and_still_ends_it() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
            let dir = std::env::temp_dir().join(format!("gui-paste-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let reader = dir.join("reader.py");
            // Counts what arrives before the paste's closing mark.
            std::fs::write(
                &reader,
                "import os, sys, time, tty\n\
                 tty.setraw(0)\n\
                 sys.stdout.write('\\x1b[?2004hready\\r\\n'); sys.stdout.flush()\n\
                 data = b''\n\
                 while b'\\x1b[201~' not in data:\n    data += os.read(0, 65536)\n\
                 sys.stdout.write('got:%d\\r\\n' % data.index(b'\\x1b[201~')); sys.stdout.flush()\n\
                 time.sleep(30)\n",
            )
            .unwrap();
            let (mut s, id, _) = shown_program(
                "paste-cancel",
                &format!("exec python3 {}", reader.display()),
            );
            let total = 8 * 1024 * 1024;
            paste(id, &"b".repeat(total)).unwrap();
            input(id, b"\x1b").unwrap();
            step_until(&mut s, || snapshot(id).unwrap().text.contains("got:"));
            let text = snapshot(id).unwrap().text;
            let got: usize = text
                .split("got:")
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|n| n.parse().ok())
                .unwrap();
            assert!(got < total, "{got}");
            assert!(!pasting(id));
            close(id);
            s.shutdown();
            let _ = std::fs::remove_dir_all(&dir);
        })
    }

    #[test]
    fn shift_enter_is_its_own_key_once_a_program_asks_for_kittys_protocol() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
            let shift = windowing::Mods {
                shift: true,
                ..windowing::Mods::default()
            };
            // Classic encoding sends Shift+Enter as Enter, a carriage return
            // the tty reads as a newline.
            let (mut s, id, _) = shown_program("classic-keys", &reading("", 1));
            s.press(windowing::Key::Return, shift).unwrap();
            step_until(&mut s, || snapshot(id).unwrap().text.contains("got:"));
            let text = snapshot(id).unwrap().text;
            assert!(text.contains("got:0a"), "{text}");
            close(id);
            s.shutdown();
            let (mut s, id, _) = shown_program("kitty-keys", &reading("\\033[>1u", 7));
            s.press(windowing::Key::Return, shift).unwrap();
            step_until(&mut s, || snapshot(id).unwrap().text.contains("got:"));
            let text = snapshot(id).unwrap().text;
            assert!(text.contains("got:1b5b31333b3275"), "{text}");
            close(id);
            s.shutdown();
        })
    }

    #[test]
    fn typing_after_a_program_leaves_kittys_protocol_reaches_the_shell_whole() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
            let ctrl = windowing::Mods {
                ctrl: true,
                ..windowing::Mods::default()
            };
            // The program asks for kitty's protocol, reads Ctrl+C as its
            // escape code (CSI 99;5u, 7 bytes), gives the protocol back, and
            // reports the first byte typed after that.
            let script = "stty -icanon -echo min 1; printf '\\033[>1uready\\r\\n'; \
                 dd bs=1 count=7 >/dev/null 2>&1; printf '\\033[<uback\\r\\n'; \
                 v=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'); \
                 printf 'got:%s\\r\\n' \"$v\"; read x";
            let (mut s, id, _) = shown_program("kitty-exit", script);
            s.press(windowing::Key::C, ctrl).unwrap();
            step_until(&mut s, || snapshot(id).unwrap().text.contains("back"));
            s.press(windowing::Key::L, windowing::Mods::default())
                .unwrap();
            s.type_text("l").unwrap();
            s.type_text("m").unwrap();
            step_until(&mut s, || snapshot(id).unwrap().text.contains("got:"));
            let text = snapshot(id).unwrap().text;
            assert!(text.contains("got:6c"), "{text}");
            close(id);
            s.shutdown();
        })
    }

    fn keys(s: &mut crate::Session, typed: &str) {
        for c in typed.chars() {
            let (key, shift) = match c {
                '$' => (windowing::Key::Num4, true),
                '0'..='9' => (
                    [
                        windowing::Key::Num0,
                        windowing::Key::Num1,
                        windowing::Key::Num2,
                        windowing::Key::Num3,
                        windowing::Key::Num4,
                        windowing::Key::Num5,
                        windowing::Key::Num6,
                        windowing::Key::Num7,
                        windowing::Key::Num8,
                        windowing::Key::Num9,
                    ][c as usize - '0' as usize],
                    false,
                ),
                'A'..='Z' => (windowing::Key::Char(c.to_ascii_lowercase()), true),
                _ => (windowing::Key::Char(c), false),
            };
            let key = match key {
                windowing::Key::Char(l) if l.is_ascii_lowercase() => [
                    windowing::Key::A,
                    windowing::Key::B,
                    windowing::Key::C,
                    windowing::Key::D,
                    windowing::Key::E,
                    windowing::Key::F,
                    windowing::Key::G,
                    windowing::Key::H,
                    windowing::Key::I,
                    windowing::Key::J,
                    windowing::Key::K,
                    windowing::Key::L,
                    windowing::Key::M,
                    windowing::Key::N,
                    windowing::Key::O,
                    windowing::Key::P,
                    windowing::Key::Q,
                    windowing::Key::R,
                    windowing::Key::S,
                    windowing::Key::T,
                    windowing::Key::U,
                    windowing::Key::V,
                    windowing::Key::W,
                    windowing::Key::X,
                    windowing::Key::Y,
                    windowing::Key::Z,
                ][(l as u8 - b'a') as usize],
                other => other,
            };
            let mods = windowing::Mods {
                shift,
                ..windowing::Mods::default()
            };
            s.press(key, mods).unwrap();
        }
    }

    #[test]
    fn copy_mode_moves_selects_and_copies_across_the_history() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
            let script = "i=0; while [ $i -lt 20 ]; do echo line $i; i=$((i+1)); done; \
                          printf 'alpha beta\\r\\ngamma delta\\r\\nready\\r\\n'; read x";
            let (mut s, id, _) = shown_program("copy-mode", script);
            copy_mode(id).unwrap();
            assert_eq!(mode(id), "copy");
            // Up to "gamma delta", from its second word to its end.
            keys(&mut s, "kk0wv$y");
            assert_eq!(crate::services::clipboard::text(), "delta");
            assert_eq!(mode(id), "");
            // From the oldest row of the history down past a screen.
            copy_mode(id).unwrap();
            keys(&mut s, "gvjjjjjjjjj$y");
            let copied = crate::services::clipboard::text();
            let lines: Vec<&str> = copied.lines().collect();
            assert_eq!(lines.first(), Some(&"line 0"), "{copied}");
            assert_eq!(lines.len(), 10, "{copied}");
            assert_eq!(lines.last(), Some(&"line 9"), "{copied}");
            close(id);
            s.shutdown();
        })
    }

    #[test]
    fn hints_open_or_copy_the_link_whose_label_is_typed() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
            let script =
                "printf 'https://e.test/a x\\r\\nhttp://b.test/y\\r\\nready\\r\\n'; read x";
            let (mut s, id, events) = shown_program("hints", script);
            s.flush().unwrap();
            assert_eq!(hints(id, true).unwrap(), 2);
            assert_eq!(mode(id), "hints");
            keys(&mut s, "s");
            assert_eq!(crate::services::clipboard::text(), "http://b.test/y");
            assert_eq!(mode(id), "");
            assert_eq!(hints(id, false).unwrap(), 2);
            keys(&mut s, "a");
            assert!(
                events
                    .borrow()
                    .contains(&Event::LinkActivated("https://e.test/a".to_string())),
                "{:?}",
                events.borrow()
            );
            close(id);
            s.shutdown();
        })
    }

    // Reads `n` bytes the program receives and prints them in hex after
    // "got:", with the terminal in raw input so no line is waited for.
    fn reading(setup: &str, n: usize) -> String {
        format!(
            "stty -icanon -echo min 1; printf '{setup}ready\\r\\n'; \
             v=$(dd bs=1 count={n} 2>/dev/null | od -An -tx1 | tr -d ' \\n'); \
             printf 'got:%s\\r\\n' \"$v\"; read x"
        )
    }

    // The window point at the middle of cell (`row`, `col`) of the view.
    fn cell_point(row: i64, col: i64) -> (f64, f64) {
        let view = widget::View::new("term", 0, "Shell");
        let theme = crate::style::Theme::default();
        let (cw, lh, _) = widget::metrics(&view, &theme);
        let pad = theme.px(widget::PAD) as f64;
        (pad + (col as f64 + 0.5) * cw, pad + (row as f64 + 0.5) * lh)
    }

    fn button(s: &mut crate::Session, (x, y): (f64, f64), clicks: i64) {
        let window = s.windows[0].native.id();
        let source = windowing::PointerSource::Mouse;
        let button = windowing::MouseButton::Left;
        s.inject(&windowing::Event::ButtonDown {
            window,
            button,
            x,
            y,
            clicks,
            source,
        })
        .unwrap();
        s.inject(&windowing::Event::ButtonUp {
            window,
            button,
            x,
            y,
            clicks,
            source,
        })
        .unwrap();
    }

    #[test]
    fn a_double_click_selects_a_word_and_copy_on_select_copies_it() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            crate::services::clipboard::use_memory(true);
            let script = "printf 'ready\\r\\nsee /usr/local/bin now\\r\\n'; read x";
            let options = widget::ViewOptions {
                copy_on_select: true,
                ..widget::ViewOptions::default()
            };
            let (mut s, id, _) = shown_with("word", script, options);
            step_until(&mut s, || snapshot(id).unwrap().text.contains("now"));
            button(&mut s, cell_point(1, 9), 2);
            assert_eq!(copy(id).unwrap(), "/usr/local/bin");
            assert_eq!(crate::services::clipboard::text(), "/usr/local/bin");
            // A double click on a blank selects nothing new.
            button(&mut s, cell_point(1, 3), 2);
            assert_eq!(crate::services::clipboard::text(), "/usr/local/bin");
            s.shutdown();
        })
    }

    #[test]
    fn a_paste_of_several_lines_waits_for_the_application_and_a_dropped_file_is_quoted() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            crate::services::clipboard::use_memory(true);
            let options = widget::ViewOptions {
                confirm_paste: true,
                ..widget::ViewOptions::default()
            };
            let (mut s, id, events) = shown_with("held", &reading("", 10), options);
            crate::services::clipboard::set_text("rm -rf a\nb");
            let chord = windowing::Mods {
                ctrl: true,
                shift: true,
                ..windowing::Mods::default()
            };
            s.press(windowing::Key::V, chord).unwrap();
            assert!(
                events
                    .borrow()
                    .contains(&Event::PasteHeld("rm -rf a\nb".to_string())),
                "{:?}",
                events.borrow()
            );
            let window = s.windows[0].native.id();
            let (x, y) = cell_point(2, 2);
            s.inject(&windowing::Event::DroppedFile {
                window,
                path: "/tmp/a b".to_string(),
                x,
                y,
            })
            .unwrap();
            step_until(&mut s, || snapshot(id).unwrap().text.contains("got:"));
            // The path arrives quoted for the shell.
            let shown = snapshot(id).unwrap().text;
            assert!(shown.contains("got:272f746d702f61206227"), "{shown}");
            s.shutdown();
        })
    }

    #[test]
    fn a_bound_ctrl_chord_reaches_the_window_and_ctrl_letters_reach_the_program() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::app::start("chords", "dev.gossamer.gui-terminal-chords").unwrap();
            let id = create(&options(8, 40, 100), crate::app::effects::current_budget()).unwrap();
            let commands = Rc::new(RefCell::new(Vec::new()));
            let heard = commands.clone();
            s.set_shell(
                move |m| {
                    if let UiMsg::Command(c) = m {
                        heard.borrow_mut().push(c);
                    }
                },
                move |_| Widget::Terminal(widget::View::new("term", id, "Shell")),
                |_| {
                    vec![
                        crate::commands::Command::new("view.zoom-in", "Zoom In").key("Ctrl+="),
                        crate::commands::Command::new("edit.copy", "Copy").key("Ctrl+B"),
                    ]
                },
            );
            s.open_window("main", "Terminal", 420, 240).unwrap();
            spawn(
                id,
                &Command {
                    program: "/bin/sh".into(),
                    args: vec!["-c".into(), reading("", 1)],
                    ..Command::default()
                },
            )
            .unwrap();
            assert!(s.focus("term").unwrap());
            step_until(&mut s, || snapshot(id).unwrap().text.contains("ready"));
            let ctrl = windowing::Mods {
                ctrl: true,
                ..windowing::Mods::default()
            };
            s.press(windowing::Key::Char('='), ctrl).unwrap();
            assert_eq!(*commands.borrow(), vec!["view.zoom-in".to_string()]);
            s.press(windowing::Key::B, ctrl).unwrap();
            step_until(&mut s, || snapshot(id).unwrap().text.contains("got:"));
            let shown = snapshot(id).unwrap().text;
            assert!(shown.contains("got:02"), "{shown}");
            assert_eq!(commands.borrow().len(), 1);
            s.shutdown();
        })
    }

    #[test]
    fn a_program_that_asks_for_the_mouse_hears_a_click_as_an_sgr_report() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let script = reading("\\033[?1000h\\033[?1006h", 5);
            let (mut s, id, _) = shown_program("mouse", &script);
            s.click_at(60, 40).unwrap();
            step_until(&mut s, || snapshot(id).unwrap().text.contains("got:"));
            let shown = snapshot(id).unwrap().text;
            assert!(shown.contains("got:1b5b3c303b"), "{shown}");
            assert_eq!(snapshot(id).unwrap().selection, None);
            s.shutdown();
        })
    }

    #[test]
    fn the_wheel_on_the_alternate_screen_sends_cursor_keys_to_a_program_without_the_mouse() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let script = reading("\\033[?1049h", 3);
            let (mut s, id, _) = shown_program("wheel", &script);
            s.scroll_at(60, 40, -1.0).unwrap();
            step_until(&mut s, || snapshot(id).unwrap().text.contains("got:"));
            let shown = snapshot(id).unwrap().text;
            assert!(shown.contains("got:1b5b42"), "{shown}");
            s.shutdown();
        })
    }

    #[test]
    fn f11_reaches_the_program_when_no_window_command_binds_it() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let (mut s, id, _) = shown_program("f11", &reading("", 5));
            s.press(windowing::Key::F11, windowing::Mods::default())
                .unwrap();
            // The program reads five bytes: F11's sequence, or this text in
            // its place were F11 withheld.
            s.type_text("xxxxx").unwrap();
            step_until(&mut s, || snapshot(id).unwrap().text.contains("got:"));
            let shown = snapshot(id).unwrap().text;
            assert!(shown.contains("got:1b5b32337e"), "{shown}");
            s.shutdown();
        })
    }

    #[test]
    fn clipboard_writes_and_notifications_reach_the_application_as_events() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let script = "printf 'ready\\r\\n\\033]52;c;Y29waWVk\\007\\033]52;p;cHJpbWFyeQ==\\007\\033]9;done\\007'; read x";
            let (mut s, _, events) = shown_program("osc", script);
            let wanted = [
                Event::Clipboard("copied".into()),
                Event::PrimarySelection("primary".into()),
                Event::Notify(String::new(), "done".into()),
            ];
            step_until(&mut s, || {
                let heard = events.borrow();
                wanted.iter().all(|w| heard.contains(w))
            });
            s.shutdown();
        })
    }
}

/// A red sixel `width` by `bands * 6` pixels.
fn red_sixel(width: usize, bands: usize) -> Vec<u8> {
    let band = format!("#1!{width}~-");
    format!("\x1bPq#1;2;100;0;0{}\x1b\\", band.repeat(bands)).into_bytes()
}

fn paint(view: &widget::View, size: crate::geometry::Size) -> crate::render::Canvas {
    let theme = crate::style::Theme::default();
    let states = crate::widgets::state::StateStore::new();
    let mut cx = crate::widgets::cx::PaintCx::new(
        &crate::render::Canvas::new(size.width, size.height),
        &theme,
        &states,
    );
    widget::paint_terminal(view, &crate::widgets::cx::Node::leaf(size), &mut cx);
    cx.canvas
}

fn pixel(canvas: &crate::render::Canvas, x: i64, y: i64) -> [u8; 4] {
    let at = ((y * canvas.width + x) * 4) as usize;
    [
        canvas.pixels[at],
        canvas.pixels[at + 1],
        canvas.pixels[at + 2],
        canvas.pixels[at + 3],
    ]
}

#[test]
fn a_sixel_paints_its_cells_and_is_charged_while_the_screen_shows_it() {
    let budget = Budget::new(64 * 1024 * 1024);
    let id = create(&options(10, 40, 100), budget.clone()).unwrap();
    let view = widget::View::new("term", id, "Images");
    let size = crate::geometry::Size::new(480, 320);
    // The first paint sizes the screen and the pixels of its cells.
    paint(&view, size);
    let (_, cell) = with(id, |entry| Ok((entry.engine.size(), entry.engine.cell()))).unwrap();
    assert!(cell.0 > 0 && cell.1 > 0);
    let before = snapshot(id).unwrap().reserved_bytes;
    feed(id, &red_sixel(40, 7)).unwrap();
    let canvas = paint(&view, size);
    let pad = crate::style::Theme::default().px(widget::PAD);
    assert_eq!(pixel(&canvas, pad + 3, pad + 3), [255, 0, 0, 255]);
    let held = snapshot(id).unwrap().reserved_bytes - before;
    assert_eq!(held, 40 * 42 * 4);
    // Clearing the screen and its history lets the image go.
    feed(id, b"\x1b[2J\x1b[3J").unwrap();
    let canvas = paint(&view, size);
    assert_ne!(pixel(&canvas, pad + 3, pad + 3), [255, 0, 0, 255]);
    assert_eq!(snapshot(id).unwrap().reserved_bytes, before);
    close(id);
}

#[test]
fn image_strings_reach_the_screen_and_other_strings_do_not() {
    let budget = Budget::new(64 * 1024 * 1024);
    let id = create(&options(10, 40, 100), budget).unwrap();
    let view = widget::View::new("term", id, "Images");
    paint(&view, crate::geometry::Size::new(480, 320));
    let images = |id| with(id, |entry| Ok(entry.engine.frame()?.images().len())).unwrap();
    // A 2x2 red kitty image sent as RGBA.
    feed(id, b"\x1b_Ga=T,f=32,s=2,v=2;/wAA//8AAP//AAD//wAA/w==\x1b\\").unwrap();
    assert!(images(id) > 0);
    feed(id, b"\x1b[2J\x1b[3J\x1b[H").unwrap();
    // An 11x11 PNG sent as an iTerm2 file.
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAsAAAALCAYAAACprHcmAAAACXBIWXMAAAGKAAABigEzlzBYAAAAOUlEQVQYlZXOwQ0AMAzCQEdi7yaT0xWAN7JuDCac2PQKYxflycOoICOKtPIuqFCg4/LzKxiz6xjyAYh9DR1sLUN1AAAAAElFTkSuQmCC";
    feed(id, format!("\x1b]1337;File=inline=1:{png}\x07").as_bytes()).unwrap();
    assert!(images(id) > 0);
    feed(id, b"\x1b[2J\x1b[3J\x1b[H").unwrap();
    // SOS and PM payloads never reach the parser.
    feed(id, b"\x1bXhidden\x1b\\\x1b^hidden\x1b\\shown").unwrap();
    let text = snapshot(id).unwrap().text;
    assert!(text.contains("shown") && !text.contains("hidden"));
    close(id);
}

#[test]
fn a_terminal_that_settles_has_its_history_compacted_off_the_session_thread() {
    let budget = Budget::new(512 * 1024 * 1024);
    let id = create(
        &Options {
            rows: 24,
            cols: 80,
            scrollback: 10_000,
            byte_limit: 64 * 1024 * 1024,
        },
        budget.clone(),
    )
    .unwrap();
    let mut out = Vec::new();
    for n in 0..5_000 {
        out.extend(format!("\x1b[3{}mrow {n} of the history\x1b[0m\r\n", n % 8).bytes());
    }
    for chunk in out.chunks(512 * 1024) {
        feed(id, chunk).unwrap();
    }
    let bytes = || with(id, |entry| Ok(entry.engine.history_bytes())).unwrap();
    let asked = || with(id, |entry| Ok(entry.compacted)).unwrap();
    let before = bytes();
    // Output the session has not pumped, or not shown, holds the compactor
    // back.
    before_wait(true);
    assert_eq!(asked(), 0);
    pump(&budget, false);
    before_wait(false);
    assert_eq!(asked(), 0);
    before_wait(true);
    assert_ne!(asked(), 0);
    while bytes() >= before * 3 / 4 {
        std::thread::yield_now();
    }
    assert!(snapshot(id).unwrap().text.contains("row 4999"));
    close(id);
}

#[test]
fn a_long_history_opens_within_its_byte_limit_and_keeps_the_newest_rows() {
    let budget = Budget::new(512 * 1024 * 1024);
    let limit = 64 * 1024 * 1024;
    let id = create(
        &Options {
            rows: 36,
            cols: 82,
            scrollback: 100_000,
            byte_limit: limit,
        },
        budget.clone(),
    )
    .unwrap();
    assert!(snapshot(id).unwrap().reserved_bytes <= limit);
    let mut out = Vec::new();
    for n in 0..30_000 {
        out.extend(format!("\x1b[3{}mrow {n}\x1b[0m\r\n", n % 8).bytes());
    }
    for chunk in out.chunks(512 * 1024) {
        feed(id, chunk).unwrap();
    }
    with(id, |entry| {
        entry.engine.compact_history();
        Ok(())
    })
    .unwrap();
    let shown = snapshot(id).unwrap();
    assert_eq!(shown.history, 30_000 - 35);
    assert!(shown.text.contains("row 29999"), "{}", shown.text);
    close(id);
    // A screen larger than the limit is still refused.
    let small = Options {
        rows: 256,
        cols: 512,
        scrollback: 0,
        byte_limit: 1024 * 1024,
    };
    assert_eq!(
        create(&small, budget).unwrap_err(),
        "terminal screen exceeds its byte budget"
    );
}

#[test]
fn saved_text_joins_wrapped_rows_back_into_their_lines() {
    let budget = Budget::new(64 * 1024 * 1024);
    let id = create(&options(4, 20, 50), budget).unwrap();
    let long = "x".repeat(45);
    feed(id, format!("first line\r\n{long}\r\nlast").as_bytes()).unwrap();
    let path = std::env::temp_dir().join(format!("gui-save-{}.txt", std::process::id()));
    let path = path.to_str().unwrap();
    let written = save_text(id, path, false).unwrap();
    let saved = std::fs::read_to_string(path).unwrap();
    assert_eq!(saved, format!("first line\n{long}\nlast\n"));
    assert_eq!(written, saved.len() as i64);
    // The first line is in the history now; the selection is on the screen.
    select(id, Some((Cell { row: 3, col: 0 }, Cell { row: 3, col: 4 }))).unwrap();
    save_text(id, path, true).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "last");
    assert!(save_text(id, "/nonexistent/dir/file.txt", false).is_err());
    let _ = std::fs::remove_file(path);
    close(id);
}

// A view painted as a window paints it, following its last retained frame.
fn paint_retained(
    view: &widget::View,
    size: crate::geometry::Size,
    retained: &mut crate::render::retained::Retained,
) -> crate::render::retained::Scene {
    let theme = crate::style::Theme::default();
    let states = crate::widgets::state::StateStore::new();
    let recording = retained.recording(size.width, size.height);
    let mut cx = crate::widgets::cx::PaintCx::new(&recording, &theme, &states);
    widget::paint_terminal(view, &crate::widgets::cx::Node::leaf(size), &mut cx);
    let scene = cx.canvas.take_scene().unwrap();
    retained.commit(scene.clone());
    scene
}

// A view painted into a recording that follows no earlier frame.
fn paint_fresh(view: &widget::View, size: crate::geometry::Size) -> crate::render::retained::Scene {
    let theme = crate::style::Theme::default();
    let states = crate::widgets::state::StateStore::new();
    let budget = crate::render::retained::Budgets::default().scene;
    let recording = crate::render::Canvas::recording(size.width, size.height, budget);
    let mut cx = crate::widgets::cx::PaintCx::new(&recording, &theme, &states);
    widget::paint_terminal(view, &crate::widgets::cx::Node::leaf(size), &mut cx);
    cx.canvas.take_scene().unwrap()
}

#[test]
fn a_repaint_that_copies_every_row_keeps_their_runs_laid_out() {
    let id = create(&options(12, 40, 100), Budget::new(64 * 1024 * 1024)).unwrap();
    let view = widget::View::new("term", id, "Term");
    let mut retained = crate::render::retained::Retained::new();
    let size = crate::geometry::Size::new(480, 320);
    feed(id, b"hello\r\n\x1b[31mred\x1b[0m world\r\n").unwrap();
    paint_retained(&view, size, &mut retained);
    let laid = with_entry(id, |entry| entry.layouts.bytes()).unwrap();
    assert!(laid > 0);
    paint_retained(&view, size, &mut retained);
    assert_eq!(with_entry(id, |entry| entry.layouts.bytes()).unwrap(), laid);
    close(id);
}

#[test]
fn a_screen_painted_after_its_last_frame_shows_what_a_first_paint_would() {
    // Opaque, the rows draw through the terminal's layer; translucent, into
    // the window's recording, which copies rows from the frame before.
    for opacity in [1.0, 0.9] {
        repaints_match_first_paints(opacity);
    }
}

fn repaints_match_first_paints(opacity: f64) {
    let budget = Budget::new(64 * 1024 * 1024);
    let kept = create(&options(12, 40, 100), budget.clone()).unwrap();
    let fresh = create(&options(12, 40, 100), budget).unwrap();
    let mut kept_view = widget::View::new("kept", kept, "Kept");
    let mut fresh_view = widget::View::new("fresh", fresh, "Fresh");
    kept_view.options.opacity = opacity;
    fresh_view.options.opacity = opacity;
    let mut retained = crate::render::retained::Retained::new();
    let small = crate::geometry::Size::new(480, 320);
    let large = crate::geometry::Size::new(560, 360);
    let mut step = |name: &str, size: crate::geometry::Size, change: &dyn Fn(i64)| {
        change(kept);
        change(fresh);
        // Twice: the second paint follows a frame that already shows it all.
        for _ in 0..2 {
            // A face other tests load meanwhile changes what both draw, so
            // they are compared only when both drew with the same faces.
            loop {
                let faces = crate::text::fonts::generation();
                let scene = paint_retained(&kept_view, size, &mut retained);
                let mut shown = crate::render::Canvas::new(size.width, size.height);
                scene.replay(&mut shown);
                let first = paint(&fresh_view, size);
                let recorded = (opacity < 1.0).then(|| paint_fresh(&fresh_view, size));
                if crate::text::fonts::generation() != faces {
                    continue;
                }
                assert!(shown.pixels == first.pixels, "{name} at opacity {opacity}");
                if let Some(recorded) = recorded {
                    assert_eq!(scene, recorded, "{name}");
                }
                break;
            }
        }
    };
    let output = |bytes: &'static str| move |id: i64| feed(id, bytes.as_bytes()).unwrap();
    step("text", small, &output("hello\r\nworld\r\n"));
    step(
        "colors",
        small,
        &output("\x1b[31mred \x1b[42mgreen\x1b[0m \x1b[1;4mbold\x1b[0m\r\n"),
    );
    step("overwrite", small, &output("\x1b[2;3HX\x1b[12;1H"));
    step("erase line", small, &output("\x1b[1;3H\x1b[K\x1b[12;1H"));
    step(
        "insert and delete",
        small,
        &output("\x1b[2;1H\x1b[2@ab\x1b[3P\x1b[12;1H"),
    );
    step(
        "scroll region",
        small,
        &output("\x1b[2;5r\x1b[5;1Hone\ntwo\nthree\n\x1b[r\x1b[12;1H"),
    );
    step(
        "reverse index",
        small,
        &output("\x1b[2;5r\x1b[2;1H\x1bM\x1bM\x1b[r\x1b[12;1H"),
    );
    step("lines", small, &output("\x1b[3;1H\x1b[2L\x1b[1M\x1b[12;1H"));
    step("history", small, &|id| {
        for i in 0..40 {
            feed(id, format!("line {i}\r\n").as_bytes()).unwrap();
        }
    });
    step("scrolled back", small, &|id| {
        scroll(id, 5).unwrap();
    });
    step("scrolled home", small, &|id| {
        scroll(id, -5).unwrap();
    });
    step("selected", small, &|id| {
        select(id, Some((Cell { row: 2, col: 3 }, Cell { row: 4, col: 7 }))).unwrap();
    });
    step("selection moved", small, &|id| {
        select(id, Some((Cell { row: 3, col: 1 }, Cell { row: 3, col: 9 }))).unwrap();
    });
    step("unselected", small, &|id| select(id, None).unwrap());
    step("highlighted", small, &|id| {
        let span = Span {
            start: Cell { row: 45, col: 0 },
            end: Cell { row: 45, col: 4 },
        };
        set_highlights(id, vec![span]).unwrap();
    });
    step(
        "alternate",
        small,
        &output("\x1b[?1049h\x1b[Hfull screen\x1b[5;5Hprogram"),
    );
    step("primary", small, &output("\x1b[?1049l"));
    step("wide", small, &output("日本語 😀 e\u{301}\r\n"));
    step("cleared", small, &output("\x1b[2J\x1b[Hafter"));
    step("resized", large, &output("wider\r\n"));
}

#[test]
fn scrolled_rows_from_the_layer_reach_a_window_surface_in_its_channel_order() {
    let budget = Budget::new(64 * 1024 * 1024);
    let kept = create(&options(12, 40, 100), budget.clone()).unwrap();
    let fresh = create(&options(12, 40, 100), budget).unwrap();
    let kept_view = widget::View::new("kept", kept, "Kept");
    let fresh_view = widget::View::new("fresh", fresh, "Fresh");
    let size = crate::geometry::Size::new(480, 320);
    let (w, h) = (size.width, size.height);
    let mut retained = crate::render::retained::Retained::new();
    let mut surface = vec![0u8; (w * h * 4) as usize];
    let show = |retained: &mut crate::render::retained::Retained, surface: &mut Vec<u8>| {
        let recording = retained.recording(w, h);
        let theme = crate::style::Theme::default();
        let states = crate::widgets::state::StateStore::new();
        let mut cx = crate::widgets::cx::PaintCx::new(&recording, &theme, &states);
        widget::paint_terminal(&kept_view, &crate::widgets::cx::Node::leaf(size), &mut cx);
        let scene = cx.canvas.take_scene().unwrap();
        let damage = retained.damage(w, h, &scene);
        let target = crate::render::retained::Target {
            pixels: surface,
            width: w,
            height: h,
            stride: w * 4,
            offsets: (2, 1, 0),
        };
        retained.raster(&scene, &damage, target);
        retained.commit(scene);
    };
    for round in 0..6 {
        for i in 0..if round == 0 { 20 } else { round % 2 + 1 } {
            let line = format!(
                "\x1b[3{}mline {round} {i}\x1b[0m \x1b[44mblue\x1b[0m\r\n",
                i % 8
            );
            feed(kept, line.as_bytes()).unwrap();
            feed(fresh, line.as_bytes()).unwrap();
        }
        loop {
            let faces = crate::text::fonts::generation();
            show(&mut retained, &mut surface);
            let first = paint(&fresh_view, size);
            if crate::text::fonts::generation() != faces {
                continue;
            }
            let mut want = first.pixels.clone();
            crate::render::swap_red_blue_in_place(&mut want);
            assert!(surface == want, "round {round}");
            break;
        }
    }
    assert!(with_entry(kept, |entry| entry.layer.holds()).unwrap());
    close(kept);
    close(fresh);
}

#[test]
fn scrolled_output_draws_its_rows_from_the_layer() {
    let id = create(&options(12, 40, 100), Budget::new(64 * 1024 * 1024)).unwrap();
    let view = widget::View::new("term", id, "Term");
    let mut retained = crate::render::retained::Retained::new();
    let size = crate::geometry::Size::new(480, 320);
    for i in 0..30 {
        feed(id, format!("line {i}\r\n").as_bytes()).unwrap();
    }
    paint_retained(&view, size, &mut retained);
    feed(id, b"one more\r\n").unwrap();
    let scene = paint_retained(&view, size, &mut retained);
    use crate::render::retained::Primitive;
    assert!(
        scene
            .commands
            .iter()
            .any(|c| matches!(c.paint, Primitive::Surface(..)))
    );
    assert!(
        !scene
            .commands
            .iter()
            .any(|c| matches!(c.paint, Primitive::Coverage(..))),
        "no glyph draws outside the layer"
    );
    // This terminal's own layer: a sweep of every terminal would reach the
    // layers other tests paint meanwhile.
    assert_eq!(
        with_entry(id, |entry| entry.layer.sweep()),
        Some(true),
        "a layer drawn since the last sweep stays"
    );
    assert_eq!(
        with_entry(id, |entry| entry.layer.sweep()),
        Some(false),
        "a layer no paint drew from goes"
    );
    close(id);
}

#[test]
fn a_layer_goes_once_output_stops_scrolling_and_a_still_screen_takes_none() {
    let id = create(&options(12, 40, 100), Budget::new(64 * 1024 * 1024)).unwrap();
    let view = widget::View::new("term", id, "Term");
    let mut retained = crate::render::retained::Retained::new();
    let size = crate::geometry::Size::new(480, 320);
    let theme = crate::style::Theme::default();
    let states = crate::widgets::state::StateStore::new();
    let mut paint_at = |now: i64| {
        let recording = retained.recording(size.width, size.height);
        let mut cx = crate::widgets::cx::PaintCx::new(&recording, &theme, &states);
        cx.now = now;
        widget::paint_terminal(&view, &crate::widgets::cx::Node::leaf(size), &mut cx);
        retained.commit(cx.canvas.take_scene().unwrap());
    };
    let holds = || with_entry(id, |entry| entry.layer.holds()).unwrap();
    feed(id, b"prompt").unwrap();
    paint_at(0);
    paint_at(10);
    assert!(!holds(), "a screen that does not scroll keeps no layer");
    for i in 0..30 {
        feed(id, format!("line {i}\r\n").as_bytes()).unwrap();
    }
    paint_at(15);
    feed(id, b"three\r\nmore\r\nlines\r\n").unwrap();
    paint_at(20);
    assert!(holds(), "scrolled output draws through a layer");
    assert_eq!(
        with_entry(id, |entry| entry.animation_due).unwrap(),
        Some(1_021),
        "the terminal paints again when the layer goes quiet"
    );
    paint_at(500);
    assert!(holds());
    paint_at(2_000);
    assert!(
        !holds(),
        "a layer goes once the output has been still a while"
    );
    close(id);
}
