use super::render::{self, Mapping};
use super::*;
use crate::geometry::Rect;
use crate::render::display::DrawOp;

fn budget() -> Budget {
    Budget::new(1024 * 1024 * 1024)
}

fn line(name: &str, capacity: i64) -> SeriesStyle {
    SeriesStyle {
        name: name.into(),
        capacity,
        ..SeriesStyle::default()
    }
}

#[test]
fn ticks_are_readable_on_every_scale() {
    assert_eq!(
        render::ticks(&Scale::Linear, 0.0, 10.0, 5),
        vec![0.0, 2.0, 4.0, 6.0, 8.0, 10.0]
    );
    assert_eq!(
        render::ticks(&Scale::Linear, -0.3, 0.3, 3),
        vec![-0.2, 0.0, 0.2]
    );
    assert_eq!(
        render::ticks(&Scale::Log, 1.0, 1000.0, 5),
        vec![1.0, 10.0, 100.0, 1000.0]
    );
    let days = render::ticks(&Scale::Time, 0.0, 10.0 * 86_400.0, 5);
    assert!(days.iter().all(|t| t % 86_400.0 == 0.0), "{days:?}");
    let names: Vec<String> = (0..100).map(|i| format!("c{i}")).collect();
    let categories = render::ticks(&Scale::Category(names), -0.5, 99.5, 10);
    assert!(categories.len() <= 11 && categories[0] == 0.0);
    assert_eq!(
        render::format_value(&Scale::Time, 86_400.0),
        "1970-01-02 00:00"
    );
    assert_eq!(render::format_value(&Scale::Linear, 2.50), "2.5");
    assert_eq!(render::format_value(&Scale::Linear, 1.0e9), "1.000e9");
}

#[test]
fn ranges_fit_visible_data_and_respect_the_scale() {
    let id = create(Spec::default(), budget()).unwrap();
    add_series(id, line("a", 100), &[(1.0, 5.0), (3.0, 9.0)]).unwrap();
    let w = window(id).unwrap();
    assert!(w.x0 < 1.0 && w.x1 > 3.0 && w.y0 < 5.0 && w.y1 > 9.0);
    let bars = SeriesStyle {
        kind: Kind::Bar,
        ..line("b", 100)
    };
    add_series(id, bars, &[(2.0, 4.0)]).unwrap();
    assert!(window(id).unwrap().y0 <= 0.0, "bars start from zero");
    set_hidden(id, 1, true).unwrap();
    assert!(window(id).unwrap().y0 > 0.0);
    let custom = Window {
        x0: 0.0,
        x1: 1.0,
        y0: 0.0,
        y1: 1.0,
    };
    set_window(id, Some(custom)).unwrap();
    assert_eq!(window(id).unwrap(), custom);
    assert!(
        set_window(
            id,
            Some(Window {
                x0: 1.0,
                x1: 0.0,
                y0: 0.0,
                y1: 1.0
            })
        )
        .is_err()
    );
    let log = create(
        Spec {
            y: Axis {
                label: String::new(),
                scale: Scale::Log,
            },
            ..Spec::default()
        },
        budget(),
    )
    .unwrap();
    add_series(
        log,
        line("l", 10),
        &[(0.0, -5.0), (1.0, 10.0), (2.0, 1000.0)],
    )
    .unwrap();
    let w = window(log).unwrap();
    assert!(w.y0 > 0.0 && w.y0 < 10.0 && w.y1 > 1000.0);
    close(id);
    close(log);
}

#[test]
fn decimation_keeps_every_columns_extremes_of_a_million_points() {
    let id = create(Spec::default(), budget()).unwrap();
    let points: Vec<(f64, f64)> = (0..1_000_000i64)
        .map(|i| (i as f64, ((i * 7919) % 1000) as f64))
        .collect();
    add_series(id, line("big", 1_000_000), &points).unwrap();
    read(id, |model| {
        let plot = Rect::new(0, 0, 500, 300);
        let map = Mapping::new(model, plot);
        let thinned = render::decimate(&model.series[0].points, &map, true);
        assert!(thinned.len() <= 500 * 4 + 8, "{}", thinned.len());
        let (lo, hi) = thinned
            .iter()
            .fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
        assert_eq!((lo, hi), (0.0, 999.0));
        let start = std::time::Instant::now();
        let ops = render::render(model, 800, 500, &crate::style::Theme::light());
        assert!(ops.len() < 400, "{} ops", ops.len());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
    })
    .unwrap();
    close(id);
}

#[test]
fn rendering_draws_titles_axes_legends_and_skips_hidden_series() {
    let spec = Spec {
        title: "Revenue".into(),
        x: Axis {
            label: "Quarter".into(),
            scale: Scale::Category(vec!["Q1".into(), "Q2".into(), "Q3".into()]),
        },
        y: Axis {
            label: "Million".into(),
            scale: Scale::Linear,
        },
        legend: true,
    };
    let id = create(spec, budget()).unwrap();
    add_series(
        id,
        SeriesStyle {
            kind: Kind::Bar,
            ..line("North", 10)
        },
        &[(0.0, 4.0), (1.0, 6.0), (2.0, 5.0)],
    )
    .unwrap();
    add_series(
        id,
        SeriesStyle {
            kind: Kind::Line,
            ..line("South", 10)
        },
        &[(0.0, 2.0), (1.0, 3.0), (2.0, 7.0)],
    )
    .unwrap();
    let theme = crate::style::Theme::light();
    let texts = |ops: &[DrawOp]| -> Vec<String> {
        ops.iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    };
    let ops = read(id, |m| render::render(m, 600, 400, &theme)).unwrap();
    let words = texts(&ops);
    for word in [
        "Revenue", "Quarter", "Million", "Q1", "Q3", "North", "South",
    ] {
        assert!(words.iter().any(|w| w == word), "{word} missing: {words:?}");
    }
    let bars = ops
        .iter()
        .filter(|op| matches!(op, DrawOp::FillRect { .. }))
        .count();
    assert!(ops.iter().any(|op| matches!(op, DrawOp::StrokePath { .. })));
    set_hidden(id, 1, true).unwrap();
    let ops = read(id, |m| render::render(m, 600, 400, &theme)).unwrap();
    assert!(!ops.iter().any(|op| matches!(op, DrawOp::StrokePath { .. })));
    assert_eq!(
        ops.iter()
            .filter(|op| matches!(op, DrawOp::FillRect { .. }))
            .count(),
        bars
    );
    close(id);
}

#[test]
fn hit_tests_find_the_nearest_visible_point_and_legend_entries() {
    let id = create(Spec::default(), budget()).unwrap();
    add_series(id, line("a", 100), &[(0.0, 0.0), (1.0, 1.0), (2.0, 4.0)]).unwrap();
    add_series(
        id,
        SeriesStyle {
            kind: Kind::Scatter,
            ..line("b", 100)
        },
        &[(2.0, 0.5), (0.5, 3.0)],
    )
    .unwrap();
    let theme = crate::style::Theme::light();
    read(id, |m| {
        let frame = render::frame(m, 600, 400, &theme);
        let map = Mapping::new(m, frame.plot);
        let hit = render::hit(m, 600, 400, &theme, map.px(1.0) + 2.0, map.py(1.0) - 2.0).unwrap();
        assert_eq!((hit.series, hit.index, hit.x, hit.y), (0, 1, 1.0, 1.0));
        let unsorted = render::hit(m, 600, 400, &theme, map.px(0.5), map.py(3.0)).unwrap();
        assert_eq!((unsorted.series, unsorted.index), (1, 1));
        assert!(render::hit(m, 600, 400, &theme, map.px(1.5), map.py(-0.2)).is_none());
        let (series, entry) = frame.legend[1];
        assert_eq!(
            render::legend_hit(m, 600, 400, &theme, entry.x + 2, entry.y + 2),
            Some(series)
        );
    })
    .unwrap();
    set_hidden(id, 0, true).unwrap();
    read(id, |m| {
        let frame = render::frame(m, 600, 400, &theme);
        let map = Mapping::new(m, frame.plot);
        assert!(render::hit(m, 600, 400, &theme, map.px(1.0), map.py(1.0)).is_none());
    })
    .unwrap();
    close(id);
}

#[test]
fn appends_keep_each_series_within_its_capacity_and_budget() {
    let budget = budget();
    let id = create(Spec::default(), budget.clone()).unwrap();
    let before = budget.usage().0;
    add_series(id, line("live", 5), &[(0.0, 0.0)]).unwrap();
    assert!(budget.usage().0 >= before + 5 * POINT_BYTES);
    append(
        id,
        0,
        &[
            (1.0, 1.0),
            (2.0, 2.0),
            (3.0, 3.0),
            (4.0, 4.0),
            (5.0, 5.0),
            (6.0, 6.0),
        ],
    )
    .unwrap();
    let points: Vec<(f64, f64)> =
        read(id, |m| m.series[0].points.iter().copied().collect()).unwrap();
    assert_eq!(points.first(), Some(&(2.0, 2.0)));
    assert_eq!(points.len(), 5);
    assert!(read(id, |m| m.series[0].sorted).unwrap());
    append(id, 0, &[(1.5, 9.0)]).unwrap();
    assert!(!read(id, |m| m.series[0].sorted).unwrap());
    assert!(append(id, 0, &[(f64::NAN, 1.0)]).is_err());
    assert!(add_series(id, line("huge", POINT_LIMIT as i64 + 1), &[]).is_err());
    let tiny = Budget::new(64 * 1024);
    let cramped = create(Spec::default(), tiny.clone()).unwrap();
    assert!(add_series(cramped, line("big", 1_000_000), &[]).is_err());
    close(cramped);
    assert_eq!(tiny.usage().0, 0);
    close(id);
    assert_eq!(budget.usage().0, 0);
}

#[test]
fn exports_draw_the_same_picture_as_png_and_through_printing() {
    let id = create(
        Spec {
            title: "Export".into(),
            ..Spec::default()
        },
        budget(),
    )
    .unwrap();
    add_series(id, line("a", 100), &[(0.0, 1.0), (1.0, 3.0), (2.0, 2.0)]).unwrap();
    let theme = crate::style::Theme::light();
    let png = export_png(id, 320, 200, &theme).unwrap();
    let image = crate::render::png::decode(&png).unwrap();
    assert_eq!((image.width, image.height), (320, 200));
    assert!(
        image.rgba.chunks(4).any(|p| p[2] > 150 && p[0] < 60),
        "series color missing"
    );
    let ops = read(id, |m| render::render(m, 320, 200, &theme)).unwrap();
    let pages = crate::print::paginate(
        &[crate::print::Item::Drawing {
            ops,
            width: 320.0,
            height: 200.0,
        }],
        &crate::print::PageSetup::default(),
        &|| false,
    )
    .unwrap();
    let pdf = crate::print::pdf::write(&pages, "Chart", &|| false).unwrap();
    assert!(String::from_utf8_lossy(&pdf).contains("/FontFile2"));
    let file = std::env::temp_dir().join(format!("gui-chart-{}.pdf", std::process::id()));
    std::fs::write(&file, &pdf).unwrap();
    if let Ok(out) = std::process::Command::new("pdftotext")
        .arg(&file)
        .arg("-")
        .output()
    {
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("Export"),
            "the chart title is PDF text"
        );
    }
    std::fs::remove_file(file).unwrap();
    assert!(export_png(id, 1, 1, &theme).is_err());
    close(id);
}

mod session {
    use super::*;
    use crate::widgets::Widget;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn the_widget_hovers_pans_zooms_reads_aloud_and_follows_live_appends() {
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| {
            crate::services::remove_test_config(&config);
        });
        let mut s = crate::app::start("Chart", "dev.gossamer.gui-chart-test").unwrap();
        let id = create(
            Spec {
                title: "Live".into(),
                ..Spec::default()
            },
            crate::app::effects::current_budget(),
        )
        .unwrap();
        add_series(
            id,
            line("Load", 1000),
            &[(0.0, 1.0), (1.0, 2.0), (2.0, 1.5), (3.0, 3.0)],
        )
        .unwrap();
        let clicked = Rc::new(RefCell::new(Vec::new()));
        let heard = clicked.clone();
        s.set_shell(
            move |m| {
                if let crate::widgets::cx::UiMsg::ChartPoint(_, hit) = m {
                    heard.borrow_mut().push(hit);
                }
            },
            move |_| {
                // Offset from the window's origin, as an embedded chart is.
                let chart = Widget::Chart(widget::View {
                    id: "chart".into(),
                    chart: id,
                    label: "Load".into(),
                    width: 600,
                    height: 400,
                });
                Widget::column(&[Widget::label("Header"), chart], 8).padding(24)
            },
            Vec::new,
        );
        s.open_window("main", "Chart", 600, 400).unwrap();
        s.step().unwrap();
        let rect = s.rect_of("chart").unwrap();
        let theme = s.windows[0].theme.clone();
        let size = crate::geometry::Size::new(rect.width, rect.height);
        let (px, py) = read(id, |m| {
            let map = Mapping::new(m, render::frame(m, size.width, size.height, &theme).plot);
            (map.px(1.0).round() as i64, map.py(2.0).round() as i64)
        })
        .unwrap();
        s.hover(rect.x + px, rect.y + py).unwrap();
        assert_eq!(
            read(id, |m| m.hover.as_ref().map(|h| h.index)).unwrap(),
            Some(1)
        );
        s.click_at(rect.x + px, rect.y + py).unwrap();
        assert_eq!(
            clicked.borrow().last().map(|h| (h.x, h.y)),
            Some((1.0, 2.0))
        );
        let fitted = window(id).unwrap();
        s.scroll_at(rect.x + rect.width / 2, rect.y + rect.height / 2, 2.0)
            .unwrap();
        let zoomed = window(id).unwrap();
        assert!(zoomed.x1 - zoomed.x0 < fitted.x1 - fitted.x0);
        s.drag(
            rect.x + rect.width / 2,
            rect.y + rect.height / 2 + 30,
            rect.x + rect.width / 2 + 100,
            rect.y + rect.height / 2 + 30,
        )
        .unwrap();
        let panned = window(id).unwrap();
        assert!(panned.x0 < zoomed.x0, "{panned:?} {zoomed:?}");
        // The keyboard reads points aloud and restores the view.
        assert!(s.focus("chart").unwrap());
        s.press(windowing::Key::Right, windowing::Mods::default())
            .unwrap();
        s.press(windowing::Key::Right, windowing::Mods::default())
            .unwrap();
        s.advance(2000).unwrap();
        assert!(
            s.spoken()
                .iter()
                .any(|t| t.contains("Load: 2, 1.5 (3 of 4)")),
            "{:?}",
            s.spoken()
        );
        s.press(windowing::Key::Home, windowing::Mods::default())
            .unwrap();
        assert_eq!(window(id).unwrap(), fitted);
        let node = s.semantics(0);
        fn find<'a>(
            n: &'a crate::accessibility::AccessNode,
            id: &str,
        ) -> Option<&'a crate::accessibility::AccessNode> {
            if n.id == id {
                return Some(n);
            }
            n.children.iter().find_map(|c| find(c, id))
        }
        let chart = find(&node, "chart").unwrap();
        assert!(
            chart.description.contains("Load: 4 points, 1 to 3"),
            "{}",
            chart.description
        );
        assert_eq!(chart.value, "Load: 2, 1.5 (3 of 4)");
        // A feed on another thread appends; the session wakes and repaints.
        let revision = read(id, |m| m.revision).unwrap();
        std::thread::spawn(move || append(id, 0, &[(4.0, 9.0)]).unwrap())
            .join()
            .unwrap();
        s.step().unwrap();
        assert!(read(id, |m| m.revision).unwrap() > revision);
        assert!(window(id).unwrap().y1 > 9.0);
        s.shutdown();
        assert!(read(id, |_| ()).is_err());
    }
}
