use std::collections::HashMap;
// Live-region pacing. A region that changes faster than speech can follow (a
// progress label, a clock, a streaming status) would queue one announcement
// per change; assistive technology then reads stale text long after it was
// replaced. Each region speaks at most once per gap for its politeness, and a
// change inside the gap waits and replaces any earlier waiting text, so the
// listener hears the latest text once the gap has passed.

/// Least time between two announcements of one polite region.
pub const POLITE_GAP_MS: i64 = 750;

/// Least time between two announcements of one assertive region.
pub const ASSERTIVE_GAP_MS: i64 = 150;

/// The politeness level AT-SPI's `Announcement` signal carries.
pub fn level(politeness: &str) -> i64 {
    if politeness == "assertive" { 2 } else { 1 }
}

fn gap(level: i64) -> i64 {
    if level >= 2 {
        ASSERTIVE_GAP_MS
    } else {
        POLITE_GAP_MS
    }
}

/// One announcement to make: the region's path, its text, and its level.
#[derive(Clone, Debug, PartialEq)]
pub struct Spoken {
    pub path: String,
    pub text: String,
    pub level: i64,
}

/// When each live region last spoke, and the text a region waits to speak.
#[derive(Debug, Clone, PartialEq)]
pub struct Pacer {
    last: HashMap<String, i64>,
    waiting: HashMap<String, Spoken>,
}

impl Default for Pacer {
    fn default() -> Self {
        Self::new()
    }
}

impl Pacer {
    pub fn new() -> Pacer {
        Pacer {
            last: HashMap::new(),
            waiting: HashMap::new(),
        }
    }

    /// Offers new `text` from the region at `path` at time `now` (milliseconds
    /// on a monotonic clock). Answers the announcement to make at once, or
    /// `None` when the region spoke within its gap and the text waits instead.
    pub fn offer(&mut self, path: &str, text: &str, level: i64, now: i64) -> Option<Spoken> {
        let spoken = Spoken {
            path: path.to_string(),
            text: text.to_string(),
            level,
        };
        let ready = match self.last.get(path) {
            Some(at) => now - at >= gap(level),
            None => true,
        };
        if ready {
            let _ = self.waiting.remove(path);
            self.last.insert(path.to_string(), now);
            return Some(spoken);
        }
        self.waiting.insert(path.to_string(), spoken);
        None
    }

    /// The waiting announcements whose gap has passed by `now`, in path order.
    pub fn due(&mut self, now: i64) -> Vec<Spoken> {
        let mut paths: Vec<String> = vec![];
        for (path, spoken) in &self.waiting {
            let at = self.last.get(path).unwrap_or(&0);
            if now - at >= gap(spoken.level) {
                paths.push(path.to_string())
            }
        }
        paths.sort();
        let mut out = vec![];
        for path in paths {
            if let Some(spoken) = self.waiting.remove(&path) {
                self.last.insert(path.to_string(), now);
                out.push(spoken)
            }
        }
        out
    }

    /// Drops what a removed region was waiting to say.
    pub fn forget(&mut self, path: &str) {
        let _ = self.waiting.remove(path);
        let _ = self.last.remove(path);
    }
}

#[cfg(test)]
mod tests {
    use super::{ASSERTIVE_GAP_MS, POLITE_GAP_MS, Pacer, Spoken};

    fn said(path: &str, text: &str, level: i64) -> Spoken {
        Spoken {
            path: path.to_string(),
            text: text.to_string(),
            level,
        }
    }

    #[test]
    fn a_region_speaks_its_first_change_at_once() {
        let mut p = Pacer::new();
        assert_eq!(p.offer("/a", "10%", 1, 0), Some(said("/a", "10%", 1)))
    }

    #[test]
    fn changes_inside_the_gap_wait_and_the_latest_one_is_spoken() {
        let mut p = Pacer::new();
        let _ = p.offer("/a", "10%", 1, 0);
        assert_eq!(p.offer("/a", "20%", 1, 100), None);
        assert_eq!(p.offer("/a", "30%", 1, 200), None);
        assert_eq!(p.due(POLITE_GAP_MS - 1), vec![]);
        assert_eq!(p.due(POLITE_GAP_MS), vec![said("/a", "30%", 1)]);
        assert_eq!(p.due(POLITE_GAP_MS * 3), vec![])
    }

    #[test]
    fn an_assertive_region_has_a_shorter_gap() {
        let mut p = Pacer::new();
        let _ = p.offer("/alert", "one", 2, 0);
        assert_eq!(
            p.offer("/alert", "two", 2, ASSERTIVE_GAP_MS),
            Some(said("/alert", "two", 2))
        )
    }

    #[test]
    fn regions_are_paced_apart_and_a_removed_region_says_nothing_more() {
        let mut p = Pacer::new();
        let _ = p.offer("/a", "a1", 1, 0);
        let _ = p.offer("/b", "b1", 1, 0);
        assert_eq!(p.offer("/a", "a2", 1, 10), None);
        assert_eq!(p.offer("/b", "b2", 1, 10), None);
        p.forget("/b");
        assert_eq!(p.due(POLITE_GAP_MS), vec![said("/a", "a2", 1)])
    }
}
