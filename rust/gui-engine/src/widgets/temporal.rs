// Gregorian dates and time editors. Civil dates are independent of a timezone;
// conversion to an instant uses std::time's explicit DST gap/fold result.
use crate::accessibility::{AccessNode, Role};
use crate::geometry::{Constraints, Rect, Size};
use crate::strext::StrExt;
use crate::style::TextRole;
use crate::text::layout::{self, LayoutOptions};
use crate::widgets::cx::{Event, EventCx, LayoutCx, Node, PaintCx, UiMsg};
use crate::widgets::state::StateStore;
use windowing::{Key, MouseButton};

#[derive(Debug, Clone, PartialEq, Eq, Copy, serde::Serialize, serde::Deserialize)]
pub struct Date {
    pub year: i64,
    pub month: i64,
    pub day: i64,
}

pub fn leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}
pub fn month_days(year: i64, month: i64) -> i64 {
    match month {
        2 => {
            if leap(year) {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    }
}
fn digits(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c: char| c.is_ascii_digit())
}
fn pad(value: i64, width: i64) -> String {
    let mut text = format!("{}", value);
    while text.byte_len() < width {
        text = "0".to_owned() + &text
    }
    text
}
impl Date {
    pub fn new(year: i64, month: i64, day: i64) -> Option<Date> {
        if !(1..=9999).contains(&year) || day < 1 || day > month_days(year, month) {
            return None;
        };
        Some(Date { year, month, day })
    }
    pub fn parse(text: &str) -> Option<Date> {
        if text.byte_len() != 10 || text.substring(4, 5) != "-" || text.substring(7, 8) != "-" {
            return None;
        };
        if !digits(text.substring(0, 4))
            || !digits(text.substring(5, 7))
            || !digits(text.substring(8, 10))
        {
            return None;
        };
        Date::new(
            text.substring(0, 4).to_i64()?,
            text.substring(5, 7).to_i64()?,
            text.substring(8, 10).to_i64()?,
        )
    }
    pub fn iso(&self) -> String {
        format!(
            "{}-{}-{}",
            pad(self.year, 4),
            pad(self.month, 2),
            pad(self.day, 2)
        )
    }
    /// Day zero is Monday, 0001-01-01. Valid dates are in 0..3652059.
    pub fn ordinal(&self) -> i64 {
        let y = self.year - 1;
        let mut days = y * 365 + y / 4 - y / 100 + y / 400 + self.day - 1;
        for m in 1..self.month {
            days += month_days(self.year, m)
        }
        days
    }
    pub fn weekday(&self) -> i64 {
        self.ordinal() % 7
    }
    pub fn add_days(&self, days: i64) -> Date {
        let target = (self.ordinal() + days.clamp(-3_652_059, 3_652_059)).clamp(0, 3_652_058);
        let mut lo = 1;
        let mut hi = 10_000;
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if (Date {
                year: mid,
                month: 1,
                day: 1,
            })
            .ordinal()
                <= target
            {
                lo = mid
            } else {
                hi = mid
            }
        }
        let mut left = target
            - (Date {
                year: lo,
                month: 1,
                day: 1,
            })
            .ordinal();
        let mut month = 1;
        while left >= month_days(lo, month) {
            left -= month_days(lo, month);
            month += 1
        }
        Date {
            year: lo,
            month,
            day: left + 1,
        }
    }
    pub fn add_months(&self, months: i64) -> Date {
        let index = ((self.year - 1) * 12 + self.month - 1 + months.clamp(-120_000, 120_000))
            .clamp(0, 119_987);
        let year = index / 12 + 1;
        let month = index % 12 + 1;
        Date {
            year,
            month,
            day: self.day.min(month_days(year, month)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, serde::Serialize, serde::Deserialize)]
pub struct ClockTime {
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
}
impl ClockTime {
    pub fn parse(text: &str) -> Option<ClockTime> {
        let parts = text.split(":").collect::<Vec<_>>();
        if parts.len() < 2
            || parts.len() > 3
            || parts[0].byte_len() != 2
            || parts[1].byte_len() != 2
        {
            return None;
        };
        if parts.len() == 3 && parts[2].byte_len() != 2 {
            return None;
        };
        for part in &parts {
            if !digits(part) {
                return None;
            }
        }
        let h = parts[0].to_i64()?;
        let m = parts[1].to_i64()?;
        let s = if parts.len() == 3 {
            parts[2].to_i64()?
        } else {
            0
        };
        if !(0..=23).contains(&h) || !(0..=59).contains(&m) || !(0..=59).contains(&s) {
            return None;
        };
        Some(ClockTime {
            hour: h,
            minute: m,
            second: s,
        })
    }
    pub fn iso(&self) -> String {
        format!(
            "{}:{}:{}",
            pad(self.hour, 2),
            pad(self.minute, 2),
            pad(self.second, 2)
        )
    }
    pub fn seconds(&self) -> i64 {
        self.hour * 3600 + self.minute * 60 + self.second
    }
}

/// A nonnegative elapsed duration, independent of daylight-saving transitions.
pub fn parse_duration(text: &str) -> Option<i64> {
    let parts = text.split(":").collect::<Vec<_>>();
    if parts.len() != 3 {
        return None;
    };
    let h = parts[0].to_i64()?;
    let m = parts[1].to_i64()?;
    let s = parts[2].to_i64()?;
    if h < 0 || !(0..=59).contains(&m) || !(0..=59).contains(&s) {
        return None;
    };
    // Leave room for minutes and seconds before multiplying.
    if h > (i64::MAX - m * 60 - s) / 3600 {
        return None;
    };
    Some(h * 3600 + m * 60 + s)
}

/// `HH:MM:SS` text for a duration in seconds, with negative durations as zero.
pub fn format_duration(seconds: i64) -> String {
    let s = seconds.max(0);
    format!(
        "{}:{}:{}",
        pad(s / 3600, 2),
        pad((s / 60) % 60, 2),
        pad(s % 60, 2)
    )
}

/// Where a wall-clock time lands in a time zone, in Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LocalResolution {
    /// Exactly one instant shows this time.
    Unique(i64),
    /// A forward clock change skips this time.
    Gap,
    /// A backward clock change shows this time twice, earlier first.
    Fold(i64, i64),
}

/// Resolves in the named IANA timezone; never silently chooses a DST fold.
pub fn resolve_local(date: Date, clock: ClockTime, zone: &str) -> Result<LocalResolution, String> {
    use jiff::tz::AmbiguousOffset;
    let tz = jiff::tz::TimeZone::get(zone).map_err(|e| e.to_string())?;
    let narrow = |n: i64| i8::try_from(n).map_err(|_| format!("{n} is out of range"));
    let year =
        i16::try_from(date.year).map_err(|_| format!("year {} is out of range", date.year))?;
    let dt = jiff::civil::DateTime::new(
        year,
        narrow(date.month)?,
        narrow(date.day)?,
        narrow(clock.hour)?,
        narrow(clock.minute)?,
        narrow(clock.second)?,
        0,
    )
    .map_err(|e| e.to_string())?;
    let at = |offset: jiff::tz::Offset| {
        offset
            .to_timestamp(dt)
            .map(|t| t.as_millisecond())
            .map_err(|e| e.to_string())
    };
    Ok(match tz.to_ambiguous_timestamp(dt).offset() {
        AmbiguousOffset::Unambiguous { offset } => LocalResolution::Unique(at(offset)?),
        AmbiguousOffset::Gap { .. } => LocalResolution::Gap,
        AmbiguousOffset::Fold { before, after } => LocalResolution::Fold(at(before)?, at(after)?),
    })
}

/// A single-selection calendar. Arrow keys move one day/week, Page Up/Down
/// one month (Control: one year), Home/End to week edges. Enter/Space selects.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Calendar {
    pub id: String,
    pub value: Date,
    pub min: Date,
    pub max: Date,
    pub first_weekday: i64,
    pub label: String,
}
impl Calendar {
    pub fn new(id: &str, value: Date) -> Calendar {
        Calendar {
            id: id.to_string(),
            value,
            min: Date {
                year: 1,
                month: 1,
                day: 1,
            },
            max: Date {
                year: 9999,
                month: 12,
                day: 31,
            },
            first_weekday: 0,
            label: crate::localization::tr("Date"),
        }
    }
    pub fn clamp(&self, date: Date) -> Date {
        if date.ordinal() < self.min.ordinal() {
            self.min
        } else if date.ordinal() > self.max.ordinal() {
            self.max
        } else {
            date
        }
    }
}
fn cursor(c: &Calendar, st: &StateStore) -> Date {
    if st.field(&c.id, "external", "") != c.value.iso() {
        return c.clamp(c.value);
    };
    Date::parse(&st.field(&c.id, "cursor", ""))
        .map(|d| c.clamp(d))
        .unwrap_or(c.clamp(c.value))
}
fn first(c: &Calendar, date: Date) -> i64 {
    let d = Date {
        year: date.year,
        month: date.month,
        day: 1,
    };
    d.ordinal() - (d.weekday() - c.first_weekday.clamp(0, 6) + 7) % 7
}
fn cell_date(start: i64, index: i64) -> Option<Date> {
    let ordinal = start + index;
    if !(0..=3_652_058).contains(&ordinal) {
        None
    } else {
        Some(
            (Date {
                year: 1,
                month: 1,
                day: 1,
            })
            .add_days(ordinal),
        )
    }
}
fn weekday_names() -> Vec<String> {
    ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"]
        .iter()
        .map(|d| crate::localization::tr(d))
        .collect::<Vec<_>>()
}
pub fn layout_calendar(limits: Constraints, cx: &mut LayoutCx) -> Node {
    let theme = &cx.theme;
    let style = theme.font(TextRole::Body);
    let widest = std::iter::once("88".to_string())
        .chain(weekday_names())
        .map(|t| layout::measure(&t, &style))
        .fold(0.0_f64, |w, (tw, _)| w.max(tw));
    let (_, _, lh) = layout::metrics(&style);
    let natural_cell = theme.px(40).max(widest.ceil() as i64 + theme.padding() * 2);
    let natural_row = theme.px(36).max(lh.ceil() as i64 + theme.padding());
    let size = limits.constrain(Size::new(natural_cell * 7, natural_row * 8));
    let mut n = Node::leaf(size);
    let cell = size.width / 7;
    let row = size.height / 8;
    n.parts.push(Rect::new(0, 0, cell, row));
    n.parts.push(Rect::new(size.width - cell, 0, cell, row));
    for i in 0..42 {
        let col = if theme.rtl { 6 - i % 7 } else { i % 7 };
        n.parts
            .push(Rect::new(col * cell, (i / 7 + 2) * row, cell, row))
    }
    n
}
fn text(cx: &mut PaintCx, rect: Rect, value: &str, color: crate::style::color::Color) {
    let style = cx.theme.font(TextRole::Body);
    let line = layout::layout(value, &style, LayoutOptions::single_line());
    let at = cx.abs(rect);
    let x = at.x as f64 + (at.width as f64 - line.width) / 2.0;
    let y = at.y as f64 + (at.height as f64 - line.height) / 2.0;
    layout::draw(&mut cx.canvas, &line, x, y, &style, color)
}
pub fn paint_calendar(c: &Calendar, n: &Node, cx: &mut PaintCx) {
    if n.parts.len() != 44 {
        return;
    };
    let date = cursor(c, &cx.states);
    let start = first(c, date);
    let theme = cx.theme.clone();
    text(cx, n.parts[0], "<", theme.text);
    text(cx, n.parts[1], ">", theme.text);
    text(
        cx,
        Rect::new(
            n.size().width / 7,
            0,
            n.size().width * 5 / 7,
            n.size().height / 8,
        ),
        &crate::localization::message(
            "calendar",
            "{month} {year}",
            1,
            &crate::hmap! { "month" => crate::localization::month_name(date.month), "year" => format!("{}", date.year) },
        ),
        theme.text,
    );
    let names = weekday_names();
    for i in 0..7 {
        let col = if theme.rtl { 6 - i } else { i };
        text(
            cx,
            Rect::new(
                col * n.size().width / 7,
                n.size().height / 8,
                n.size().width / 7,
                n.size().height / 8,
            ),
            &names[((i + c.first_weekday.clamp(0, 6)) % 7) as usize],
            theme.text_muted,
        )
    }
    for i in 0..42 {
        let Some(d) = cell_date(start, i) else {
            continue;
        };
        let r = n.parts[(i + 2) as usize];
        let enabled =
            d.ordinal() >= c.min.ordinal() && d.ordinal() <= c.max.ordinal() && !cx.disabled;
        if d == c.value {
            cx.fill_round(r, theme.radius(), theme.selection)
        };
        if d == date && cx.is_focused(&c.id) {
            cx.focus_ring(r, theme.radius())
        };
        text(
            cx,
            r,
            &format!("{}", d.day),
            if !enabled {
                theme.text_disabled
            } else if d.month != date.month {
                theme.text_muted
            } else {
                theme.text
            },
        )
    }
}
pub fn calendar_event(c: &Calendar, n: &Node, ev: &Event, st: &mut StateStore, cx: &mut EventCx) {
    if cx.disabled {
        return;
    };
    let old = cursor(c, st);
    let mut date = old;
    let mut select = false;
    match ev {
        Event::KeyDown { key, mods, .. } => {
            date = match key {
                Key::Left => date.add_days(if cx.theme.rtl { 1 } else { -1 }),
                Key::Right => date.add_days(if cx.theme.rtl { -1 } else { 1 }),
                Key::Up => date.add_days(-7),
                Key::Down => date.add_days(7),
                Key::Home => date.add_days(-((date.weekday() - c.first_weekday + 7) % 7)),
                Key::End => date.add_days(6 - (date.weekday() - c.first_weekday + 7) % 7),
                Key::PageUp => date.add_months(if mods.ctrl { -12 } else { -1 }),
                Key::PageDown => date.add_months(if mods.ctrl { 12 } else { 1 }),
                Key::Return | Key::Space => {
                    select = true;
                    date
                }
                _ => return,
            };
        }
        Event::PointerDown { x, y, button, .. } => {
            if *button != MouseButton::Left || n.parts.len() != 44 {
                return;
            };
            cx.focus_on(&c.id);
            let lx = x - cx.origin_x;
            let ly = y - cx.origin_y;
            if n.parts[0].contains(lx, ly) {
                date = date.add_months(-1)
            } else if n.parts[1].contains(lx, ly) {
                date = date.add_months(1)
            } else {
                let start = first(c, old);
                for i in 0..42 {
                    if n.parts[i + 2].contains(lx, ly) {
                        let Some(d) = cell_date(start, i as i64) else {
                            continue;
                        };
                        if d.ordinal() < c.min.ordinal() || d.ordinal() > c.max.ordinal() {
                            return;
                        };
                        date = d;
                        select = true;
                    }
                }
            }
        }
        _ => return,
    }
    date = c.clamp(date);
    st.set_field(&c.id, "external", &c.value.iso());
    st.set_field(&c.id, "cursor", &date.iso());
    if select && date != c.value {
        cx.emit(&UiMsg::Date(c.id.clone(), date.iso()))
    };
    cx.redraw = true;
    cx.consume()
}
pub fn calendar_semantics(c: &Calendar, n: &Node, st: &StateStore) -> Option<AccessNode> {
    let mut root = AccessNode::control(Role::Calendar, &c.id, &c.label, "");
    root.value = c.value.iso();
    let date = cursor(c, st);
    let start = first(c, date);
    for i in 0..42 {
        let Some(d) = cell_date(start, i) else {
            continue;
        };
        let mut day = AccessNode::control(
            Role::Cell,
            &format!("{}#{}", c.id, d.iso()),
            &d.iso(),
            "press",
        );
        day.selected = d == c.value;
        day.disabled = d.ordinal() < c.min.ordinal() || d.ordinal() > c.max.ordinal();
        day.focusable = false;
        if n.parts.len() == 44 {
            day.bounds = n.parts[(i + 2) as usize]
        };
        root.children.push(day)
    }
    Some(root)
}

#[cfg(test)]
mod tests {
    use super::LocalResolution;
    use super::{Calendar, ClockTime, Date};
    use crate::geometry::{Constraints, Rect, Size};

    use crate::style::Theme;
    use crate::widgets::cx::{Event, EventCx, LayoutCx, UiMsg};
    use crate::widgets::state::StateStore;

    #[test]
    fn calendar_cells_fit_two_digit_days_and_weekday_names_at_large_text() {
        for text_scale in [1.0, 2.0] {
            let theme = Theme {
                text_scale,
                ..Theme::default()
            };
            let mut l = LayoutCx::new(&theme, &StateStore::new(), Size::new(2000, 2000));
            let node = super::layout_calendar(Constraints::loose(l.window), &mut l);
            let style = theme.font(crate::style::TextRole::Body);
            let (_, _, line) = crate::text::layout::metrics(&style);
            for text in ["28", "We"] {
                let (w, _) = crate::text::layout::measure(text, &style);
                assert!(
                    node.parts[2].width as f64 >= w && node.parts[2].height as f64 >= line,
                    "{} at text size {}",
                    text,
                    text_scale
                )
            }
        }
    }
    #[test]
    fn calendar_leap_month_edges_and_epoch_round_trip() {
        assert_eq!(Date::new(1900, 2, 29), None);
        assert_eq!(
            Date::new(2000, 2, 29).unwrap().add_months(12).iso(),
            "2001-02-28"
        );
        assert_eq!(Date::parse("2026-02-30"), None);
        assert_eq!(
            Date::new(2026, 3, 1).unwrap().add_days(-1).iso(),
            "2026-02-28"
        );
        assert_eq!(Date::new(1, 1, 1).unwrap().add_days(-1).iso(), "0001-01-01");
        assert_eq!(
            Date::new(9999, 12, 31).unwrap().add_days(1).iso(),
            "9999-12-31"
        );
        assert_eq!(ClockTime::parse("24:00"), None);
        assert_eq!(ClockTime::parse("23:59:59").unwrap().seconds(), 86_399);
        assert_eq!(super::parse_duration("27:04:05"), Some(97_445));
        assert_eq!(super::parse_duration("9999999999999999:00:00"), None)
    }
    #[test]
    fn calendar_keyboard_bounds_and_no_duplicate_selection() {
        let mut c = Calendar::new("calendar", Date::new(2026, 3, 1).unwrap());
        c.min = c.value;
        c.max = c.value.add_days(2);
        let theme = Theme::default();
        let mut st = StateStore::new();
        let mut l = LayoutCx::new(&theme, &StateStore::new(), Size::new(280, 288));
        let node = super::layout_calendar(Constraints::loose(l.window), &mut l);
        let mut cx = EventCx::new(&theme, &c.id, 0, Rect::new(0, 0, 280, 288));
        super::calendar_event(
            &c,
            &node,
            &Event::KeyDown {
                key: windowing::Key::Return,
                mods: windowing::Mods::default(),
                repeat: false,
            },
            &mut st,
            &mut cx,
        );
        assert!(cx.messages.is_empty());
        super::calendar_event(
            &c,
            &node,
            &Event::KeyDown {
                key: windowing::Key::PageDown,
                mods: windowing::Mods::default(),
                repeat: false,
            },
            &mut st,
            &mut cx,
        );
        super::calendar_event(
            &c,
            &node,
            &Event::KeyDown {
                key: windowing::Key::Return,
                mods: windowing::Mods::default(),
                repeat: false,
            },
            &mut st,
            &mut cx,
        );
        assert_eq!(
            cx.messages,
            vec![UiMsg::Date(
                "calendar".to_string(),
                "2026-03-03".to_string()
            )]
        )
    }
    #[test]
    fn timezone_picker_preserves_gap_and_fold() {
        let clock = ClockTime::parse("02:30").unwrap();
        assert!(matches!(
            super::resolve_local(Date::new(2026, 3, 8).unwrap(), clock, "America/New_York")
                .unwrap(),
            LocalResolution::Gap
        ));
        assert!(matches!(
            super::resolve_local(
                Date::new(2026, 11, 1).unwrap(),
                ClockTime::parse("01:30").unwrap(),
                "America/New_York"
            )
            .unwrap(),
            LocalResolution::Fold(_, _)
        ))
    }
}

/// Text editors share grapheme editing, IME, clipboard, undo and accessible text
/// with TextInput. A valid change emits Date; incomplete drafts stay local.
#[derive(Debug, Clone, PartialEq, Eq, Copy, serde::Serialize, serde::Deserialize)]
pub enum TemporalKind {
    DateOnly,
    TimeOnly,
    DateAndTime,
    Elapsed,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TemporalEditor {
    pub id: String,
    pub value: String,
    pub kind: TemporalKind,
    pub min: String,
    pub max: String,
    pub label: String,
}
impl TemporalEditor {
    pub fn new(id: &str, value: &str, kind: TemporalKind) -> TemporalEditor {
        TemporalEditor {
            id: id.to_string(),
            value: value.to_string(),
            kind,
            min: "".to_string(),
            max: "".to_string(),
            label: crate::localization::tr("Date or time"),
        }
    }
    pub fn canonical(&self, text: &str) -> Option<String> {
        let result = canonical(self.kind, text)?;
        let min = if self.min.is_empty() {
            None
        } else {
            Some(canonical(self.kind, &self.min)?)
        };
        let max = if self.max.is_empty() {
            None
        } else {
            Some(canonical(self.kind, &self.max)?)
        };
        if self.kind == TemporalKind::Elapsed {
            let value = parse_duration(&result)?;
            if let Some(lo) = min
                && value < parse_duration(&lo)?
            {
                return None;
            };
            if let Some(hi) = max
                && value > parse_duration(&hi)?
            {
                return None;
            }
        } else {
            if let Some(lo) = min
                && result < lo
            {
                return None;
            };
            if let Some(hi) = max
                && result > hi
            {
                return None;
            }
        }
        Some(result)
    }
}
pub fn canonical(kind: TemporalKind, text: &str) -> Option<String> {
    match kind {
        TemporalKind::DateOnly => Some(Date::parse(text)?.iso()),
        TemporalKind::TimeOnly => Some(ClockTime::parse(text)?.iso()),
        TemporalKind::Elapsed => Some(format_duration(parse_duration(text)?)),
        TemporalKind::DateAndTime => {
            let p = text.split("T").collect::<Vec<_>>();
            if p.len() != 2 {
                return None;
            };
            Some(format!(
                "{}T{}",
                Date::parse(p[0])?.iso(),
                ClockTime::parse(p[1])?.iso()
            ))
        }
    }
}
fn input(t: &TemporalEditor) -> crate::widgets::textinput::TextInput {
    let hint = match t.kind {
        TemporalKind::DateOnly => "YYYY-MM-DD",
        TemporalKind::TimeOnly => "HH:MM:SS",
        TemporalKind::DateAndTime => "YYYY-MM-DDTHH:MM:SS",
        TemporalKind::Elapsed => "hours:minutes:seconds",
    };
    let mut field = crate::widgets::textinput::TextInput::new(
        &t.id,
        &t.value,
        hint,
        crate::widgets::textinput::InputKind::Line,
    );
    field.label = t.label.clone();
    field.max_length = 32;
    let t = t.clone();
    field.validate = Some(crate::Callback::new(std::rc::Rc::new(move |text| {
        if t.canonical(&text).is_some() {
            return crate::editing::Validity::Valid;
        };
        let complete = match t.kind {
            TemporalKind::DateOnly => text.byte_len() >= 10,
            TemporalKind::TimeOnly => text.byte_len() >= 5,
            TemporalKind::DateAndTime => text.byte_len() >= 16,
            TemporalKind::Elapsed => text.split(":").count() >= 3,
        };
        let why = format!("Enter {} within the allowed range", hint);
        if complete {
            crate::editing::Validity::Invalid(why)
        } else {
            crate::editing::Validity::Intermediate(why)
        }
    })));
    field
}
pub fn layout_editor(t: &TemporalEditor, c: Constraints, cx: &mut LayoutCx) -> Node {
    crate::widgets::textinput::layout_input(&input(t), c, cx)
}
pub fn paint_editor(t: &TemporalEditor, n: &Node, cx: &mut PaintCx) {
    crate::widgets::textinput::paint_input(&input(t), n, cx)
}
pub fn editor_event(
    t: &TemporalEditor,
    n: &Node,
    ev: &Event,
    st: &mut StateStore,
    cx: &mut EventCx,
) {
    let before = cx.messages.len();
    crate::widgets::textinput::input_event(&input(t), n, ev, st, cx);
    let messages = &cx.messages;
    let mut kept = vec![];
    for (i, msg) in messages.iter().enumerate() {
        if i < before {
            kept.push(msg.clone())
        } else {
            match msg {
                UiMsg::Text(id, text) | UiMsg::Submit(id, text) if *id == t.id => {
                    if let Some(value) = t.canonical(text)
                        && value != t.value
                    {
                        kept.push(UiMsg::Date(t.id.clone(), value))
                    }
                }
                _ => kept.push(msg.clone()),
            }
        }
    }
    cx.messages = kept;
}
pub fn editor_semantics(t: &TemporalEditor, st: &StateStore) -> Option<AccessNode> {
    crate::widgets::textinput::input_semantics(&input(t), st)
}
