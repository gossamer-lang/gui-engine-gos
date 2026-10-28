use super::*;

const UNICODE: &str = "Grüße, Ελληνικά, Русский, العربية and עברית";

fn paragraphs(n: usize) -> Vec<Item> {
    (0..n)
        .map(|i| Item::Paragraph {
            text: format!("Paragraph {i}: the quick brown fox jumps over the lazy dog, {UNICODE}."),
        })
        .collect()
}

fn texts(page: &Page) -> Vec<(i64, i64, String)> {
    page.ops
        .iter()
        .filter_map(|op| match op {
            DrawOp::Text { x, y, text, .. } => Some((*x, *y, text.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn pages_keep_text_inside_their_margins_and_number_themselves() {
    for (paper, landscape) in [
        (Paper::A4, false),
        (Paper::Letter, true),
        (Paper::A5, false),
    ] {
        let setup = PageSetup {
            paper,
            landscape,
            header: "Report".into(),
            ..PageSetup::default()
        };
        let mut items = vec![Item::Heading {
            text: "Title".into(),
            level: 1,
        }];
        items.extend(paragraphs(120));
        items.push(Item::PageBreak);
        items.push(Item::Paragraph {
            text: "After the break".into(),
        });
        let pages = paginate(&items, &setup, &|| false).unwrap();
        assert!(pages.len() > 3, "{paper:?}: {} pages", pages.len());
        let (w, h) = setup.size();
        let m = setup.margins;
        for (i, page) in pages.iter().enumerate() {
            assert_eq!((page.width, page.height), (w, h));
            let lines = texts(page);
            let footer = format!("Page {} of {}", i + 1, pages.len());
            assert!(
                lines
                    .iter()
                    .any(|(_, y, t)| *t == footer && *y as f64 > h - m.bottom),
                "{footer}"
            );
            assert!(
                lines
                    .iter()
                    .any(|(_, y, t)| t == "Report" && (*y as f64) < m.top)
            );
            for (x, y, text) in lines
                .iter()
                .filter(|(_, _, t)| *t != footer && t != "Report")
            {
                assert!(*x as f64 >= m.left - 0.5, "{text} at x {x}");
                assert!(
                    *y as f64 >= m.top - 0.5 && (*y as f64) < h - m.bottom,
                    "{text} at y {y}"
                );
            }
        }
        let last = texts(pages.last().unwrap());
        assert!(last.iter().any(|(_, _, t)| t == "After the break"));
        assert!(
            texts(&pages[pages.len() - 2])
                .iter()
                .all(|(_, _, t)| t != "After the break")
        );
    }
}

#[test]
fn impossible_setups_and_oversized_documents_are_refused() {
    let tiny = PageSetup {
        paper: Paper::Custom {
            width: 10.0,
            height: 10.0,
        },
        ..PageSetup::default()
    };
    assert!(paginate(&[], &tiny, &|| false).is_err());
    let margins = PageSetup {
        margins: Margins {
            top: 500.0,
            right: 0.0,
            bottom: 500.0,
            left: 0.0,
        },
        ..PageSetup::default()
    };
    assert!(paginate(&[], &margins, &|| false).is_err());
    let font = PageSetup {
        font_size: 0.0,
        ..PageSetup::default()
    };
    assert!(paginate(&[], &font, &|| false).is_err());
    // An empty document still prints one sheet.
    assert_eq!(
        paginate(&[], &PageSetup::default(), &|| false)
            .unwrap()
            .len(),
        1
    );
    let cancelled = paginate(&paragraphs(10), &PageSetup::default(), &|| true);
    assert_eq!(cancelled.unwrap_err(), "printing cancelled");
}

#[test]
fn pictures_fit_the_printable_width_and_keep_their_aspect() {
    let setup = PageSetup::default();
    let wide = Item::Pixels {
        rgba: vec![200; 2000 * 100 * 4],
        width: 2000,
        height: 100,
        alt: "wide".into(),
    };
    let drawing = Item::Drawing {
        ops: vec![DrawOp::FillRect {
            rect: Rect::new(0, 0, 400, 300),
            color: Color::rgb(0, 120, 200),
        }],
        width: 400.0,
        height: 300.0,
    };
    let pages = paginate(&[wide, drawing], &setup, &|| false).unwrap();
    let content = setup.size().0 - setup.margins.left - setup.margins.right;
    let image = pages[0]
        .ops
        .iter()
        .find_map(|op| match op {
            DrawOp::Image { rect, .. } => Some(*rect),
            _ => None,
        })
        .unwrap();
    assert!((image.width as f64 - content).abs() <= 1.0);
    assert!((image.height as f64 - content / 20.0).abs() <= 1.0);
    let layer = pages[0]
        .ops
        .iter()
        .find_map(|op| match op {
            DrawOp::Layer { transform, .. } => Some(*transform),
            _ => None,
        })
        .unwrap();
    assert!((layer.scale_x - layer.scale_y).abs() < 0.01, "{layer:?}");
    assert!(
        paginate(
            &[Item::Pixels {
                rgba: vec![0; 3],
                width: 2,
                height: 2,
                alt: String::new()
            }],
            &setup,
            &|| false
        )
        .is_err()
    );
}

fn pdf_objects_are_where_the_cross_reference_says(pdf: &[u8]) {
    let find = |needle: &[u8], from: usize| {
        pdf[from..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|at| at + from)
    };
    let marker = pdf.windows(10).rposition(|w| w == b"startxref\n").unwrap();
    let digits: String = pdf[marker + 10..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .map(|b| *b as char)
        .collect();
    let start: usize = digits.parse().unwrap();
    assert_eq!(&pdf[start..start + 7], b"xref\n0 ");
    let mut at = find(b"\n", start + 5).unwrap() + 1;
    at += 20;
    let mut number = 1;
    while &pdf[at + 16..at + 19] == b" n " {
        let offset: usize = std::str::from_utf8(&pdf[at..at + 10])
            .unwrap()
            .parse()
            .unwrap();
        let expected = format!("{number} 0 obj");
        assert_eq!(
            &pdf[offset..offset + expected.len()],
            expected.as_bytes(),
            "object {number}"
        );
        at += 20;
        number += 1;
    }
    assert!(number > 4);
}

fn tool(name: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(name).args(args).output().ok()?;
    assert!(
        output.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[test]
fn pdfs_carry_searchable_unicode_text_images_and_page_sizes() {
    let setup = PageSetup {
        paper: Paper::Letter,
        ..PageSetup::default()
    };
    let mut items = vec![Item::Heading {
        text: "Invoice".into(),
        level: 1,
    }];
    items.push(Item::Pixels {
        rgba: (0..64 * 64)
            .flat_map(|i| [i as u8, 80, 160, if i % 2 == 0 { 255 } else { 128 }])
            .collect(),
        width: 64,
        height: 64,
        alt: "logo".into(),
    });
    items.push(Item::Code {
        text: "let x = 1\nprintln(x)".into(),
    });
    items.extend(paragraphs(80));
    let pages = paginate(&items, &setup, &|| false).unwrap();
    let pdf = pdf::write(&pages, "Unicode report", &|| false).unwrap();
    assert!(pdf.starts_with(b"%PDF-1.7"));
    assert!(pdf.ends_with(b"%%EOF\n"));
    pdf_objects_are_where_the_cross_reference_says(&pdf);
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("/FontFile2") && text.contains("/ToUnicode") && text.contains("/SMask"));
    assert!(pdf::write(&pages, "x", &|| true).is_err());
    // Independent readers, where the system has them.
    let dir = std::env::temp_dir().join(format!("gui-print-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("report.pdf");
    std::fs::write(&file, &pdf).unwrap();
    let path = file.to_str().unwrap();
    if let Some(info) = tool("pdfinfo", &[path]) {
        let count = info
            .lines()
            .find_map(|l| l.strip_prefix("Pages:"))
            .map(str::trim);
        assert_eq!(count, Some(pages.len().to_string().as_str()), "{info}");
        assert!(info.contains("612 x 792 pts (letter)"), "{info}");
        assert!(info.contains("Unicode report"), "{info}");
        let extracted = tool("pdftotext", &["-enc", "UTF-8", path, "-"]).unwrap();
        assert!(extracted.contains("Invoice"), "{extracted}");
        assert!(extracted.contains("Grüße"), "{extracted}");
        assert!(
            extracted.contains("Ελληνικά") && extracted.contains("Русский"),
            "{extracted}"
        );
        assert!(extracted.contains("Page 2 of"), "{extracted}");
        let fonts = tool("pdffonts", &[path]).unwrap();
        assert!(
            fonts.contains("CID TrueType") && fonts.lines().skip(2).all(|l| l.contains(" yes ")),
            "{fonts}"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn previews_draw_ink_inside_the_margins_on_white_paper() {
    let setup = PageSetup {
        footer: String::new(),
        ..PageSetup::default()
    };
    let pages = paginate(&paragraphs(3), &setup, &|| false).unwrap();
    let image = preview(&pages[0], 0.5).unwrap();
    assert_eq!(image.width, (setup.size().0 * 0.5).ceil() as i64);
    let ink = |x0: i64, y0: i64, x1: i64, y1: i64| {
        (y0..y1).any(|y| (x0..x1).any(|x| image.rgba[((y * image.width + x) * 4) as usize] < 128))
    };
    let margin = (setup.margins.left * 0.5) as i64 - 1;
    assert!(ink(margin, margin, image.width - margin, image.height / 3));
    assert!(!ink(0, 0, image.width, margin));
    assert!(!ink(0, 0, margin, image.height));
    assert!(preview(&pages[0], 100.0).is_err());
}

#[test]
fn rich_documents_print_headings_lists_quotes_and_code() {
    let budget = crate::memory::Budget::new(512 * 1024 * 1024);
    let source = "# Notes\n\nFirst *paragraph*.\n\n- one\n- two\n\n1. first\n2. second\n\n> quoted\n\n```\ncode line\n```\n";
    let id = crate::rich::registry::import(
        crate::rich::markup::Format::Markdown,
        source,
        budget,
        || true,
    )
    .unwrap();
    let pages = paginate(&[Item::Rich(id)], &PageSetup::default(), &|| false).unwrap();
    let lines: Vec<String> = texts(&pages[0]).into_iter().map(|(_, _, t)| t).collect();
    for expected in [
        "Notes",
        "First paragraph.",
        "\u{2022}",
        "one",
        "two",
        "1.",
        "2.",
        "second",
        "quoted",
        "code line",
    ] {
        assert!(
            lines.iter().any(|l| l == expected),
            "{expected} missing from {lines:?}"
        );
    }
    crate::rich::registry::close(id);
    assert!(paginate(&[Item::Rich(id)], &PageSetup::default(), &|| false).is_err());
}

#[cfg(target_os = "linux")]
mod spooler {
    use super::*;
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixListener;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    /// A CUPS scheduler of the test's own, started on a socket the test has
    /// already bound, so clients reach it as soon as it is spawned.
    struct Spooler {
        dir: std::path::PathBuf,
        child: Child,
    }

    impl Drop for Spooler {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl Spooler {
        fn start() -> Option<Spooler> {
            let cupsd = std::path::Path::new("/usr/sbin/cupsd");
            if !cupsd.exists() {
                assert!(
                    std::env::var_os("GUI_REQUIRE_CUPS").is_none(),
                    "cupsd is required"
                );
                return None;
            }
            let dir = std::env::temp_dir().join(format!("gui-cups-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            for sub in ["spool", "tmp", "cache", "state"] {
                std::fs::create_dir_all(dir.join(sub)).unwrap();
            }
            let socket = dir.join("cups.sock");
            let d = dir.display();
            std::fs::write(
                dir.join("cupsd.conf"),
                format!("Listen {}\nLogLevel warn\nWebInterface No\nIdleExitTimeout 0\n<Location />\nOrder allow,deny\nAllow all\n</Location>\n", socket.display()),
            )
            .unwrap();
            let user = |flag: &str| {
                let out = Command::new("id").arg(flag).output().unwrap();
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            };
            std::fs::write(
                dir.join("cups-files.conf"),
                format!(
                    "ServerRoot {d}\nRequestRoot {d}/spool\nTempDir {d}/tmp\nCacheDir {d}/cache\nStateDir {d}/state\n\
                     DataDir /usr/share/cups\nServerBin /usr/lib/cups\nErrorLog {d}/error_log\nAccessLog {d}/access_log\n\
                     PageLog {d}/page_log\nUser {}\nGroup {}\nSystemGroup {}\n",
                    user("-un"),
                    user("-gn"),
                    user("-gn")
                ),
            )
            .unwrap();
            let listener = UnixListener::bind(&socket).unwrap();
            let fd = listener.as_raw_fd();
            let mut command = Command::new(cupsd);
            command
                .arg("-l")
                .arg("-c")
                .arg(dir.join("cupsd.conf"))
                .arg("-s")
                .arg(dir.join("cups-files.conf"));
            // SAFETY: the closure runs in the forked child before exec and only
            // makes async-signal-safe calls on a stack buffer.
            unsafe {
                command.pre_exec(move || {
                    if fd == 3 {
                        let flags = libc::fcntl(3, libc::F_GETFD);
                        libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                    } else if libc::dup2(fd, 3) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    // systemd socket activation: descriptor 3, for this process.
                    let mut pid = [0u8; 24];
                    let mut n = libc::getpid() as u64;
                    let mut i = pid.len() - 1;
                    loop {
                        i -= 1;
                        pid[i] = b'0' + (n % 10) as u8;
                        n /= 10;
                        if n == 0 {
                            break;
                        }
                    }
                    libc::setenv(c"LISTEN_FDS".as_ptr(), c"1".as_ptr(), 1);
                    libc::setenv(c"LISTEN_PID".as_ptr(), pid[i..].as_ptr().cast(), 1);
                    Ok(())
                });
            }
            let child = command.spawn().unwrap();
            drop(listener);
            Some(Spooler { dir, child })
        }

        fn server(&self) -> String {
            self.dir.join("cups.sock").display().to_string()
        }

        fn admin(&self, args: &[&str]) {
            let status = Command::new("lpadmin")
                .arg("-h")
                .arg(self.server())
                .args(args)
                .status()
                .unwrap();
            let log = std::fs::read_to_string(self.dir.join("error_log")).unwrap_or_default();
            assert!(status.success(), "lpadmin {args:?}: {log}");
        }
    }

    #[test]
    fn a_real_spooler_receives_the_pdf_withdraws_held_jobs_and_reports_errors() {
        let Some(spooler) = Spooler::start() else {
            return;
        };
        let printer = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = printer.local_addr().unwrap().port();
        spooler.admin(&[
            "-p",
            "socket-printer",
            "-E",
            "-v",
            &format!("socket://127.0.0.1:{port}"),
        ]);
        let pages = paginate(&paragraphs(30), &PageSetup::default(), &|| false).unwrap();
        let pdf = pdf::write(&pages, "Spooled", &|| false).unwrap();
        let options = native::Options {
            destination: "socket-printer".into(),
            server: spooler.server(),
            title: "Spooled".into(),
            ..native::Options::default()
        };
        let names = native::destinations(&spooler.server()).unwrap();
        assert!(
            names.iter().any(|d| d.name == "socket-printer"),
            "{names:?}"
        );
        let job = native::submit(&pages, || Ok(pdf.clone()), "A4", &options, &|| false).unwrap();
        assert!(job.starts_with("socket-printer-"), "{job}");
        // The printer end receives the document the spooler was given.
        let (mut connection, _) = printer.accept().unwrap();
        let mut received = Vec::new();
        connection.read_to_end(&mut received).unwrap();
        assert_eq!(received, pdf);
        // A held job is withdrawn before it prints.
        spooler.admin(&["-p", "held", "-v", &format!("socket://127.0.0.1:{port}")]);
        let held = native::Options {
            destination: "held".into(),
            ..options.clone()
        };
        let accept = Command::new("cupsaccept")
            .arg("-h")
            .arg(spooler.server())
            .arg("held")
            .status()
            .unwrap();
        assert!(accept.success());
        let job = native::submit(&pages, || Ok(pdf.clone()), "A4", &held, &|| false).unwrap();
        native::cancel(&spooler.server(), &job).unwrap();
        let pending = Command::new("lpstat")
            .arg("-h")
            .arg(spooler.server())
            .args(["-W", "not-completed", "-o", "held"])
            .output()
            .unwrap();
        assert!(!String::from_utf8_lossy(&pending.stdout).contains(&job));
        let missing = native::Options {
            destination: "no-such-printer".into(),
            ..options
        };
        let error =
            native::submit(&pages, || Ok(pdf.clone()), "A4", &missing, &|| false).unwrap_err();
        assert!(error.contains("lp:"), "{error}");
        assert!(native::cancel(&spooler.server(), "--all").is_err());
    }
}

mod session {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn background_exports_answer_through_the_session_and_cancel() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::app::start("Print", "dev.gossamer.gui-print-test").unwrap();
            let events = Rc::new(RefCell::new(Vec::new()));
            let heard = events.clone();
            s.set_shell(
                move |m| {
                    if let crate::widgets::cx::UiMsg::Print(job, event) = m {
                        heard.borrow_mut().push((job, event));
                    }
                },
                |_| crate::widgets::Widget::label("print"),
                Vec::new,
            );
            s.open_window("main", "Print", 200, 100).unwrap();
            let path = std::env::temp_dir().join(format!("gui-export-{}.pdf", std::process::id()));
            let target = job::Target::Pdf(path.display().to_string());
            let id = job::start(
                job::Source::Items(paragraphs(40), PageSetup::default()),
                "Export".into(),
                target.clone(),
            )
            .unwrap();
            let finished = |events: &[(i64, job::Event)], id: i64| {
                events
                    .iter()
                    .any(|(j, e)| *j == id && !matches!(e, job::Event::Paginated(_)))
            };
            while !finished(&events.borrow(), id) {
                s.step().unwrap();
            }
            let done: Vec<job::Event> = events
                .borrow()
                .iter()
                .filter(|(j, _)| *j == id)
                .map(|(_, e)| e.clone())
                .collect();
            assert!(
                matches!(done[0], job::Event::Paginated(n) if n > 1),
                "{done:?}"
            );
            let job::Event::Exported { bytes, .. } = &done[1] else {
                panic!("{done:?}")
            };
            assert_eq!(std::fs::metadata(&path).unwrap().len() as i64, *bytes);
            std::fs::remove_file(&path).unwrap();
            let handle = document::open(
                paragraphs(400),
                PageSetup::default(),
                crate::app::effects::current_budget(),
            )
            .unwrap();
            assert!(
                document::page_text(handle, 1)
                    .unwrap()
                    .contains("Paragraph 0")
            );
            // A cancelled job reports cancellation and writes nothing.
            let id = job::start(
                job::Source::Items(paragraphs(20_000), PageSetup::default()),
                "Big".into(),
                target.clone(),
            )
            .unwrap();
            job::cancel(id).unwrap();
            while !finished(&events.borrow(), id) {
                s.step().unwrap();
            }
            assert!(
                events
                    .borrow()
                    .iter()
                    .any(|(j, e)| *j == id && *e == job::Event::Cancelled)
            );
            assert!(!path.exists());
            assert!(job::cancel(id).is_err());
            // An open document's pages export without laying out again.
            let id = job::start(job::Source::Document(handle), "Doc".into(), target).unwrap();
            while !finished(&events.borrow(), id) {
                s.step().unwrap();
            }
            assert!(
                events
                    .borrow()
                    .iter()
                    .any(|(j, e)| *j == id && matches!(e, job::Event::Exported { .. }))
            );
            std::fs::remove_file(&path).unwrap();
            s.shutdown();
            assert!(document::pages(handle).is_err());
        })
    }
}
