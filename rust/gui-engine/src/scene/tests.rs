use super::render::{self, Affine};
use super::*;
use crate::render::display::DrawOp;

fn budget() -> Budget {
    Budget::new(1024 * 1024 * 1024)
}

fn rect(id: &str, x: f64, y: f64, w: f64, h: f64) -> Node {
    Node {
        id: id.into(),
        label: id.to_uppercase(),
        shape: Shape::Rect {
            width: w,
            height: h,
            radius: 0.0,
        },
        transform: Transform {
            x,
            y,
            ..Transform::default()
        },
        fill: Some(Color::rgb(200, 30, 30)),
        ..Node::default()
    }
}

fn triangle(id: &str) -> Node {
    let mut path = Path::new();
    path.move_to(0.0, 0.0);
    path.line_to(100.0, 0.0);
    path.line_to(0.0, 100.0);
    path.close();
    Node {
        id: id.into(),
        shape: Shape::Path(path),
        transform: Transform {
            x: 300.0,
            y: 300.0,
            ..Transform::default()
        },
        fill: Some(Color::rgb(0, 0, 200)),
        ..Node::default()
    }
}

#[test]
fn transforms_compose_and_invert() {
    let t = Transform {
        x: 10.0,
        y: 20.0,
        rotation: 90.0,
        scale: 2.0,
    };
    let m = Affine::of(&t);
    let (x, y) = m.apply(1.0, 0.0);
    assert!(
        (x - 10.0).abs() < 1e-9 && (y - 22.0).abs() < 1e-9,
        "{x} {y}"
    );
    let camera = Affine::camera(&Camera {
        x: 5.0,
        y: 5.0,
        zoom: 3.0,
    });
    let both = camera.after(&m);
    let (px, py) = both.apply(4.0, -7.0);
    let (bx, by) = both.inverse().unwrap().apply(px, py);
    assert!((bx - 4.0).abs() < 1e-9 && (by + 7.0).abs() < 1e-9);
    assert!((both.magnitude() - 6.0).abs() < 1e-9);
}

#[test]
fn hits_follow_rotation_shapes_paths_groups_and_locks() {
    let rotated = Node {
        transform: Transform {
            x: 100.0,
            y: 100.0,
            rotation: 45.0,
            scale: 1.0,
        },
        ..rect("rot", 0.0, 0.0, 100.0, 20.0)
    };
    let ellipse = Node {
        id: "oval".into(),
        shape: Shape::Ellipse {
            width: 100.0,
            height: 50.0,
        },
        transform: Transform {
            x: 400.0,
            y: 100.0,
            ..Transform::default()
        },
        ..Node::default()
    };
    let group = Node {
        id: "group".into(),
        shape: Shape::Group {
            width: 50.0,
            height: 50.0,
            clip: true,
            children: vec![rect("inner", 0.0, 0.0, 200.0, 200.0)],
        },
        transform: Transform {
            x: 600.0,
            y: 600.0,
            ..Transform::default()
        },
        ..Node::default()
    };
    let locked = Node {
        locked: true,
        ..rect("locked", 0.0, 500.0, 50.0, 50.0)
    };
    let id = create(
        vec![rotated, ellipse, triangle("tri"), group, locked],
        budget(),
    )
    .unwrap();
    let hit = |x: f64, y: f64, deep: bool| hit_at(id, x, y, deep).unwrap();
    // Along the rotated bar's diagonal, but not at its unrotated corner.
    assert_eq!(hit(135.0, 145.0, false).as_deref(), Some("rot"));
    assert_eq!(hit(195.0, 105.0, false), None);
    assert_eq!(hit(450.0, 125.0, false).as_deref(), Some("oval"));
    assert_eq!(
        hit(402.0, 102.0, false),
        None,
        "outside the ellipse, inside its box"
    );
    assert_eq!(hit(310.0, 310.0, false).as_deref(), Some("tri"));
    assert_eq!(
        hit(390.0, 390.0, false),
        None,
        "beyond the triangle's hypotenuse"
    );
    assert_eq!(hit(610.0, 610.0, false).as_deref(), Some("group"));
    assert_eq!(hit(610.0, 610.0, true).as_deref(), Some("inner"));
    assert_eq!(hit(700.0, 700.0, true), None, "clipped by its group");
    assert_eq!(
        hit(10.0, 510.0, false),
        None,
        "locked objects are not picked"
    );
    close(id);
}

#[test]
fn edits_undo_redo_and_keep_their_history_bounded() {
    let budget = budget();
    let id = create(vec![rect("a", 0.0, 0.0, 10.0, 10.0)], budget.clone()).unwrap();
    add(id, "", None, rect("b", 50.0, 0.0, 10.0, 10.0)).unwrap();
    translate(id, &["a".into()], 5.0, 7.0).unwrap();
    resize(id, "b", 30.0, 40.0).unwrap();
    restack(id, "b", 0).unwrap();
    let order = read(id, |s| {
        s.nodes.iter().map(|n| n.id.clone()).collect::<Vec<_>>()
    })
    .unwrap();
    assert_eq!(order, ["b", "a"]);
    select(id, &["a".into(), "missing".into()]).unwrap();
    let info = remove(id, &["a".into()]).unwrap();
    assert!(info.selection.is_empty() && info.can_undo && !info.can_redo);
    undo(id).unwrap();
    let a = read(id, |s| find(&s.nodes, "a").cloned()).unwrap().unwrap();
    assert_eq!((a.transform.x, a.transform.y), (5.0, 7.0));
    assert!(redo(id).unwrap().nodes == 1);
    assert!(
        add(id, "", None, rect("b", 0.0, 0.0, 1.0, 1.0)).is_err(),
        "ids are unique"
    );
    assert!(
        add(id, "b", None, rect("c", 0.0, 0.0, 1.0, 1.0)).is_err(),
        "only groups take children"
    );
    assert!(resize(id, "b", -1.0, 5.0).is_err());
    for i in 0..(HISTORY_LIMIT + 20) {
        translate(id, &["b".into()], 1.0, 0.0).unwrap();
        assert!(
            with(id, |e| Ok(e.undo.len())).unwrap() <= HISTORY_LIMIT,
            "{i}"
        );
    }
    let x = read(id, |s| find(&s.nodes, "b").unwrap().transform.x).unwrap();
    for _ in 0..HISTORY_LIMIT + 5 {
        undo(id).unwrap();
    }
    let floor = read(id, |s| find(&s.nodes, "b").unwrap().transform.x).unwrap();
    assert_eq!(x - floor, HISTORY_LIMIT as f64);
    close(id);
    assert_eq!(budget.usage().0, 0);
}

#[test]
fn gestures_commit_one_undo_step() {
    let id = create(vec![rect("a", 0.0, 0.0, 10.0, 10.0)], budget()).unwrap();
    begin_gesture(id).unwrap();
    for step in 1..=20 {
        gesture(id, |nodes| {
            find_mut(nodes, "a").unwrap().transform.x = step as f64
        })
        .unwrap();
    }
    let info = end_gesture(id).unwrap().unwrap();
    assert!(info.can_undo);
    assert_eq!(with(id, |e| Ok(e.undo.len())).unwrap(), 1);
    undo(id).unwrap();
    assert_eq!(read(id, |s| s.nodes[0].transform.x).unwrap(), 0.0);
    begin_gesture(id).unwrap();
    assert_eq!(
        end_gesture(id).unwrap(),
        None,
        "an unchanged gesture records nothing"
    );
    close(id);
}

#[test]
fn rendering_transforms_shapes_and_marks_only_the_live_selection() {
    let spun = Node {
        transform: Transform {
            x: 50.0,
            y: 50.0,
            rotation: 90.0,
            scale: 2.0,
        },
        stroke: Some((Color::black(), 1.0)),
        ..rect("spun", 0.0, 0.0, 10.0, 5.0)
    };
    let id = create(vec![spun, triangle("tri")], budget()).unwrap();
    select(id, &["spun".into()]).unwrap();
    let theme = crate::style::Theme::light();
    let live = read(id, |s| render::render(s, &theme, true)).unwrap();
    let export = read(id, |s| render::render(s, &theme, false)).unwrap();
    assert!(live.len() > export.len());
    let DrawOp::Fill { path, .. } = &export[0] else {
        panic!("{export:?}")
    };
    // The 10 x 5 bar, doubled and turned a quarter, spans x 40..50 and y 50..70.
    let points = render::flatten(path);
    assert!(
        points
            .iter()
            .flatten()
            .all(|(x, y)| (39.9..=50.1).contains(x) && (49.9..=70.1).contains(y)),
        "{points:?}"
    );
    let DrawOp::StrokePath { width, .. } = &export[1] else {
        panic!()
    };
    assert!((width - 2.0).abs() < 1e-9);
    close(id);
}

#[test]
fn hit_test_geometry_stays_within_its_cache_bound() {
    let nodes: Vec<Node> = (0..1000)
        .map(|i| {
            let mut big = Path::new();
            big.move_to(0.0, 0.0);
            for k in 0..120 {
                big.cubic_to(k as f64, 1.0, k as f64 + 0.5, 2.0, k as f64 + 1.0, 0.0);
            }
            big.close();
            Node {
                id: format!("p{i}"),
                shape: Shape::Path(big),
                transform: Transform {
                    x: (i % 50) as f64 * 130.0,
                    y: (i / 50) as f64 * 5.0,
                    ..Transform::default()
                },
                ..Node::default()
            }
        })
        .collect();
    let id = create(nodes, budget()).unwrap();
    for i in 0..1000 {
        assert!(
            hit_at(
                id,
                (i % 50) as f64 * 130.0 + 60.5,
                (i / 50) as f64 * 5.0 + 0.5,
                false
            )
            .unwrap()
            .is_some()
        );
    }
    let (bytes, entries) = cache_usage(id).unwrap();
    assert!(
        bytes <= GEOMETRY_BYTES && entries > 0,
        "{bytes} bytes in {entries}"
    );
    close(id);
}

mod session {
    use super::*;
    use crate::widgets::Widget;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn pointer_and_keyboard_edit_the_scene_which_draws_as_it_exports() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::app::start("Scene", "dev.gossamer.gui-scene-test").unwrap();
            let blue = Node {
                fill: Some(Color::rgb(0, 0, 200)),
                ..rect("b", 200.0, 40.0, 60.0, 60.0)
            };
            let id = create(
                vec![rect("a", 40.0, 40.0, 80.0, 60.0), blue],
                crate::app::effects::current_budget(),
            )
            .unwrap();
            let heard = Rc::new(RefCell::new(Vec::new()));
            let log = heard.clone();
            s.set_shell(
                move |m| {
                    if let crate::widgets::cx::UiMsg::Scene(_, info) = m {
                        log.borrow_mut().push(info);
                    }
                },
                move |_| {
                    Widget::column(
                        &[
                            Widget::label("Header"),
                            Widget::Scene(widget::View {
                                id: "canvas".into(),
                                scene: id,
                                label: "Drawing".into(),
                                width: 500,
                                height: 300,
                            }),
                        ],
                        8,
                    )
                    .padding(24)
                },
                |_| Vec::new(),
            );
            s.open_window("main", "Scene", 600, 420).unwrap();
            s.step().unwrap();
            let r = s.rect_of("canvas").unwrap();
            let zoom = s.windows[0].theme.scale;
            let at = |x: f64, y: f64| (r.x + (x * zoom) as i64, r.y + (y * zoom) as i64);
            // The view draws what the export draws: the blue square's middle.
            let (cx, cy) = at(230.0, 70.0);
            let frame = s.capture_window(0).unwrap();
            let pixel = &frame.pixels[((cy * frame.width + cx) * 4) as usize..][..3];
            assert_eq!(pixel, [0, 0, 200]);
            let ops = read(id, |sc| {
                render::render(sc, &crate::style::Theme::light(), false)
            })
            .unwrap();
            let mut canvas = crate::render::Canvas::try_new(500, 300).unwrap();
            crate::render::display::draw(&mut canvas, &ops, 0, 0);
            assert_eq!(
                &canvas.pixels[((70 * 500 + 230) * 4) as usize..][..3],
                [0, 0, 200]
            );
            // Click selects; Shift-click adds; dragging moves both as one step.
            let (ax, ay) = at(60.0, 60.0);
            s.click_at(ax, ay).unwrap();
            let (bx, by) = at(220.0, 60.0);
            s.click_with(
                bx,
                by,
                windowing::Mods {
                    shift: true,
                    ..windowing::Mods::default()
                },
            )
            .unwrap();
            assert_eq!(read(id, |sc| sc.selection.clone()).unwrap(), ["a", "b"]);
            s.drag(ax, ay, ax + (20.0 * zoom) as i64, ay + (10.0 * zoom) as i64)
                .unwrap();
            let moved = read(id, |sc| (sc.nodes[0].transform.x, sc.nodes[1].transform.y)).unwrap();
            assert!(
                (moved.0 - 60.0).abs() < 1.0 && (moved.1 - 50.0).abs() < 1.0,
                "{moved:?}"
            );
            assert_eq!(with(id, |e| Ok(e.undo.len())).unwrap(), 1);
            // The keyboard walks objects, deletes and undoes.
            assert!(s.focus("canvas").unwrap());
            s.press(windowing::Key::Home, windowing::Mods::default())
                .unwrap();
            s.advance(2000).unwrap();
            assert!(
                s.spoken().iter().any(|t| t == "A, rectangle, 1 of 2"),
                "{:?}",
                s.spoken()
            );
            s.press(windowing::Key::Delete, windowing::Mods::default())
                .unwrap();
            assert_eq!(info_of(id).unwrap().nodes, 1);
            s.press(
                windowing::Key::Z,
                windowing::Mods {
                    ctrl: true,
                    ..windowing::Mods::default()
                },
            )
            .unwrap();
            assert_eq!(info_of(id).unwrap().nodes, 2);
            assert!(heard.borrow().len() >= 4);
            // Every object is an accessible child, with its selection.
            let tree = s.semantics(0);
            fn find_node<'a>(
                n: &'a crate::accessibility::AccessNode,
                id: &str,
            ) -> Option<&'a crate::accessibility::AccessNode> {
                if n.id == id {
                    return Some(n);
                }
                n.children.iter().find_map(|c| find_node(c, id))
            }
            let canvas = find_node(&tree, "canvas").unwrap();
            assert_eq!(canvas.children.len(), 2);
            assert_eq!(canvas.children[1].name, "B");
            assert!(!canvas.children[1].selected);
            s.accessible_action("canvas", "canvas#b", "select", "")
                .unwrap();
            assert_eq!(read(id, |sc| sc.selection.clone()).unwrap(), ["b"]);
            let tree = s.semantics(0);
            assert!(find_node(&tree, "canvas#b").unwrap().selected);
            // The wheel zooms about the pointer; dragging empty space pans.
            s.scroll_at(r.x + 10, r.y + 10, 3.0).unwrap();
            assert!(read(id, |sc| sc.camera.zoom).unwrap() > 1.2);
            let (ex, ey) = at(400.0, 250.0);
            s.drag(ex, ey, ex - 50, ey - 50).unwrap();
            assert!(read(id, |sc| sc.camera.x).unwrap() > 0.0);
            s.shutdown();
            assert!(read(id, |_| ()).is_err());
        })
    }
}
