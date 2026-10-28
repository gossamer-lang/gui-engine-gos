/// Text operations on UTF-8 byte offsets, which the toolkit keeps as `i64`.
pub trait StrExt {
    /// The length in bytes.
    fn byte_len(&self) -> i64;
    /// Bytes `start..end`, clamped to the text and widened to whole
    /// characters: the start moves back and the end forward to a boundary.
    fn substring(&self, start: i64, end: i64) -> &str;
    /// The byte at `i`, or 0 outside the text.
    fn byte_at(&self, i: i64) -> i64;
    /// The whole text as a decimal integer.
    fn to_i64(&self) -> Option<i64>;
    /// The whole text as a decimal floating-point number.
    fn to_f64(&self) -> Option<f64>;
}

impl StrExt for str {
    fn byte_len(&self) -> i64 {
        self.len() as i64
    }

    fn substring(&self, start: i64, end: i64) -> &str {
        let clamp = |at: i64| (at.max(0) as usize).min(self.len());
        let mut lo = clamp(start);
        while !self.is_char_boundary(lo) {
            lo -= 1;
        }
        let mut hi = clamp(end).max(lo);
        while !self.is_char_boundary(hi) {
            hi += 1;
        }
        &self[lo..hi]
    }

    fn byte_at(&self, i: i64) -> i64 {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.as_bytes().get(i))
            .map_or(0, |&b| i64::from(b))
    }

    fn to_i64(&self) -> Option<i64> {
        self.parse().ok()
    }

    fn to_f64(&self) -> Option<f64> {
        self.parse().ok()
    }
}

#[cfg(test)]
mod tests {

    use crate::strext::StrExt;

    #[test]
    fn substring_clamps_and_widens_to_characters() {
        assert_eq!("héllo".substring(2, 3), "é");
        assert_eq!("abc".substring(-4, 99), "abc");
        assert_eq!("abc".substring(2, 1), "");
    }

    #[test]
    fn byte_at_answers_zero_outside_the_text() {
        assert_eq!("a".byte_at(0), 97);
        assert_eq!("a".byte_at(1), 0);
        assert_eq!("a".byte_at(-1), 0);
    }
}
