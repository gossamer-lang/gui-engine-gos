// AT-SPI wire contracts, kept separate from transport and widget mutation.
#[cfg(target_os = "linux")]
mod linux {
    use crate::accessibility::Role;
    use crate::accessibility::atspi::Adapter;
    use crate::accessibility::provider::{self, Published, ROOT};
    use crate::accessibility::wire::Wire;
    use crate::geometry::Rect;
    use crate::strext::StrExt;
    const PREFIX: &str = "org.a11y.atspi.";
    fn string(w: &Wire) -> Option<String> {
        if let Wire::Text(s) = w {
            Some(s.to_string())
        } else {
            None
        }
    }
    fn integer(w: &Wire) -> Option<i64> {
        match w {
            Wire::Short(n) | Wire::Signed(n) | Wire::Unsigned(n) => Some(*n),
            _ => None,
        }
    }
    fn strings(values: &[String]) -> Wire {
        Wire::Items(
            "s",
            values
                .iter()
                .map(|s| Wire::Text(s.to_string()))
                .collect::<Vec<_>>(),
        )
    }
    fn states(n: &crate::accessibility::AccessNode) -> Wire {
        let (a, b) = provider::state_words(n);
        Wire::Items("u", vec![Wire::Unsigned(a), Wire::Unsigned(b)])
    }
    fn interfaces(p: &Published) -> Vec<String> {
        let mut names = vec!["Accessible", "Component"];
        if p.object == ROOT {
            names.push("Application")
        };
        if !p.node.actions.is_empty() {
            names.push("Action")
        };
        if p.node.text_start >= 0 && p.node.role != Role::PasswordField {
            names.push("Text");
            if !p.node.read_only {
                names.push("EditableText")
            }
        };
        if p.node.has_range {
            names.push("Value")
        };
        if selection_container(p.node.role) {
            names.push("Selection")
        };
        if p.node.role == Role::Table {
            names.push("Table")
        };
        names
            .iter()
            .map(|name| PREFIX.to_owned() + name)
            .collect::<Vec<_>>()
    }
    // A table publishes its column headers as a first header row and only the
    // rows on screen, each with its position among all rows.
    struct TableView {
        header: Vec<String>,
        rows: Vec<(i64, String, Published)>,
    }
    fn table_view(a: &Adapter, p: &Published) -> TableView {
        let mut header = vec![];
        let mut rows = vec![];
        for child in &p.children {
            let Some(item) = a.snapshot.objects.get(child) else {
                continue;
            };
            if item.node.role != Role::Row {
                continue;
            };
            let headers = item
                .children
                .iter()
                .filter(|c| match a.snapshot.objects.get(*c) {
                    Some(h) => h.node.role == Role::ColumnHeader,
                    None => false,
                })
                .cloned()
                .collect::<Vec<_>>();
            if !headers.is_empty() && header.is_empty() {
                header = headers;
                continue;
            };
            if item.node.pos_in_set > 0 {
                rows.push((item.node.pos_in_set - 1, child.clone(), item.clone()))
            }
        }
        TableView { header, rows }
    }
    fn table_columns(_a: &Adapter, t: &TableView) -> i64 {
        if !t.header.is_empty() {
            return t.header.len() as i64;
        };
        match t.rows.first() {
            Some((_, _, row)) => row.children.len() as i64,
            None => 0,
        }
    }
    fn table_row(t: &TableView, row: i64) -> Option<(String, Published)> {
        for (index, path, item) in &t.rows {
            if *index == row {
                return Some((path.to_string(), item.clone()));
            }
        }
        None
    }
    fn row_selected(t: &TableView, row: i64) -> bool {
        match table_row(t, row) {
            Some((_, item)) => item.node.selected,
            None => false,
        }
    }
    // Containers whose children are chosen: AT-SPI Selection reads and
    // changes which child is selected through each child's own state and
    // `select` action.
    fn selection_container(role: Role) -> bool {
        matches!(
            role,
            Role::ListBox
                | Role::TabList
                | Role::Tree
                | Role::Table
                | Role::Grid
                | Role::RadioGroup
        )
    }
    fn selection_children(a: &Adapter, p: &Published) -> Vec<(String, Published)> {
        let mut out = vec![];
        for child in &p.children {
            if let Some(item) = a.snapshot.objects.get(child) {
                out.push((child.clone(), item.clone()))
            }
        }
        out
    }
    fn selected_children(a: &Adapter, p: &Published) -> Vec<String> {
        let mut out = vec![];
        for (path, item) in selection_children(a, &p.clone()) {
            if item.node.selected {
                out.push(path)
            }
        }
        out
    }
    fn property(
        a: &Adapter,
        p: &Published,
        interface: &str,
        name: &str,
    ) -> Option<(&'static str, Wire)> {
        let n = &p.node;
        if interface == PREFIX.to_owned() + "Accessible" {
            return Some(match name {
                "Name" => ("s", Wire::Text(n.name.clone())),
                "Description" | "HelpText" => ("s", Wire::Text(n.description.clone())),
                "Parent" => (
                    "(so)",
                    if p.object == ROOT && !a.desktop_path.is_empty() {
                        Wire::Fields(vec![
                            Wire::Text(a.desktop_name.clone()),
                            Wire::Path(a.desktop_path.clone()),
                        ])
                    } else {
                        a.reference(&p.parent)
                    },
                ),
                "ChildCount" => ("i", Wire::Signed(p.children.len() as i64)),
                "Locale" => ("s", Wire::Text("C.UTF-8".to_string())),
                "AccessibleId" => ("s", Wire::Text(n.id.clone())),
                _ => return None,
            });
        }
        if interface == PREFIX.to_owned() + "Application" && p.object == ROOT {
            return Some(match name {
                "ToolkitName" => ("s", Wire::Text("gui-gos".to_string())),
                "Version" | "ToolkitVersion" => ("s", Wire::Text("0.1.0".to_string())),
                "AtspiVersion" => ("s", Wire::Text("2.1".to_string())),
                "Id" => ("i", Wire::Signed(a.application_id)),
                _ => return None,
            });
        }
        if interface == PREFIX.to_owned() + "Action" && !n.actions.is_empty() && name == "NActions"
        {
            return Some(("i", Wire::Signed(n.actions.len() as i64)));
        };
        if interface == PREFIX.to_owned() + "Text"
            && n.text_start >= 0
            && n.role != Role::PasswordField
        {
            return Some(match name {
                "CharacterCount" => ("i", Wire::Signed(provider::text_length(n))),
                "CaretOffset" => (
                    "i",
                    Wire::Signed(provider::node_character_offset(n, n.caret)),
                ),
                _ => return None,
            });
        }
        if interface == PREFIX.to_owned() + "Table" && n.role == Role::Table {
            let t = table_view(a, p);
            return Some(match name {
                "NRows" => ("i", Wire::Signed(n.set_size.max(t.rows.len() as i64))),
                "NColumns" => ("i", Wire::Signed(table_columns(a, &t))),
                "NSelectedRows" => (
                    "i",
                    Wire::Signed(
                        (t.rows
                            .iter()
                            .filter(|r| r.2.node.selected)
                            .cloned()
                            .collect::<Vec<_>>()
                            .len()) as i64,
                    ),
                ),
                "NSelectedColumns" => ("i", Wire::Signed(0)),
                "Caption" | "Summary" => ("(so)", a.reference("")),
                _ => return None,
            });
        }
        if interface == PREFIX.to_owned() + "Selection"
            && selection_container(n.role)
            && name == "NSelectedChildren"
        {
            return Some(("i", Wire::Signed((selected_children(a, p).len()) as i64)));
        }
        if interface == PREFIX.to_owned() + "Value" && n.has_range {
            if name == "Text" {
                return Some(("s", Wire::Text(provider::node_text(n))));
            };
            return Some((
                "d",
                Wire::Real(match name {
                    "CurrentValue" => n.range_now,
                    "MinimumValue" => n.range_min,
                    "MaximumValue" => n.range_max,
                    "MinimumIncrement" => 0.0,
                    _ => return None,
                }),
            ));
        }
        None
    }
    pub fn cache(a: &Adapter, p: &Published) -> Wire {
        Wire::Fields(vec![
            a.reference(&p.object),
            a.reference(ROOT),
            a.reference(&p.parent),
            Wire::Signed(p.index),
            Wire::Signed(p.children.len() as i64),
            strings(&interfaces(p)),
            Wire::Text(p.node.name.clone()),
            Wire::Unsigned(if p.object == ROOT {
                75
            } else {
                provider::role(p.node.role)
            }),
            Wire::Text(p.node.description.clone()),
            states(&p.node),
        ])
    }
    pub fn query(
        a: &mut Adapter,
        path: &str,
        interface: &str,
        member: &str,
        args: &[Wire],
    ) -> Option<Vec<Wire>> {
        if interface == "org.freedesktop.DBus.Peer" && member == "Ping" && args.is_empty() {
            return Some(vec![]);
        };
        if interface == "org.freedesktop.DBus.Introspectable"
            && member == "Introspect"
            && args.is_empty()
        {
            let children = match path {
                "/" => vec!["org"],
                "/org" => vec!["a11y"],
                "/org/a11y" => vec!["atspi"],
                "/org/a11y/atspi" => vec!["accessible", "cache"],
                "/org/a11y/atspi/accessible" => {
                    let mut nodes = vec![];
                    let prefix = "/org/a11y/atspi/accessible/";
                    for object in a.snapshot.objects.keys() {
                        nodes.push(object.substring(prefix.byte_len(), object.byte_len()))
                    }
                    nodes.sort();
                    nodes
                }
                _ => vec![],
            };
            let supported = if path == "/org/a11y/atspi/cache" {
                vec![PREFIX.to_owned() + "Cache"]
            } else if let Some(p) = a.snapshot.objects.get(path) {
                interfaces(p)
            } else if !children.is_empty() {
                vec![]
            } else {
                return None;
            };
            let extents = match a.snapshot.objects.get(path) {
                Some(p) => !p.node.text_boxes.is_empty() && p.node.role != Role::PasswordField,
                None => false,
            };
            return Some(vec![Wire::Text(
                crate::accessibility::introspection::describe(
                    &supported,
                    &children.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
                    extents,
                ),
            )]);
        }
        if interface == PREFIX.to_owned() + "Cache"
            && path == "/org/a11y/atspi/cache"
            && member == "GetItems"
            && args.is_empty()
        {
            let mut items = vec![];
            for p in a.snapshot.objects.values() {
                items.push(cache(a, p))
            }
            return Some(vec![Wire::Items("((so)(so)(so)iiassusau)", items)]);
        }
        let p = a.snapshot.objects.get(path)?;
        let n = &p.node;
        if interface == "org.freedesktop.DBus.Properties" {
            if member == "Get" && args.len() == 2 {
                let (sig, value) = property(a, p, &string(&args[0])?, &string(&args[1])?)?;
                return Some(vec![Wire::Variant(sig, vec![value])]);
            }
            if member == "GetAll" && args.len() == 1 {
                let iface = string(&args[0])?;
                if !interfaces(p).contains(&iface) {
                    return None;
                };
                let mut entries = vec![];
                for name in vec![
                    "Name",
                    "Description",
                    "Parent",
                    "ChildCount",
                    "Locale",
                    "AccessibleId",
                    "HelpText",
                    "ToolkitName",
                    "Version",
                    "ToolkitVersion",
                    "AtspiVersion",
                    "Id",
                    "NActions",
                    "CharacterCount",
                    "CaretOffset",
                    "CurrentValue",
                    "MinimumValue",
                    "MaximumValue",
                    "MinimumIncrement",
                    "Text",
                    "NSelectedChildren",
                    "NRows",
                    "NColumns",
                    "NSelectedRows",
                    "NSelectedColumns",
                    "Caption",
                    "Summary",
                ] {
                    if let Some((sig, value)) = property(a, p, &iface, name) {
                        entries.push(Wire::Entry(vec![
                            Wire::Text(name.to_string()),
                            Wire::Variant(sig, vec![value]),
                        ]))
                    }
                }
                return Some(vec![Wire::Items("{sv}", entries)]);
            }
            if member == "Set" && args.len() == 3 {
                let iface = string(&args[0])?;
                let name = string(&args[1])?;
                if iface == PREFIX.to_owned() + "Application" && path == ROOT && name == "Id" {
                    if let Wire::Variant(_, values) = &args[2]
                        && values.len() == 1
                    {
                        a.application_id = integer(&values[0])?;
                        return Some(vec![]);
                    };
                    return None;
                };
                if iface == PREFIX.to_owned() + "Value"
                    && name == "CurrentValue"
                    && let Wire::Variant(_, values) = &args[2]
                    && values.len() == 1
                    && let Wire::Real(v) = values[0].clone()
                    && v >= n.range_min
                    && v <= n.range_max
                    && a.queue(path, "set-value", &format!("{}", v), 0, 0)
                {
                    return Some(vec![]);
                }
            }
            return None;
        }
        if interface == PREFIX.to_owned() + "Accessible" {
            return Some(match member {
                "GetChildren" if args.is_empty() => {
                    let mut children = vec![];
                    for child in &p.children {
                        children.push(a.reference(child))
                    }
                    vec![Wire::Items("(so)", children)]
                }
                "GetChildAtIndex" if args.len() == 1 => {
                    let i = integer(&args[0])?;
                    vec![a.reference(if i >= 0 && i < p.children.len() as i64 {
                        &p.children[i as usize]
                    } else {
                        ""
                    })]
                }
                "GetIndexInParent" if args.is_empty() => vec![Wire::Signed(p.index)],
                "GetRole" if args.is_empty() => vec![Wire::Unsigned(if path == ROOT {
                    75
                } else {
                    provider::role(n.role)
                })],
                "GetRoleName" | "GetLocalizedRoleName" if args.is_empty() => {
                    vec![Wire::Text(if path == ROOT {
                        "application".to_string()
                    } else {
                        n.role.name().to_string()
                    })]
                }
                "GetState" if args.is_empty() => vec![states(n)],
                "GetAttributes" if args.is_empty() => {
                    let mut attributes = vec![];
                    if !n.live.is_empty() {
                        attributes.push(Wire::Entry(vec![
                            Wire::Text("live".to_string()),
                            Wire::Text(n.live.clone()),
                        ]))
                    };
                    vec![Wire::Items("{ss}", attributes)]
                }
                "GetRelationSet" if args.is_empty() => {
                    let mut relations = vec![];
                    for (kind, id) in [
                        (2, n.labelled_by.clone()),
                        (18, n.described_by.clone()),
                        (3, n.controls.clone()),
                    ] {
                        if let Some(related) = a.snapshot.related(&p.window, &id) {
                            relations.push(Wire::Fields(vec![
                                Wire::Unsigned(kind),
                                Wire::Items("(so)", vec![a.reference(&related)]),
                            ]))
                        }
                    }
                    vec![Wire::Items("(ua(so))", relations)]
                }
                "GetApplication" if args.is_empty() => vec![a.reference(ROOT)],
                "GetInterfaces" if args.is_empty() => vec![strings(&interfaces(p))],
                _ => return None,
            });
        }
        if interface == PREFIX.to_owned() + "Application"
            && path == ROOT
            && member == "GetLocale"
            && args.len() == 1
        {
            if integer(&args[0])? < 0 {
                return None;
            };
            return Some(vec![Wire::Text("C.UTF-8".to_string())]);
        }
        if interface == PREFIX.to_owned() + "Action" && !n.actions.is_empty() {
            if member == "GetNActions" && args.is_empty() {
                return Some(vec![Wire::Signed(n.actions.len() as i64)]);
            };
            if member == "GetActions" && args.is_empty() {
                return Some(vec![Wire::Items(
                    "(sss)",
                    n.actions
                        .iter()
                        .map(|action| {
                            Wire::Fields(vec![
                                Wire::Text(action.to_string()),
                                Wire::Text(action.to_string()),
                                Wire::Text("".to_string()),
                            ])
                        })
                        .collect::<Vec<_>>(),
                )]);
            };
            if args.len() != 1 {
                return None;
            };
            let i = integer(&args[0])?;
            if i < 0 || i >= n.actions.len() as i64 {
                return None;
            };
            return Some(match member {
                "GetName" | "GetLocalizedName" | "GetDescription" => {
                    vec![Wire::Text(n.actions[i as usize].clone())]
                }
                "GetKeyBinding" => vec![Wire::Text("".to_string())],
                "DoAction" => {
                    let action = n.actions[i as usize].clone();
                    vec![Wire::Flag(a.queue(path, &action, "", 0, 0))]
                }
                _ => return None,
            });
        }
        if interface == PREFIX.to_owned() + "Component" {
            if member == "GrabFocus" && args.is_empty() {
                return Some(vec![Wire::Flag(a.queue(path, "focus", "", 0, 0))]);
            };
            if member == "GetLayer" && args.is_empty() {
                return Some(vec![Wire::Unsigned(3)]);
            };
            if member == "GetMDIZOrder" && args.is_empty() {
                return Some(vec![Wire::Short(0)]);
            };
            if member == "GetAlpha" && args.is_empty() {
                return Some(vec![Wire::Real(1.0)]);
            };
            if member == "GetSize" && args.is_empty() {
                return Some(vec![
                    Wire::Signed(n.bounds.width),
                    Wire::Signed(n.bounds.height),
                ]);
            };
            if (member == "GetExtents" || member == "GetPosition") && args.len() == 1 {
                let r = a.snapshot.extents(p, integer(&args[0])?);
                return Some(if member == "GetExtents" {
                    vec![Wire::Fields(vec![
                        Wire::Signed(r.x),
                        Wire::Signed(r.y),
                        Wire::Signed(r.width),
                        Wire::Signed(r.height),
                    ])]
                } else {
                    vec![Wire::Signed(r.x), Wire::Signed(r.y)]
                });
            }
            if (member == "Contains" || member == "GetAccessibleAtPoint") && args.len() == 3 {
                let x = integer(&args[0])?;
                let y = integer(&args[1])?;
                let coords = integer(&args[2])?;
                if member == "Contains" {
                    return Some(vec![Wire::Flag(
                        a.snapshot.extents(p, coords).contains(x, y),
                    )]);
                };
                for child in &p.children {
                    let item = a.snapshot.objects.get(child)?;
                    if a.snapshot.extents(item, coords).contains(x, y) {
                        return Some(vec![a.reference(child)]);
                    }
                }
                return Some(vec![a.reference("")]);
            }
        }
        if interface == PREFIX.to_owned() + "Table" && n.role == Role::Table {
            let t = table_view(a, p);
            let columns = table_columns(a, &t);
            let selected_rows = t
                .rows
                .iter()
                .filter(|r| r.2.node.selected)
                .cloned()
                .collect::<Vec<_>>()
                .iter()
                .map(|r| Wire::Signed(r.0))
                .collect::<Vec<_>>();
            if args.is_empty() {
                return Some(match member {
                    "GetSelectedRows" => vec![Wire::Items("i", selected_rows)],
                    "GetSelectedColumns" => vec![Wire::Items("i", vec![])],
                    _ => return None,
                });
            }
            let first = integer(&args[0])?;
            if args.len() == 1 {
                return Some(match member {
                    "GetRowAtIndex" => {
                        vec![Wire::Signed(if columns > 0 { first / columns } else { -1 })]
                    }
                    "GetColumnAtIndex" => {
                        vec![Wire::Signed(if columns > 0 { first % columns } else { -1 })]
                    }
                    "GetColumnDescription" => {
                        vec![Wire::Text(if first >= 0 && first < t.header.len() as i64 {
                            match a.snapshot.objects.get(&t.header[first as usize]) {
                                Some(h) => h.node.name.clone(),
                                None => "".to_string(),
                            }
                        } else {
                            "".to_string()
                        })]
                    }
                    "GetRowDescription" => vec![Wire::Text("".to_string())],
                    "GetColumnHeader" => {
                        vec![a.reference(if first >= 0 && first < t.header.len() as i64 {
                            &t.header[first as usize]
                        } else {
                            ""
                        })]
                    }
                    "GetRowHeader" => vec![a.reference("")],
                    "IsRowSelected" => vec![Wire::Flag(row_selected(&t, first))],
                    "IsColumnSelected"
                    | "AddColumnSelection"
                    | "RemoveColumnSelection"
                    | "RemoveRowSelection" => vec![Wire::Flag(false)],
                    "AddRowSelection" => {
                        let Some((path, row)) = table_row(&t, first) else {
                            return Some(vec![Wire::Flag(false)]);
                        };
                        vec![Wire::Flag(
                            row.node.actions.iter().any(|x| x == "select")
                                && a.queue(&path, "select", "", 0, 0),
                        )]
                    }
                    "GetRowColumnExtentsAtIndex" => {
                        let row = if columns > 0 { first / columns } else { -1 };
                        let col = if columns > 0 { first % columns } else { -1 };
                        let selected = row_selected(&t, row);
                        vec![
                            Wire::Flag(row >= 0 && col >= 0 && col < columns),
                            Wire::Signed(row),
                            Wire::Signed(col),
                            Wire::Signed(1),
                            Wire::Signed(1),
                            Wire::Flag(selected),
                        ]
                    }
                    _ => return None,
                });
            }
            if args.len() != 2 {
                return None;
            };
            let second = integer(&args[1])?;
            return Some(match member {
                "GetAccessibleAt" => {
                    let cell = match table_row(&t, first) {
                        Some((_, row)) if second >= 0 && second < row.children.len() as i64 => {
                            row.children[second as usize].clone()
                        }
                        _ => "".to_string(),
                    };
                    vec![a.reference(&cell)]
                }
                "GetIndexAt" => vec![Wire::Signed(if second >= 0 && second < columns {
                    first * columns + second
                } else {
                    -1
                })],
                "GetRowExtentAt" | "GetColumnExtentAt" => vec![Wire::Signed(1)],
                "IsSelected" => vec![Wire::Flag(row_selected(&t, first))],
                _ => return None,
            });
        }
        if interface == PREFIX.to_owned() + "Selection" && selection_container(n.role) {
            let items = selection_children(a, &p.clone());
            let selected = selected_children(a, p);
            if member == "GetNSelectedChildren" && args.is_empty() {
                return Some(vec![Wire::Signed(selected.len() as i64)]);
            };
            // One child is chosen at a time in these containers, so nothing
            // deselects or selects every child.
            if (member == "SelectAll" || member == "ClearSelection") && args.is_empty() {
                return Some(vec![Wire::Flag(false)]);
            };
            if args.len() != 1 {
                return None;
            };
            let i = integer(&args[0])?;
            return Some(match member {
                "GetSelectedChild" => vec![a.reference(if i >= 0 && i < selected.len() as i64 {
                    &selected[i as usize]
                } else {
                    ""
                })],
                "IsChildSelected" => vec![Wire::Flag(
                    i >= 0 && i < items.len() as i64 && items[i as usize].1.node.selected,
                )],
                "SelectChild" => {
                    let chosen = i >= 0
                        && i < items.len() as i64
                        && items[i as usize]
                            .1
                            .node
                            .actions
                            .iter()
                            .any(|x| x == "select");
                    vec![Wire::Flag(
                        chosen && a.queue(&items[i as usize].0, "select", "", 0, 0),
                    )]
                }
                "DeselectChild" | "DeselectSelectedChild" => vec![Wire::Flag(false)],
                _ => return None,
            });
        }
        if interface == PREFIX.to_owned() + "Text"
            && n.text_start >= 0
            && n.role != Role::PasswordField
        {
            if member == "GetText" && args.len() == 2 {
                return Some(vec![Wire::Text(provider::node_text_range(
                    n,
                    integer(&args[0])?,
                    integer(&args[1])?,
                ))]);
            };
            if !n.text_boxes.is_empty() {
                let item = a.snapshot.objects.get(path)?;
                let boxed = |r: Rect| {
                    vec![
                        Wire::Signed(r.x),
                        Wire::Signed(r.y),
                        Wire::Signed(r.width),
                        Wire::Signed(r.height),
                    ]
                };
                if member == "GetCharacterExtents" && args.len() == 2 {
                    let offset = integer(&args[0])?;
                    let r = provider::range_box(&n.text_boxes, offset, offset + 1)?;
                    return Some(boxed(a.snapshot.place(item, r, integer(&args[1])?)));
                }
                if member == "GetRangeExtents" && args.len() == 3 {
                    let r =
                        provider::range_box(&n.text_boxes, integer(&args[0])?, integer(&args[1])?)?;
                    return Some(boxed(a.snapshot.place(item, r, integer(&args[2])?)));
                }
                if member == "GetOffsetAtPoint" && args.len() == 3 {
                    // The point comes in the asked space; boxes are in the window's.
                    let origin = a
                        .snapshot
                        .place(item, Rect::new(0, 0, 0, 0), integer(&args[2])?);
                    let x = integer(&args[0])? - origin.x;
                    let y = integer(&args[1])? - origin.y;
                    return Some(vec![Wire::Signed(provider::offset_at(&n.text_boxes, x, y))]);
                }
            }
            if member == "GetCharacterAtOffset" && args.len() == 1 {
                let offset = integer(&args[0])?;
                let value = usize::try_from(offset)
                    .ok()
                    .and_then(|o| {
                        provider::node_text_range(n, o as i64, o as i64 + 1)
                            .chars()
                            .next()
                    })
                    .map_or(0, |ch| ch as i64);
                return Some(vec![Wire::Signed(value)]);
            }
            if member == "GetStringAtOffset" && args.len() == 2 {
                // ATSPI_TEXT_GRANULARITY_LINE of single-line text is all of it,
                // its one paragraph; wrapped lines need the laid-out glyphs.
                let granularity = integer(&args[1])?;
                let granularity = if granularity == 3 && !n.multiline {
                    4
                } else {
                    granularity
                };
                let (text, start, end) = provider::text_segment(
                    &provider::node_text(n),
                    integer(&args[0])?,
                    granularity,
                )?;
                return Some(vec![
                    Wire::Text(text),
                    Wire::Signed(start),
                    Wire::Signed(end),
                ]);
            }
            if member == "RemoveSelection" && args.len() == 1 && integer(&args[0])? == 0 {
                let caret = provider::node_character_offset(n, n.caret);
                return Some(vec![Wire::Flag(a.queue(
                    path,
                    "select-text",
                    "",
                    caret,
                    caret,
                ))]);
            };
            if member == "GetNSelections" && args.is_empty() {
                return Some(vec![Wire::Signed(if n.text_start != n.text_end {
                    1
                } else {
                    0
                })]);
            };
            if member == "GetSelection" && args.len() == 1 && integer(&args[0])? == 0 {
                return Some(vec![
                    Wire::Signed(provider::node_character_offset(n, n.text_start)),
                    Wire::Signed(provider::node_character_offset(n, n.text_end)),
                ]);
            };
            if member == "SetCaretOffset" && args.len() == 1 {
                let pos = integer(&args[0])?;
                return Some(vec![Wire::Flag(a.queue(path, "select-text", "", pos, pos))]);
            };
            if (member == "AddSelection" && args.len() == 2)
                || (member == "SetSelection" && args.len() == 3 && integer(&args[0])? == 0)
            {
                let i = args.len() - 2;
                return Some(vec![Wire::Flag(a.queue(
                    path,
                    "select-text",
                    "",
                    integer(&args[i])?,
                    integer(&args[i + 1])?,
                ))]);
            }
        }
        if interface == PREFIX.to_owned() + "EditableText"
            && n.text_start >= 0
            && !n.read_only
            && n.role != Role::PasswordField
        {
            if member == "SetTextContents" && args.len() == 1 {
                return Some(vec![Wire::Flag(a.queue(
                    path,
                    "set-text",
                    &string(&args[0])?,
                    0,
                    -1,
                ))]);
            };
            if member == "InsertText" && args.len() == 3 {
                let at = integer(&args[0])?;
                let text = string(&args[1])?;
                let len = integer(&args[2])?;
                return Some(vec![Wire::Flag(a.queue(
                    path,
                    "replace-text",
                    &provider::text_range(&text, 0, len),
                    at,
                    at,
                ))]);
            };
            if member == "DeleteText" && args.len() == 2 {
                return Some(vec![Wire::Flag(a.queue(
                    path,
                    "replace-text",
                    "",
                    integer(&args[0])?,
                    integer(&args[1])?,
                ))]);
            }
        }
        None
    }
    #[cfg(test)]
    mod tests {
        use super::query;
        use crate::accessibility::atspi::Adapter;
        use crate::accessibility::wire::Wire;
        use crate::accessibility::{AccessNode, Role};
        use crate::geometry::Rect;

        #[test]
        fn atspi_wire_queries_and_actions_are_typed_and_bounded() {
            let mut a = Adapter::detached("test");
            let mut field = AccessNode::control(Role::TextField, "text", "Name", "focus");
            field.value = "aé🙂z".to_string();
            field.text_start = 1;
            field.text_end = 7;
            field.caret = 7;
            a.snapshot.publish("main", &field);
            let path = a.snapshot.related("main", "text").unwrap();
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Text",
                    "GetText",
                    &[Wire::Signed(1), Wire::Signed(3)]
                ),
                Some(vec![Wire::Text("é🙂".to_string())])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Text",
                    "GetSelection",
                    &[Wire::Signed(0)]
                ),
                Some(vec![Wire::Signed(1), Wire::Signed(3)])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Action",
                    "DoAction",
                    &[Wire::Signed(99)]
                ),
                None
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.EditableText",
                    "SetTextContents",
                    &[Wire::Text("updated".to_string())]
                ),
                Some(vec![Wire::Flag(true)])
            );
            assert_eq!(a.poll()[0].value, "updated");
            field.read_only = true;
            a.snapshot.publish("main", &field);
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.EditableText",
                    "SetTextContents",
                    &[Wire::Text("no".to_string())]
                ),
                None
            );
            a.snapshot.close("main");
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Text",
                    "GetText",
                    &[Wire::Signed(0), Wire::Signed(-1)]
                ),
                None
            )
        }

        #[test]
        fn selection_reads_and_chooses_children_through_their_select_action() {
            let mut a = Adapter::detached("test");
            // As in a collection, the focusable list owns its items' actions.
            let mut list = AccessNode::control(Role::ListBox, "list", "Fruit", "");
            for (i, name) in ["Apple", "Pear", "Plum"].iter().enumerate() {
                let mut item = AccessNode::new(Role::ListItem, name);
                item.id = format!("item{}", i);
                item.selected = i == 1;
                item.actions = vec!["select".to_string()];
                list.children.push(item)
            }
            a.snapshot.publish("main", &list);
            let path = a.snapshot.related("main", "list").unwrap();
            let pear = a.snapshot.related("main", "item1").unwrap();
            let plum = a.snapshot.related("main", "item2").unwrap();
            assert!(
                introspect(&mut a, &path).contains("<interface name=\"org.a11y.atspi.Selection\">")
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Selection",
                    "GetNSelectedChildren",
                    &[]
                ),
                Some(vec![Wire::Signed(1)])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Selection",
                    "GetSelectedChild",
                    &[Wire::Signed(0)]
                ),
                Some(vec![a.reference(&pear)])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Selection",
                    "IsChildSelected",
                    &[Wire::Signed(0)]
                ),
                Some(vec![Wire::Flag(false)])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Selection",
                    "SelectChild",
                    &[Wire::Signed(2)]
                ),
                Some(vec![Wire::Flag(true)])
            );
            let action = &a.poll()[0];
            assert_eq!(action.kind, "select");
            assert_eq!(a.snapshot.related("main", &action.target), Some(plum));
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Selection",
                    "SelectChild",
                    &[Wire::Signed(9)]
                ),
                Some(vec![Wire::Flag(false)])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Selection",
                    "ClearSelection",
                    &[]
                ),
                Some(vec![Wire::Flag(false)])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.freedesktop.DBus.Properties",
                    "Get",
                    &[
                        Wire::Text("org.a11y.atspi.Selection".to_string()),
                        Wire::Text("NSelectedChildren".to_string())
                    ]
                ),
                Some(vec![Wire::Variant("i", vec![Wire::Signed(1)])])
            )
        }

        #[test]
        fn a_virtual_table_answers_rows_columns_and_cells_by_position() {
            let mut a = Adapter::detached("test");
            let mut table = AccessNode::control(Role::Table, "files", "Files", "");
            table.set_size = 100;
            let mut header = AccessNode::new(Role::Row, "");
            for title in ["Name", "Size"] {
                let mut h = AccessNode::new(Role::ColumnHeader, title);
                h.id = format!("files#col:{}", title);
                header.children.push(h)
            }
            table.children.push(header);
            for i in 5..7 {
                let mut row = AccessNode::new(Role::Row, &format!("file{}", i));
                row.id = format!("files#k{}", i);
                row.pos_in_set = i + 1;
                row.set_size = 100;
                row.selected = i == 6;
                row.actions = vec!["select".to_string()];
                row.children
                    .push(AccessNode::new(Role::Cell, &format!("file{}.txt", i)));
                row.children
                    .push(AccessNode::new(Role::Cell, &format!("{} KB", i)));
                table.children.push(row)
            }
            a.snapshot.publish("main", &table);
            let path = a.snapshot.related("main", "files").unwrap();
            let prop = |a: &mut Adapter, name: String| {
                query(
                    a,
                    &path,
                    "org.freedesktop.DBus.Properties",
                    "Get",
                    &[
                        Wire::Text("org.a11y.atspi.Table".to_string()),
                        Wire::Text(name),
                    ],
                )
            };
            assert_eq!(
                prop(&mut a, "NRows".to_string()),
                Some(vec![Wire::Variant("i", vec![Wire::Signed(100)])])
            );
            assert_eq!(
                prop(&mut a, "NColumns".to_string()),
                Some(vec![Wire::Variant("i", vec![Wire::Signed(2)])])
            );
            assert_eq!(
                prop(&mut a, "NSelectedRows".to_string()),
                Some(vec![Wire::Variant("i", vec![Wire::Signed(1)])])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Table",
                    "GetColumnDescription",
                    &[Wire::Signed(1)]
                ),
                Some(vec![Wire::Text("Size".to_string())])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Table",
                    "GetIndexAt",
                    &[Wire::Signed(6), Wire::Signed(1)]
                ),
                Some(vec![Wire::Signed(13)])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Table",
                    "GetRowAtIndex",
                    &[Wire::Signed(13)]
                ),
                Some(vec![Wire::Signed(6)])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Table",
                    "GetSelectedRows",
                    &[]
                ),
                Some(vec![Wire::Items("i", vec![Wire::Signed(6)])])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Table",
                    "IsRowSelected",
                    &[Wire::Signed(5)]
                ),
                Some(vec![Wire::Flag(false)])
            );
            let cell = query(
                &mut a,
                &path,
                "org.a11y.atspi.Table",
                "GetAccessibleAt",
                &[Wire::Signed(5), Wire::Signed(1)],
            )
            .unwrap();
            assert!(cell != vec![a.reference("")]);
            // A row off screen has no published cell.
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Table",
                    "GetAccessibleAt",
                    &[Wire::Signed(50), Wire::Signed(0)]
                ),
                Some(vec![a.reference("")])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Table",
                    "AddRowSelection",
                    &[Wire::Signed(5)]
                ),
                Some(vec![Wire::Flag(true)])
            );
            assert_eq!(a.poll()[0].target, "files#k5");
            assert!(introspect(&mut a, &path).contains("<interface name=\"org.a11y.atspi.Table\">"))
        }

        #[test]
        fn laid_out_text_answers_character_range_and_point_extents() {
            let mut a = Adapter::detached("test");
            let mut field = AccessNode::control(Role::TextField, "name", "Name", "focus");
            field.bounds = Rect::new(10, 20, 100, 30);
            field.value = "héllo".to_string();
            field.text_start = 0;
            field.text_end = 0;
            field.caret = 0;
            field.text_boxes = vec![
                Rect::new(14, 25, 8, 20),
                Rect::new(22, 25, 8, 20),
                Rect::new(30, 25, 4, 20),
                Rect::new(34, 25, 4, 20),
                Rect::new(38, 25, 8, 20),
            ];
            a.snapshot.publish("main", &field);
            a.snapshot.origins.insert("main".to_string(), (100, 200));
            let path = a.snapshot.related("main", "name").unwrap();
            assert!(introspect(&mut a, &path).contains("GetCharacterExtents"));
            let text = "org.a11y.atspi.Text";
            // In the window, on the screen (window origin 100, 200), and in the parent's space.
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    text,
                    "GetCharacterExtents",
                    &[Wire::Signed(1), Wire::Unsigned(1)]
                ),
                Some(vec![
                    Wire::Signed(22),
                    Wire::Signed(25),
                    Wire::Signed(8),
                    Wire::Signed(20)
                ])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    text,
                    "GetCharacterExtents",
                    &[Wire::Signed(1), Wire::Unsigned(0)]
                ),
                Some(vec![
                    Wire::Signed(122),
                    Wire::Signed(225),
                    Wire::Signed(8),
                    Wire::Signed(20)
                ])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    text,
                    "GetRangeExtents",
                    &[Wire::Signed(1), Wire::Signed(4), Wire::Unsigned(1)]
                ),
                Some(vec![
                    Wire::Signed(22),
                    Wire::Signed(25),
                    Wire::Signed(16),
                    Wire::Signed(20)
                ])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    text,
                    "GetCharacterExtents",
                    &[Wire::Signed(5), Wire::Unsigned(1)]
                ),
                None
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    text,
                    "GetOffsetAtPoint",
                    &[Wire::Signed(131), Wire::Signed(230), Wire::Unsigned(0)]
                ),
                Some(vec![Wire::Signed(2)])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    text,
                    "GetOffsetAtPoint",
                    &[Wire::Signed(80), Wire::Signed(30), Wire::Unsigned(1)]
                ),
                Some(vec![Wire::Signed(4)])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    text,
                    "GetOffsetAtPoint",
                    &[Wire::Signed(80), Wire::Signed(90), Wire::Unsigned(1)]
                ),
                Some(vec![Wire::Signed(-1)])
            )
        }

        #[test]
        fn a_live_region_reports_its_politeness_as_an_attribute() {
            let mut a = Adapter::detached("test");
            let mut root = AccessNode::new(Role::Group, "root");
            let mut status = AccessNode::new(Role::Status, "Saved notes.txt");
            status.id = "status".to_string();
            status.live = "polite".to_string();
            let mut quiet = AccessNode::new(Role::Label, "Title");
            quiet.id = "title".to_string();
            root.children.push(status);
            root.children.push(quiet);
            a.snapshot.publish("main", &root);
            let live = a.snapshot.related("main", "status").unwrap();
            let plain = a.snapshot.related("main", "title").unwrap();
            assert_eq!(
                query(
                    &mut a,
                    &live,
                    "org.a11y.atspi.Accessible",
                    "GetAttributes",
                    &[]
                ),
                Some(vec![Wire::Items(
                    "{ss}",
                    vec![Wire::Entry(vec![
                        Wire::Text("live".to_string()),
                        Wire::Text("polite".to_string())
                    ])]
                )])
            );
            assert_eq!(
                query(
                    &mut a,
                    &plain,
                    "org.a11y.atspi.Accessible",
                    "GetAttributes",
                    &[]
                ),
                Some(vec![Wire::Items("{ss}", vec![])])
            )
        }

        fn introspect(a: &mut Adapter, path: &str) -> String {
            let reply = query(
                a,
                path,
                "org.freedesktop.DBus.Introspectable",
                "Introspect",
                &[],
            )
            .unwrap();
            if let Wire::Text(ref xml) = reply[0] {
                xml.to_string()
            } else {
                "".to_string()
            }
        }

        #[test]
        fn introspection_tracks_live_interfaces_and_native_signatures() {
            let mut a = Adapter::detached("test");
            let mut field = AccessNode::new(Role::TextField, "Field");
            field.id = "field".to_string();
            field.bounds = crate::geometry::Rect::new(0, 0, 100, 30);
            field.value = "private value".to_string();
            field.text_start = 0;
            field.text_end = 0;
            field.caret = 0;
            a.snapshot.publish("main", &field);
            let path = a.snapshot.related("main", "field").unwrap();
            let xml = introspect(&mut a, &path);
            assert!(xml.contains("<interface name=\"org.a11y.atspi.Accessible\">"));
            assert!(xml.contains("<interface name=\"org.a11y.atspi.Text\">"));
            assert!(xml.contains("<interface name=\"org.a11y.atspi.EditableText\">"));
            assert!(
                xml.contains("<method name=\"GetMDIZOrder\"><arg direction=\"out\" type=\"n\"/>")
            );
            assert!(!xml.contains("private value"));
            assert!(!xml.contains("GetCharacterExtents"));
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.a11y.atspi.Component",
                    "GetMDIZOrder",
                    &[]
                ),
                Some(vec![Wire::Short(0)])
            );
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.freedesktop.DBus.Properties",
                    "GetAll",
                    &[Wire::Text("org.example.Unknown".to_string())]
                ),
                None
            );
            let app = introspect(&mut a, crate::accessibility::provider::ROOT);
            assert!(app.contains("<property name=\"ToolkitVersion\" type=\"s\" access=\"read\"/>"));
            assert_eq!(
                query(
                    &mut a,
                    crate::accessibility::provider::ROOT,
                    "org.freedesktop.DBus.Properties",
                    "Get",
                    &[
                        Wire::Text("org.a11y.atspi.Application".to_string()),
                        Wire::Text("AtspiVersion".to_string())
                    ]
                ),
                Some(vec![Wire::Variant(
                    "s",
                    vec![Wire::Text("2.1".to_string())]
                )])
            );
            assert_eq!(
                query(
                    &mut a,
                    crate::accessibility::provider::ROOT,
                    "org.a11y.atspi.Application",
                    "GetLocale",
                    &[Wire::Unsigned(0)]
                ),
                Some(vec![Wire::Text("C.UTF-8".to_string())])
            );
            assert_eq!(
                query(
                    &mut a,
                    crate::accessibility::provider::ROOT,
                    "org.a11y.atspi.Application",
                    "GetLocale",
                    &[Wire::Text("invalid".to_string())]
                ),
                None
            );
            assert_eq!(
                query(
                    &mut a,
                    crate::accessibility::provider::ROOT,
                    "org.a11y.atspi.Application",
                    "GetLocale",
                    &[Wire::Signed(-1)]
                ),
                None
            );
            let cache = introspect(&mut a, "/org/a11y/atspi/cache");
            assert!(cache.contains("a((so)(so)(so)iiassusau)"));
            assert!(!cache.contains("<interface name=\"org.a11y.atspi.Accessible\">"));
            assert!(
                introspect(&mut a, "/org/a11y/atspi/accessible").contains("<node name=\"n1\"/>")
            );
            field.read_only = true;
            a.snapshot.publish("main", &field);
            assert!(!introspect(&mut a, &path).contains("org.a11y.atspi.EditableText"));
            field.role = Role::PasswordField;
            a.snapshot.publish("main", &field);
            assert!(!introspect(&mut a, &path).contains("org.a11y.atspi.Text"));
            a.snapshot.close("main");
            assert_eq!(
                query(
                    &mut a,
                    &path,
                    "org.freedesktop.DBus.Introspectable",
                    "Introspect",
                    &[]
                ),
                None
            );
            assert!(
                !introspect(&mut a, "/org/a11y/atspi/accessible").contains("<node name=\"n1\"/>")
            )
        }
    }
}
#[cfg(target_os = "linux")]
pub fn query(
    a: &mut crate::accessibility::atspi::Adapter,
    path: &str,
    interface: &str,
    member: &str,
    args: &[crate::accessibility::wire::Wire],
) -> Option<Vec<crate::accessibility::wire::Wire>> {
    linux::query(a, path, interface, member, args)
}

#[cfg(target_os = "linux")]
pub fn cache_entry(
    a: &crate::accessibility::atspi::Adapter,
    item: &crate::accessibility::provider::Published,
) -> crate::accessibility::wire::Wire {
    linux::cache(a, &item.clone())
}
