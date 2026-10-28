// Introspection of the methods the native provider implements. Signatures
// follow GNOME at-spi2-core/xml; unsupported interfaces are not advertised.
#[cfg(target_os = "linux")]
mod linux {
    use crate::strext::StrExt;
    fn method(name: &str, input: &str, output: &[String]) -> String {
        let mut xml = format!("<method name=\"{}\">", name);
        for kind in input.chars() {
            xml += &format!("<arg direction=\"in\" type=\"{}\"/>", kind)
        }
        for kind in output {
            xml += &format!("<arg direction=\"out\" type=\"{}\"/>", kind)
        }
        xml + "</method>"
    }
    fn methods(name: &str, extents: bool) -> Vec<(String, String, Vec<String>)> {
        if name == "Text" && extents {
            let mut all = methods(name, false);
            all.push((
                "GetCharacterExtents".to_string(),
                "iu".to_string(),
                vec![
                    "i".to_string(),
                    "i".to_string(),
                    "i".to_string(),
                    "i".to_string(),
                ],
            ));
            all.push((
                "GetRangeExtents".to_string(),
                "iiu".to_string(),
                vec![
                    "i".to_string(),
                    "i".to_string(),
                    "i".to_string(),
                    "i".to_string(),
                ],
            ));
            all.push((
                "GetOffsetAtPoint".to_string(),
                "iiu".to_string(),
                vec!["i".to_string()],
            ));
            return all;
        }
        match name {
            "Accessible" => vec![
                (
                    "GetChildAtIndex".to_string(),
                    "i".to_string(),
                    vec!["(so)".to_string()],
                ),
                (
                    "GetChildren".to_string(),
                    "".to_string(),
                    vec!["a(so)".to_string()],
                ),
                (
                    "GetIndexInParent".to_string(),
                    "".to_string(),
                    vec!["i".to_string()],
                ),
                (
                    "GetRelationSet".to_string(),
                    "".to_string(),
                    vec!["a(ua(so))".to_string()],
                ),
                ("GetRole".to_string(), "".to_string(), vec!["u".to_string()]),
                (
                    "GetRoleName".to_string(),
                    "".to_string(),
                    vec!["s".to_string()],
                ),
                (
                    "GetLocalizedRoleName".to_string(),
                    "".to_string(),
                    vec!["s".to_string()],
                ),
                (
                    "GetState".to_string(),
                    "".to_string(),
                    vec!["au".to_string()],
                ),
                (
                    "GetAttributes".to_string(),
                    "".to_string(),
                    vec!["a{ss}".to_string()],
                ),
                (
                    "GetApplication".to_string(),
                    "".to_string(),
                    vec!["(so)".to_string()],
                ),
                (
                    "GetInterfaces".to_string(),
                    "".to_string(),
                    vec!["as".to_string()],
                ),
            ],
            "Application" => vec![(
                "GetLocale".to_string(),
                "u".to_string(),
                vec!["s".to_string()],
            )],
            "Action" => vec![
                (
                    "GetName".to_string(),
                    "i".to_string(),
                    vec!["s".to_string()],
                ),
                (
                    "GetLocalizedName".to_string(),
                    "i".to_string(),
                    vec!["s".to_string()],
                ),
                (
                    "GetDescription".to_string(),
                    "i".to_string(),
                    vec!["s".to_string()],
                ),
                (
                    "GetKeyBinding".to_string(),
                    "i".to_string(),
                    vec!["s".to_string()],
                ),
                (
                    "GetActions".to_string(),
                    "".to_string(),
                    vec!["a(sss)".to_string()],
                ),
                (
                    "GetNActions".to_string(),
                    "".to_string(),
                    vec!["i".to_string()],
                ),
                (
                    "DoAction".to_string(),
                    "i".to_string(),
                    vec!["b".to_string()],
                ),
            ],
            "Component" => vec![
                (
                    "Contains".to_string(),
                    "iiu".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "GetAccessibleAtPoint".to_string(),
                    "iiu".to_string(),
                    vec!["(so)".to_string()],
                ),
                (
                    "GetExtents".to_string(),
                    "u".to_string(),
                    vec!["(iiii)".to_string()],
                ),
                (
                    "GetPosition".to_string(),
                    "u".to_string(),
                    vec!["i".to_string(), "i".to_string()],
                ),
                (
                    "GetSize".to_string(),
                    "".to_string(),
                    vec!["i".to_string(), "i".to_string()],
                ),
                (
                    "GetLayer".to_string(),
                    "".to_string(),
                    vec!["u".to_string()],
                ),
                (
                    "GetMDIZOrder".to_string(),
                    "".to_string(),
                    vec!["n".to_string()],
                ),
                (
                    "GetAlpha".to_string(),
                    "".to_string(),
                    vec!["d".to_string()],
                ),
                (
                    "GrabFocus".to_string(),
                    "".to_string(),
                    vec!["b".to_string()],
                ),
            ],
            "Text" => vec![
                (
                    "GetText".to_string(),
                    "ii".to_string(),
                    vec!["s".to_string()],
                ),
                (
                    "GetCharacterAtOffset".to_string(),
                    "i".to_string(),
                    vec!["i".to_string()],
                ),
                (
                    "GetStringAtOffset".to_string(),
                    "iu".to_string(),
                    vec!["s".to_string(), "i".to_string(), "i".to_string()],
                ),
                (
                    "GetNSelections".to_string(),
                    "".to_string(),
                    vec!["i".to_string()],
                ),
                (
                    "GetSelection".to_string(),
                    "i".to_string(),
                    vec!["i".to_string(), "i".to_string()],
                ),
                (
                    "SetCaretOffset".to_string(),
                    "i".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "AddSelection".to_string(),
                    "ii".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "RemoveSelection".to_string(),
                    "i".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "SetSelection".to_string(),
                    "iii".to_string(),
                    vec!["b".to_string()],
                ),
            ],
            "EditableText" => vec![
                (
                    "SetTextContents".to_string(),
                    "s".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "InsertText".to_string(),
                    "isi".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "DeleteText".to_string(),
                    "ii".to_string(),
                    vec!["b".to_string()],
                ),
            ],
            "Table" => vec![
                (
                    "GetAccessibleAt".to_string(),
                    "ii".to_string(),
                    vec!["(so)".to_string()],
                ),
                (
                    "GetIndexAt".to_string(),
                    "ii".to_string(),
                    vec!["i".to_string()],
                ),
                (
                    "GetRowAtIndex".to_string(),
                    "i".to_string(),
                    vec!["i".to_string()],
                ),
                (
                    "GetColumnAtIndex".to_string(),
                    "i".to_string(),
                    vec!["i".to_string()],
                ),
                (
                    "GetRowDescription".to_string(),
                    "i".to_string(),
                    vec!["s".to_string()],
                ),
                (
                    "GetColumnDescription".to_string(),
                    "i".to_string(),
                    vec!["s".to_string()],
                ),
                (
                    "GetRowExtentAt".to_string(),
                    "ii".to_string(),
                    vec!["i".to_string()],
                ),
                (
                    "GetColumnExtentAt".to_string(),
                    "ii".to_string(),
                    vec!["i".to_string()],
                ),
                (
                    "GetRowHeader".to_string(),
                    "i".to_string(),
                    vec!["(so)".to_string()],
                ),
                (
                    "GetColumnHeader".to_string(),
                    "i".to_string(),
                    vec!["(so)".to_string()],
                ),
                (
                    "GetSelectedRows".to_string(),
                    "".to_string(),
                    vec!["ai".to_string()],
                ),
                (
                    "GetSelectedColumns".to_string(),
                    "".to_string(),
                    vec!["ai".to_string()],
                ),
                (
                    "IsRowSelected".to_string(),
                    "i".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "IsColumnSelected".to_string(),
                    "i".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "IsSelected".to_string(),
                    "ii".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "AddRowSelection".to_string(),
                    "i".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "AddColumnSelection".to_string(),
                    "i".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "RemoveRowSelection".to_string(),
                    "i".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "RemoveColumnSelection".to_string(),
                    "i".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "GetRowColumnExtentsAtIndex".to_string(),
                    "i".to_string(),
                    vec![
                        "b".to_string(),
                        "i".to_string(),
                        "i".to_string(),
                        "i".to_string(),
                        "i".to_string(),
                        "b".to_string(),
                    ],
                ),
            ],
            "Selection" => vec![
                (
                    "GetSelectedChild".to_string(),
                    "i".to_string(),
                    vec!["(so)".to_string()],
                ),
                (
                    "SelectChild".to_string(),
                    "i".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "DeselectSelectedChild".to_string(),
                    "i".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "IsChildSelected".to_string(),
                    "i".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "SelectAll".to_string(),
                    "".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "ClearSelection".to_string(),
                    "".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "DeselectChild".to_string(),
                    "i".to_string(),
                    vec!["b".to_string()],
                ),
                (
                    "GetNSelectedChildren".to_string(),
                    "".to_string(),
                    vec!["i".to_string()],
                ),
            ],
            "Cache" => vec![(
                "GetItems".to_string(),
                "".to_string(),
                vec!["a((so)(so)(so)iiassusau)".to_string()],
            )],
            "org.freedesktop.DBus.Peer" => vec![("Ping".to_string(), "".to_string(), vec![])],
            "org.freedesktop.DBus.Introspectable" => vec![(
                "Introspect".to_string(),
                "".to_string(),
                vec!["s".to_string()],
            )],
            "org.freedesktop.DBus.Properties" => vec![
                ("Get".to_string(), "ss".to_string(), vec!["v".to_string()]),
                (
                    "GetAll".to_string(),
                    "s".to_string(),
                    vec!["a{sv}".to_string()],
                ),
                ("Set".to_string(), "ssv".to_string(), vec![]),
            ],
            _ => vec![],
        }
    }
    fn properties(name: &str) -> Vec<(String, String, String)> {
        match name {
            "Accessible" => vec![
                ("Name".to_string(), "s".to_string(), "read".to_string()),
                (
                    "Description".to_string(),
                    "s".to_string(),
                    "read".to_string(),
                ),
                ("Parent".to_string(), "(so)".to_string(), "read".to_string()),
                (
                    "ChildCount".to_string(),
                    "i".to_string(),
                    "read".to_string(),
                ),
                ("Locale".to_string(), "s".to_string(), "read".to_string()),
                (
                    "AccessibleId".to_string(),
                    "s".to_string(),
                    "read".to_string(),
                ),
                ("HelpText".to_string(), "s".to_string(), "read".to_string()),
            ],
            "Application" => vec![
                (
                    "ToolkitName".to_string(),
                    "s".to_string(),
                    "read".to_string(),
                ),
                ("Version".to_string(), "s".to_string(), "read".to_string()),
                (
                    "ToolkitVersion".to_string(),
                    "s".to_string(),
                    "read".to_string(),
                ),
                (
                    "AtspiVersion".to_string(),
                    "s".to_string(),
                    "read".to_string(),
                ),
                ("Id".to_string(), "i".to_string(), "readwrite".to_string()),
            ],
            "Action" => vec![("NActions".to_string(), "i".to_string(), "read".to_string())],
            "Text" => vec![
                (
                    "CharacterCount".to_string(),
                    "i".to_string(),
                    "read".to_string(),
                ),
                (
                    "CaretOffset".to_string(),
                    "i".to_string(),
                    "read".to_string(),
                ),
            ],
            "Selection" => vec![(
                "NSelectedChildren".to_string(),
                "i".to_string(),
                "read".to_string(),
            )],
            "Table" => vec![
                ("NRows".to_string(), "i".to_string(), "read".to_string()),
                ("NColumns".to_string(), "i".to_string(), "read".to_string()),
                (
                    "Caption".to_string(),
                    "(so)".to_string(),
                    "read".to_string(),
                ),
                (
                    "Summary".to_string(),
                    "(so)".to_string(),
                    "read".to_string(),
                ),
                (
                    "NSelectedRows".to_string(),
                    "i".to_string(),
                    "read".to_string(),
                ),
                (
                    "NSelectedColumns".to_string(),
                    "i".to_string(),
                    "read".to_string(),
                ),
            ],
            "Value" => vec![
                (
                    "MinimumValue".to_string(),
                    "d".to_string(),
                    "read".to_string(),
                ),
                (
                    "MaximumValue".to_string(),
                    "d".to_string(),
                    "read".to_string(),
                ),
                (
                    "MinimumIncrement".to_string(),
                    "d".to_string(),
                    "read".to_string(),
                ),
                (
                    "CurrentValue".to_string(),
                    "d".to_string(),
                    "readwrite".to_string(),
                ),
                ("Text".to_string(), "s".to_string(), "read".to_string()),
            ],
            _ => vec![],
        }
    }
    pub fn describe(supported: &[String], children: &[String], extents: bool) -> String {
        let prefix = "org.a11y.atspi.";
        let mut names = vec![
            "org.freedesktop.DBus.Peer",
            "org.freedesktop.DBus.Introspectable",
        ];
        if !supported.is_empty() && !supported.contains(&(prefix.to_owned() + "Cache")) {
            names.push("org.freedesktop.DBus.Properties")
        };
        for name in supported {
            names.push(name)
        }
        let mut xml = String::from("<node>");
        for full in names {
            let name = if full.starts_with(prefix) {
                full.substring(prefix.byte_len(), full.byte_len())
            } else {
                full
            };
            xml += &format!("<interface name=\"{}\">", full);
            for (member, input, output) in methods(name, extents) {
                xml += &method(&member, &input, &output)
            }
            for (member, kind, access) in properties(name) {
                xml += &format!(
                    "<property name=\"{}\" type=\"{}\" access=\"{}\"/>",
                    member, kind, access
                )
            }
            xml += "</interface>";
        }
        for child in children {
            xml += &format!("<node name=\"{}\"/>", child)
        }
        xml.to_owned() + "</node>"
    }
}
#[cfg(target_os = "linux")]
/// The introspection XML of an object with the `supported` interfaces and
/// `children`; `extents` adds the Text methods that need laid-out glyphs.
pub fn describe(supported: &[String], children: &[String], extents: bool) -> String {
    linux::describe(supported, children, extents)
}
