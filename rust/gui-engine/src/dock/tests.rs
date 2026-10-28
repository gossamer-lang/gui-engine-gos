use super::*;

fn names(panes: &[&str]) -> Vec<String> {
    panes.iter().map(|p| p.to_string()).collect()
}

fn group_panes(layout: &Layout, group: i64) -> Vec<String> {
    layout
        .group_areas()
        .into_iter()
        .find(|(_, id, ..)| *id == group)
        .map(|(_, _, panes, _)| panes)
        .unwrap()
}

#[test]
fn edge_and_center_placement_split_and_collapse_groups() {
    let mut layout = Layout::new(&names(&["a", "b", "c"]));
    layout.place("b", 1, Zone::Right, None).unwrap();
    let Node::Split {
        axis,
        first,
        second,
        ..
    } = &layout.root
    else {
        panic!("no split: {layout:?}")
    };
    assert_eq!(*axis, Axis::Horizontal);
    assert!(matches!(**first, Node::Group { ref panes, .. } if *panes == names(&["a", "c"])));
    assert!(matches!(**second, Node::Group { ref panes, .. } if *panes == names(&["b"])));
    let right = layout.group_of("b").unwrap();
    layout.place("c", right, Zone::Top, None).unwrap();
    assert_eq!(layout.group_areas().len(), 3);
    // Moving the last pane out of a group collapses its split.
    let left = layout.group_of("a").unwrap();
    layout.place("c", left, Zone::Center, Some(0)).unwrap();
    assert_eq!(group_panes(&layout, left), names(&["c", "a"]));
    assert_eq!(layout.group_areas().len(), 2);
    layout.place("b", left, Zone::Center, None).unwrap();
    assert!(
        matches!(layout.root, Node::Group { ref panes, ref active, .. }
        if *panes == names(&["c", "a", "b"]) && active == "b")
    );
    // A pane alone cannot split from its own group.
    let only = Layout::new(&names(&["x"]));
    let mut same = only.clone();
    same.place("x", 1, Zone::Left, None).unwrap();
    assert_eq!(same, only);
    assert!(layout.place("missing", 1, Zone::Left, None).is_err());
    assert!(layout.place("a", 99, Zone::Left, None).is_err());
}

#[test]
fn a_share_sizes_the_group_holding_the_pane_against_its_neighbor() {
    let mut layout = Layout::new(&names(&["editor", "outline", "console"]));
    let main = layout.group_of("editor").unwrap();
    layout.place("outline", main, Zone::Left, None).unwrap();
    layout.place("console", main, Zone::Bottom, None).unwrap();
    layout.set_share("outline", 0.25).unwrap();
    layout.set_share("console", 0.3).unwrap();
    let Node::Split { ratio, second, .. } = &layout.root else {
        panic!("no split: {layout:?}")
    };
    assert_eq!(*ratio, 0.25);
    let Node::Split { ratio, .. } = &**second else {
        panic!("no inner split: {layout:?}")
    };
    assert!((ratio - 0.7).abs() < 1e-9);
    assert!(layout.set_share("missing", 0.5).is_err());
    assert!(layout.set_share("outline", f64::NAN).is_err());
    let mut alone = Layout::new(&names(&["only"]));
    assert!(alone.set_share("only", 0.5).is_err());
}

#[test]
fn reorder_detach_reattach_and_close_keep_every_pane_once() {
    let mut layout = Layout::new(&names(&["a", "b", "c", "d"]));
    layout.reorder(1, "d", 0).unwrap();
    assert_eq!(layout.panes(), names(&["d", "a", "b", "c"]));
    let (window, closed) = layout.detach("b", (10, 20, 50, 50)).unwrap();
    assert_eq!(closed, None);
    assert_eq!((window.width, window.height), (160, 120));
    let floating_group = layout.group_of("b").unwrap();
    layout
        .place("c", floating_group, Zone::Bottom, None)
        .unwrap();
    assert_eq!(layout.panes(), names(&["d", "a", "b", "c"]));
    // Moving the last panes out of a floating window closes it.
    layout.place("b", 1, Zone::Center, None).unwrap();
    let closed = layout.place("c", 1, Zone::Center, None).unwrap();
    assert_eq!(closed, Some(window.window.clone()));
    assert!(layout.floating.is_empty());
    let (window, _) = layout.detach("a", (0, 0, 640, 480)).unwrap();
    layout.reattach(&window.window).unwrap();
    assert!(layout.floating.is_empty());
    assert_eq!(layout.panes().len(), 4);
    assert_eq!(layout.close("a").unwrap(), None);
    assert!(layout.close("a").is_err());
    let mut single = Layout::new(&names(&["only"]));
    assert!(single.detach("only", (0, 0, 640, 480)).is_err());
}

#[test]
fn keyboard_moves_reach_neighbors_and_cycle_groups() {
    let mut layout = Layout::new(&names(&["a", "b", "c"]));
    layout.nudge("b", Zone::Right).unwrap();
    let right = layout.group_of("b").unwrap();
    assert_ne!(right, 1);
    // With a group on the left, moving left joins it rather than splitting.
    layout.nudge("b", Zone::Left).unwrap();
    assert_eq!(layout.group_of("b"), Some(1));
    assert_eq!(layout.group_areas().len(), 1);
    layout.nudge("c", Zone::Bottom).unwrap();
    assert_eq!(layout.next_group("a", false), Some("c".to_string()));
    assert_eq!(layout.next_group("c", false), Some("b".to_string()));
    assert_eq!(layout.next_group("c", true), Some("b".to_string()));
    assert_eq!(
        layout.command("dock", "a", "dock.next-group"),
        vec![Change::Focus {
            strip: group_strip("dock", layout.group_of("c").unwrap()),
            pane: "c".into()
        }]
    );
    let changes = layout.command("dock", "c", "dock.move-up");
    assert!(changes.contains(&Change::Layout));
    assert_eq!(layout.group_of("c"), Some(1));
    let changes = layout.command("dock", "c", "dock.detach");
    let Some(Change::Opened(window)) = changes.last().cloned() else {
        panic!("{changes:?}")
    };
    assert_eq!(
        layout.command("dock", "c", "dock.reattach"),
        vec![Change::Layout, Change::Closed(window.window)]
    );
}

#[test]
fn toolkit_messages_fold_into_the_layout() {
    let mut layout = Layout::new(&names(&["a", "b"]));
    let strip = group_strip("dock", 1);
    assert_eq!(
        layout.handle("dock", &UiMsg::Tab(strip.clone(), "b".into())),
        vec![Change::Layout]
    );
    assert!(matches!(layout.root, Node::Group { ref active, .. } if active == "b"));
    layout.handle("dock", &UiMsg::Reorder(strip.clone(), "b".into(), 0));
    assert_eq!(layout.panes(), names(&["b", "a"]));
    let drop = Drop {
        pane: "a".into(),
        group: strip.clone(),
        zone: Zone::Bottom,
    };
    layout.handle("dock", &UiMsg::Dock("dock".into(), drop));
    let Node::Split { id, axis, .. } = layout.root.clone() else {
        panic!("{layout:?}")
    };
    assert_eq!(axis, Axis::Vertical);
    layout.handle("dock", &UiMsg::Split(format!("dock:split:{id}"), 0.999));
    assert!(matches!(layout.root, Node::Split { ratio, .. } if ratio == 1.0 - MIN_RATIO));
    let changes = layout.handle("dock", &UiMsg::TabDetach(strip.clone(), "b".into(), 30, 40));
    let Some(Change::Opened(window)) = changes.last().cloned() else {
        panic!("{changes:?}")
    };
    assert_eq!((window.x, window.y), (30, 40));
    let changes = layout.handle("dock", &UiMsg::CloseRequested(window.window.clone()));
    assert_eq!(changes, vec![Change::Layout, Change::Closed(window.window)]);
    let a_group = group_strip("dock", layout.group_of("a").unwrap());
    let changes = layout.handle(
        "dock",
        &UiMsg::TabAction(a_group, "a".into(), "close".into()),
    );
    assert!(changes.contains(&Change::PaneClosed("a".into())));
    // Messages about other docks and widgets leave the layout alone.
    let before = layout.clone();
    assert!(
        layout
            .handle("other", &UiMsg::Tab(strip.clone(), "b".into()))
            .is_empty()
    );
    assert!(layout.handle("dock", &UiMsg::Click("x".into())).is_empty());
    assert_eq!(layout, before);
}

#[test]
fn restore_migrates_repairs_and_recovers_from_corrupt_saves() {
    let known = names(&["a", "b", "c", "new"]);
    let mut layout = Layout::new(&names(&["a", "b", "c"]));
    layout.place("b", 1, Zone::Right, None).unwrap();
    layout.detach("c", (5, 6, 700, 500)).unwrap();
    let saved = layout.save();
    let (restored, repairs) = Layout::restore(&saved, &names(&["a", "b", "c"]));
    assert_eq!(restored, layout);
    assert!(repairs.is_empty(), "{repairs:?}");
    // A new pane joins; a pane the application no longer has is dropped.
    let (restored, repairs) = Layout::restore(&saved, &names(&["a", "c", "new"]));
    assert_eq!(restored.panes(), names(&["a", "new", "c"]));
    assert!(repairs.iter().any(|r| r.contains("added 1")), "{repairs:?}");
    assert!(matches!(restored.root, Node::Group { .. }));
    // Version 1 saved a row of groups.
    let v1 = r#"{"version":1,"groups":[["a"],["b","c"],["zz"]]}"#;
    let (migrated, repairs) = Layout::restore(v1, &known);
    assert!(repairs[0].contains("migrated"));
    assert_eq!(migrated.version, VERSION);
    assert_eq!(migrated.panes(), names(&["a", "new", "b", "c"]));
    assert_eq!(migrated.group_areas().len(), 2);
    // Repeated ids, repeated panes and impossible ratios are repaired.
    let tampered = saved
        .replace("\"ratio\":0.5", "\"ratio\":7.0")
        .replace("\"panes\":[\"b\"]", "\"panes\":[\"b\",\"a\"]");
    let (repaired, repairs) = Layout::restore(&tampered, &names(&["a", "b", "c"]));
    assert_eq!(repaired.panes(), names(&["a", "b", "c"]));
    assert!(repairs.iter().any(|r| r.contains("clamped")), "{repairs:?}");
    for corrupt in [
        "",
        "{",
        r#"{"version":99}"#,
        r#"{"version":2,"root":7}"#,
        "[]",
    ] {
        let (fresh, repairs) = Layout::restore(corrupt, &known);
        assert_eq!(fresh, Layout::new(&known), "{corrupt}");
        assert!(repairs[0].starts_with("started over"));
    }
    let huge = "x".repeat(SOURCE_LIMIT + 1);
    assert!(Layout::restore(&huge, &known).1[0].contains("size limit"));
}

#[test]
fn the_view_builds_workspace_tab_groups_and_splits() {
    let mut layout = Layout::new(&names(&["a", "b"]));
    layout.place("b", 1, Zone::Bottom, None).unwrap();
    let panes: HashMap<String, Pane> = ["a", "b"]
        .iter()
        .map(|key| {
            (
                key.to_string(),
                Pane {
                    title: key.to_uppercase(),
                    content: Widget::label(key),
                    closable: *key == "b",
                },
            )
        })
        .collect();
    let Widget::Split(split) = view(&layout, "dock", "", &panes) else {
        panic!("no split")
    };
    assert_eq!(split.axis, Axis::Vertical);
    let Widget::Tabs(top) = *split.first else {
        panic!("no group")
    };
    assert_eq!(top.id, "dock:group:1");
    assert_eq!(top.workspace, "dock");
    assert_eq!(top.items[0].title, "A");
    assert!(!top.items[0].closable);
    assert!(top.detachable);
    assert_eq!(
        view(&layout, "dock", "missing-window", &panes),
        Widget::Empty
    );
}

mod session {
    use super::*;
    use crate::widgets::tree;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn panes(text: &str) -> HashMap<String, Pane> {
        let mut panes = HashMap::new();
        panes.insert(
            "editor".to_string(),
            Pane {
                title: "Editor".into(),
                content: Widget::text_field("editor-input", text, ""),
                closable: false,
            },
        );
        for key in ["notes", "log"] {
            panes.insert(
                key.to_string(),
                Pane {
                    title: key.to_string(),
                    content: Widget::label(key),
                    closable: true,
                },
            );
        }
        panes
    }

    /// The window pixel center of `pane`'s tab in `strip`, and the strip's
    /// rectangle.
    fn tab_center(
        s: &crate::Session,
        w: usize,
        strip: &str,
        pane: &str,
    ) -> ((i64, i64), crate::geometry::Rect) {
        let window = &s.windows[w];
        for entry in tree::find_kind(&window.root, &window.node, &window.theme, "tabs") {
            if let Widget::Tabs(t) = &entry.widget
                && t.id == strip
            {
                let at = t.items.iter().position(|i| i.key == pane).unwrap();
                let r = entry.node.parts[at * 2];
                let rect = crate::geometry::Rect::new(
                    entry.x,
                    entry.y,
                    entry.node.rect.width,
                    entry.node.rect.height,
                );
                return ((entry.x + r.center_x(), entry.y + r.center_y()), rect);
            }
        }
        panic!("no strip {strip}")
    }

    #[test]
    fn dragged_tabs_dock_by_zone_and_panes_keep_state_across_windows_of_different_scales() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let mut s = crate::app::start("Dock", "dev.gossamer.gui-dock-test").unwrap();
            let layout = Rc::new(RefCell::new(Layout::new(&names(&[
                "editor", "notes", "log",
            ]))));
            let (model, shown) = (layout.clone(), layout.clone());
            let text = Rc::new(RefCell::new("draft".to_string()));
            let (typed, value) = (text.clone(), text.clone());
            s.set_shell(
                move |m| {
                    if let UiMsg::Text(id, t) = &m
                        && id == "editor-input"
                    {
                        *typed.borrow_mut() = t.clone();
                    }
                    for change in model.borrow_mut().handle("dock", &m) {
                        if let Change::Opened(f) = change {
                            crate::app::effects::perform(crate::app::effects::Effect::OpenWindow {
                                key: f.window,
                                title: "Pane".into(),
                                width: 420,
                                height: 300,
                            });
                        } else if let Change::Closed(window) = change {
                            crate::app::effects::perform(crate::app::effects::Effect::CloseWindow(
                                window,
                            ));
                        }
                    }
                },
                move |key| {
                    let window = if key == "main" { "" } else { key };
                    view(&shown.borrow(), "dock", window, &panes(&value.borrow()))
                },
                |_| Vec::new(),
            );
            s.open_window("main", "Dock", 900, 600).unwrap();
            s.step().unwrap();
            // Dragging "log" to the right quarter of its own group docks it there.
            let ((tx, ty), group) = tab_center(&s, 0, "dock:group:1", "log");
            let to = (group.x + group.width - 20, group.y + group.height / 2);
            s.drag(tx, ty, to.0, to.1).unwrap();
            let right = layout.borrow().group_of("log").unwrap();
            assert_ne!(right, 1);
            assert!(matches!(
                layout.borrow().root,
                Node::Split {
                    axis: Axis::Horizontal,
                    ..
                }
            ));
            // Dragging "notes" into the middle of that group joins its tabs.
            let ((tx, ty), _) = tab_center(&s, 0, "dock:group:1", "notes");
            let (_, target) = tab_center(&s, 0, &group_strip("dock", right), "log");
            let middle = (target.x + target.width / 2, target.y + target.height / 2);
            s.drag(tx, ty, middle.0, middle.1).unwrap();
            assert_eq!(layout.borrow().group_of("notes"), Some(right));
            // The editor's caret survives moving to a window at twice the scale.
            assert!(s.focus("editor-input").unwrap());
            s.press(windowing::Key::End, windowing::Mods::default())
                .unwrap();
            s.type_text(" kept").unwrap();
            let caret = s.windows[0].states.edit("editor-input").caret;
            assert_eq!(*text.borrow(), "draft kept");
            s.emit(&UiMsg::TabAction(
                group_strip("dock", 1),
                "editor".into(),
                "detach".into(),
            ));
            s.flush().unwrap();
            let floating = layout.borrow().floating[0].window.clone();
            let w = s.windows.iter().position(|w| w.key == floating).unwrap();
            let native = s.windows[w].native;
            s.handle(&windowing::Event::ScaleChanged {
                window: native.id(),
                scale: s.windows[0].theme.scale * 2.0,
            })
            .unwrap();
            s.step().unwrap();
            assert_eq!(s.windows[w].theme.scale, s.windows[0].theme.scale * 2.0);
            assert_eq!(s.windows[w].states.edit("editor-input").caret, caret);
            assert_eq!(
                s.windows[w].states.edit("editor-input").text.to_string(),
                "draft kept"
            );
            // Closing the floating window returns the pane, still with its caret.
            s.emit(&UiMsg::CloseRequested(floating.clone()));
            s.flush().unwrap();
            assert!(layout.borrow().floating.is_empty());
            assert!(s.windows.iter().all(|w| w.key != floating));
            assert_eq!(s.windows[0].states.edit("editor-input").caret, caret);
            assert_eq!(layout.borrow().panes().len(), 3);
            s.shutdown();
        })
    }
}
