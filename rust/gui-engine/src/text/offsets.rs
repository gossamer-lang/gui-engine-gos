// offsets: one wrapper type per text position unit, and the checked
// conversions between them. Every range is half-open. A conversion that
// cannot land exactly on a position of the target unit answers an
// `OffsetError` rather than rounding, so one unit is never passed as another.
//
// Units each boundary uses:
// - `ByteOffset`: `String` slicing, `TextLayout`, `EditState`, syntax spans.
// - `ScalarIndex`: SDL text-editing cursor and length, AT-SPI text offsets,
//   `s.len()` and `s[i]`.
// - `Utf16Offset`: Windows UI Automation and TSF, macOS `NSString` ranges.
// - `GraphemeIndex`: user-visible character counts (max-length policies).
// - `LinePosition`: editor line and column, column in bytes into the line.
// - `VisualPosition`: caret slots of one laid-out line, left to right.

use crate::strext::StrExt;
use crate::text::layout::{TextLayout, grapheme_boundaries};

/// A UTF-8 byte position. The one-element tuple wire form matches Gossamer's
/// tuple structs; deserialization also accepts legacy scalar encodings.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Copy)]
pub struct ByteOffset(pub i64);
impl serde::Serialize for ByteOffset {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut record = serializer.serialize_struct("ByteOffset", 1)?;
        record.serialize_field("0", &self.0)?;
        record.end()
    }
}
impl<'de> serde::Deserialize<'de> for ByteOffset {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Record {
                #[serde(rename = "0")]
                value: i64,
            },
            Tuple([i64; 1]),
            Scalar(i64),
        }
        Ok(Self(
            match <Wire as serde::Deserialize>::deserialize(deserializer)? {
                Wire::Record { value } | Wire::Tuple([value]) | Wire::Scalar(value) => value,
            },
        ))
    }
}

/// A Unicode scalar value index: the number of scalars before the position.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Copy,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct ScalarIndex(pub i64);

/// A UTF-16 code unit position: the number of code units before it.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Copy,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct Utf16Offset(pub i64);

/// An extended grapheme cluster boundary, counted in clusters before it.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Copy,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct GraphemeIndex(pub i64);

/// A logical line (split at `\n` only) and a byte column into it.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Copy,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct LinePosition {
    pub line: i64,
    pub column: ByteOffset,
}

/// A caret slot of one laid-out line, numbered from the leftmost caret.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct VisualPosition {
    pub line: i64,
    pub slot: i64,
}

/// Why a position could not be checked or converted.
#[derive(Clone, Debug, PartialEq, Eq, Copy, serde::Serialize, serde::Deserialize)]
pub enum OffsetError {
    /// The position is negative or past `len`, in the unit it was given in.
    OutOfRange { offset: i64, len: i64 },
    /// The byte offset falls inside a UTF-8 sequence.
    InsideScalar { offset: i64 },
    /// The UTF-16 offset falls between the two halves of a surrogate pair.
    InsideSurrogatePair { offset: i64 },
    /// The byte offset is a scalar boundary inside a grapheme cluster.
    InsideGrapheme { offset: i64 },
    /// The line does not exist; `lines` is how many do.
    LineOutOfRange { line: i64, lines: i64 },
    /// A range whose start lies after its end.
    Reversed { start: i64, end: i64 },
    /// The byte offset is cut off by elision and has no caret slot.
    NotLaidOut { offset: i64 },
}

impl std::fmt::Display for OffsetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = {
            match self {
                OffsetError::OutOfRange { offset, len } => {
                    format!("position {} is outside 0..={}", offset, len)
                }
                OffsetError::InsideScalar { offset } => {
                    format!("byte {} is inside a UTF-8 sequence", offset)
                }
                OffsetError::InsideSurrogatePair { offset } => {
                    format!("UTF-16 offset {} splits a surrogate pair", offset)
                }
                OffsetError::InsideGrapheme { offset } => {
                    format!("byte {} is inside a grapheme cluster", offset)
                }
                OffsetError::LineOutOfRange { line, lines } => {
                    format!("line {} is outside 0..{}", line, lines)
                }
                OffsetError::Reversed { start, end } => {
                    format!("range {}..{} ends before it starts", start, end)
                }
                OffsetError::NotLaidOut { offset } => format!("byte {} is not laid out", offset),
            }
        };
        f.write_str(&text)
    }
}

/// `at` checked to lie on a scalar boundary of `text`.
pub fn check_byte(text: &str, at: ByteOffset) -> Result<ByteOffset, OffsetError> {
    let ByteOffset(b) = at;
    let len = text.byte_len();
    if b < 0 || b > len {
        return Err(OffsetError::OutOfRange { offset: b, len });
    }
    if b < len && (text.byte_at(b) & 0xC0) == 0x80 {
        return Err(OffsetError::InsideScalar { offset: b });
    }
    Ok(at)
}

/// `at` checked to lie on an extended grapheme cluster boundary of `text`.
pub fn check_grapheme(text: &str, at: ByteOffset) -> Result<ByteOffset, OffsetError> {
    let _ = grapheme_of_byte(text, at)?;
    Ok(at)
}

/// The byte offset of scalar index `at` in `text`.
pub fn byte_of_scalar(text: &str, at: ScalarIndex) -> Result<ByteOffset, OffsetError> {
    let ScalarIndex(want) = at;
    if want < 0 {
        return Err(OffsetError::OutOfRange {
            offset: want,
            len: text.len() as i64,
        });
    }
    let mut bytes = 0;
    let mut n = 0;
    for c in text.chars() {
        if n == want {
            return Ok(ByteOffset(bytes));
        }
        bytes += c.len_utf8() as i64;
        n += 1;
    }
    if n == want {
        Ok(ByteOffset(bytes))
    } else {
        Err(OffsetError::OutOfRange {
            offset: want,
            len: n,
        })
    }
}

/// The scalar index of byte offset `at` in `text`.
pub fn scalar_of_byte(text: &str, at: ByteOffset) -> Result<ScalarIndex, OffsetError> {
    let ByteOffset(b) = check_byte(text, at)?;
    let mut n = 0;
    let mut i = 0;
    while i < b {
        if (text.byte_at(i) & 0xC0) != 0x80 {
            n += 1;
        }
        i += 1;
    }
    Ok(ScalarIndex(n))
}

/// The number of UTF-16 code units `text` encodes to.
pub fn utf16_len(text: &str) -> i64 {
    let mut n = 0;
    for c in text.chars() {
        n += utf16_width(c);
    }
    n
}

/// The byte offset of UTF-16 offset `at` in `text`.
pub fn byte_of_utf16(text: &str, at: Utf16Offset) -> Result<ByteOffset, OffsetError> {
    let Utf16Offset(want) = at;
    if want < 0 {
        return Err(OffsetError::OutOfRange {
            offset: want,
            len: utf16_len(text),
        });
    }
    let mut bytes = 0;
    let mut units = 0;
    for c in text.chars() {
        if units == want {
            return Ok(ByteOffset(bytes));
        }
        units += utf16_width(c);
        if units > want {
            return Err(OffsetError::InsideSurrogatePair { offset: want });
        }
        bytes += c.len_utf8() as i64;
    }
    if units == want {
        Ok(ByteOffset(bytes))
    } else {
        Err(OffsetError::OutOfRange {
            offset: want,
            len: units,
        })
    }
}

/// The UTF-16 offset of byte offset `at` in `text`.
pub fn utf16_of_byte(text: &str, at: ByteOffset) -> Result<Utf16Offset, OffsetError> {
    let ByteOffset(b) = check_byte(text, at)?;
    let mut bytes = 0;
    let mut units = 0;
    for c in text.chars() {
        if bytes >= b {
            break;
        }
        bytes += c.len_utf8() as i64;
        units += utf16_width(c);
    }
    Ok(Utf16Offset(units))
}

/// The byte offset of grapheme boundary `at` in `text`.
pub fn byte_of_grapheme(text: &str, at: GraphemeIndex) -> Result<ByteOffset, OffsetError> {
    let GraphemeIndex(g) = at;
    let bounds = grapheme_boundaries(text);
    if g < 0 || g >= bounds.len() as i64 {
        return Err(OffsetError::OutOfRange {
            offset: g,
            len: (bounds.len() as i64 - 1),
        });
    }
    Ok(ByteOffset(bounds[g as usize]))
}

/// The grapheme index of byte offset `at`, which must be a cluster boundary.
pub fn grapheme_of_byte(text: &str, at: ByteOffset) -> Result<GraphemeIndex, OffsetError> {
    let ByteOffset(b) = check_byte(text, at)?;
    let bounds = grapheme_boundaries(text);
    bounds
        .binary_search(&b)
        .map(|i| GraphemeIndex(i as i64))
        .map_err(|_| OffsetError::InsideGrapheme { offset: b })
}

/// A half-open byte range whose ends lie on scalar boundaries of its text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Copy, serde::Serialize, serde::Deserialize)]
pub struct ByteRange {
    pub start: ByteOffset,
    pub end: ByteOffset,
}

impl ByteRange {
    /// `start..end` checked against `text`.
    pub fn checked(
        text: &str,
        start: ByteOffset,
        end: ByteOffset,
    ) -> Result<ByteRange, OffsetError> {
        let start = check_byte(text, start)?;
        let end = check_byte(text, end)?;
        if start > end {
            return Err(OffsetError::Reversed {
                start: start.0,
                end: end.0,
            });
        }
        Ok(ByteRange { start, end })
    }

    /// The byte range of the scalar range `start..end` of `text`.
    pub fn from_scalars(
        text: &str,
        start: ScalarIndex,
        end: ScalarIndex,
    ) -> Result<ByteRange, OffsetError> {
        if start > end {
            return Err(OffsetError::Reversed {
                start: start.0,
                end: end.0,
            });
        }
        Ok(ByteRange {
            start: byte_of_scalar(text, start)?,
            end: byte_of_scalar(text, end)?,
        })
    }

    /// The byte range of the UTF-16 range `start..end` of `text`.
    pub fn from_utf16(
        text: &str,
        start: Utf16Offset,
        end: Utf16Offset,
    ) -> Result<ByteRange, OffsetError> {
        if start > end {
            return Err(OffsetError::Reversed {
                start: start.0,
                end: end.0,
            });
        }
        Ok(ByteRange {
            start: byte_of_utf16(text, start)?,
            end: byte_of_utf16(text, end)?,
        })
    }

    /// This range in scalar indices of `text`.
    pub fn to_scalars(&self, text: &str) -> Result<(ScalarIndex, ScalarIndex), OffsetError> {
        Ok((
            scalar_of_byte(text, self.start)?,
            scalar_of_byte(text, self.end)?,
        ))
    }

    /// This range in UTF-16 offsets of `text`.
    pub fn to_utf16(&self, text: &str) -> Result<(Utf16Offset, Utf16Offset), OffsetError> {
        Ok((
            utf16_of_byte(text, self.start)?,
            utf16_of_byte(text, self.end)?,
        ))
    }

    /// The number of bytes covered.
    pub fn len(&self) -> i64 {
        self.end.0 - self.start.0
    }

    /// Whether the range covers no bytes.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Whether `at` lies in `start..end`.
    pub fn contains(&self, at: ByteOffset) -> bool {
        at >= self.start && at < self.end
    }

    /// The covered part of `text`, which must be the text the range was checked against.
    pub fn slice(&self, text: &str) -> String {
        text.substring(self.start.0, self.end.0).to_string()
    }
}

/// Line starts of one text revision, for line/column conversion in O(log lines).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LineIndex {
    pub text: String,
    starts: Vec<i64>,
}

impl LineIndex {
    /// The index of `text`, split at each `\n` (a `\r` stays line content).
    pub fn new(text: &str) -> LineIndex {
        let mut starts = vec![0];
        let mut i = 0;
        let len = text.byte_len();
        while i < len {
            if text.byte_at(i) == 10 {
                starts.push(i + 1)
            }
            i += 1;
        }
        LineIndex {
            text: text.to_string(),
            starts,
        }
    }

    /// The number of lines; an empty text and a text ending in `\n` both end with an empty line.
    pub fn line_count(&self) -> i64 {
        self.starts.len() as i64
    }

    /// The byte range of `line`, excluding its terminating `\n`.
    pub fn line_range(&self, line: i64) -> Result<ByteRange, OffsetError> {
        let lines = self.starts.len();
        if line < 0 || line >= lines as i64 {
            return Err(OffsetError::LineOutOfRange {
                line,
                lines: lines as i64,
            });
        }
        let end = if line + 1 < lines as i64 {
            self.starts[(line + 1) as usize] - 1
        } else {
            self.text.byte_len()
        };
        Ok(ByteRange {
            start: ByteOffset(self.starts[line as usize]),
            end: ByteOffset(end),
        })
    }

    /// The line and column of byte offset `at`; the `\n` itself is its line's end.
    pub fn position(&self, at: ByteOffset) -> Result<LinePosition, OffsetError> {
        let ByteOffset(b) = check_byte(&self.text, at)?;
        let mut lo = 0;
        let mut hi = self.starts.len();
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if self.starts[mid] <= b {
                lo = mid
            } else {
                hi = mid
            }
        }
        Ok(LinePosition {
            line: lo as i64,
            column: ByteOffset(b - self.starts[lo]),
        })
    }

    /// The byte offset of `pos`; the column may reach, but not pass, the line's end.
    pub fn offset(&self, pos: LinePosition) -> Result<ByteOffset, OffsetError> {
        let range = self.line_range(pos.line)?;
        let ByteOffset(column) = pos.column;
        if column < 0 || column > range.len() {
            return Err(OffsetError::OutOfRange {
                offset: column,
                len: range.len(),
            });
        }
        check_byte(&self.text, ByteOffset(range.start.0 + column))
    }
}

/// The caret slot of byte offset `at` in `layout`; a soft-wrap boundary belongs to the later line.
pub fn visual_position(layout: &TextLayout, at: ByteOffset) -> Result<VisualPosition, OffsetError> {
    let ByteOffset(b) = check_grapheme(&layout.text, at)?;
    if layout.lines.is_empty() {
        return Err(OffsetError::NotLaidOut { offset: b });
    }
    let line = layout.line_of(b);
    match layout.visual_carets(line).iter().position(|c| *c == b) {
        Some(slot) => Ok(VisualPosition {
            line,
            slot: slot as i64,
        }),
        None => Err(OffsetError::NotLaidOut { offset: b }),
    }
}

/// The byte offset at caret slot `pos` of `layout`.
pub fn byte_of_visual(layout: &TextLayout, pos: VisualPosition) -> Result<ByteOffset, OffsetError> {
    let lines = layout.lines.len();
    if pos.line < 0 || pos.line >= lines as i64 {
        return Err(OffsetError::LineOutOfRange {
            line: pos.line,
            lines: lines as i64,
        });
    }
    let carets = layout.visual_carets(pos.line);
    if pos.slot < 0 || pos.slot >= carets.len() as i64 {
        return Err(OffsetError::OutOfRange {
            offset: pos.slot,
            len: (carets.len() as i64 - 1),
        });
    }
    Ok(ByteOffset(carets[pos.slot as usize]))
}

fn utf16_width(c: char) -> i64 {
    if c as u32 >= 0x10000 { 2 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::{
        ByteOffset, ByteRange, GraphemeIndex, LineIndex, LinePosition, OffsetError, ScalarIndex,
        Utf16Offset, VisualPosition, byte_of_grapheme, byte_of_scalar, byte_of_utf16,
        byte_of_visual, check_byte, check_grapheme, grapheme_of_byte, scalar_of_byte, utf16_len,
        utf16_of_byte, visual_position,
    };

    use crate::text::layout;
    use crate::text::layout::{LayoutOptions, TextStyle};

    // "e", e + combining acute (one grapheme, two scalars), U+1F600 (one
    // scalar, two UTF-16 units, four bytes), "z".
    const MIXED: &str = "ee\u{301}\u{1F600}z";

    #[test]
    fn byte_checks_reject_sequence_interiors_and_out_of_range() {
        assert_eq!(check_byte(MIXED, ByteOffset(4)), Ok(ByteOffset(4)));
        assert_eq!(check_byte(MIXED, ByteOffset(2)), Ok(ByteOffset(2)));
        assert_eq!(
            check_byte(MIXED, ByteOffset(3)),
            Err(OffsetError::InsideScalar { offset: 3 })
        );
        assert_eq!(
            check_byte(MIXED, ByteOffset(5)),
            Err(OffsetError::InsideScalar { offset: 5 })
        );
        assert_eq!(check_byte(MIXED, ByteOffset(9)), Ok(ByteOffset(9)));
        assert_eq!(
            check_byte(MIXED, ByteOffset(10)),
            Err(OffsetError::OutOfRange { offset: 10, len: 9 })
        );
        assert_eq!(
            check_byte(MIXED, ByteOffset(-1)),
            Err(OffsetError::OutOfRange { offset: -1, len: 9 })
        )
    }

    #[test]
    fn scalar_indices_round_trip_through_bytes() {
        let expected = [0, 1, 2, 4, 8, 9];
        for (i, b) in expected.iter().enumerate() {
            assert_eq!(
                byte_of_scalar(MIXED, ScalarIndex(i as i64)),
                Ok(ByteOffset(*b))
            );
            assert_eq!(
                scalar_of_byte(MIXED, ByteOffset(*b)),
                Ok(ScalarIndex(i as i64))
            )
        }
        assert_eq!(
            byte_of_scalar(MIXED, ScalarIndex(6)),
            Err(OffsetError::OutOfRange { offset: 6, len: 5 })
        );
        assert_eq!(
            scalar_of_byte(MIXED, ByteOffset(6)),
            Err(OffsetError::InsideScalar { offset: 6 })
        )
    }

    #[test]
    fn utf16_offsets_count_surrogate_pairs_and_refuse_their_middle() {
        assert_eq!(utf16_len(MIXED), 6);
        assert_eq!(byte_of_utf16(MIXED, Utf16Offset(3)), Ok(ByteOffset(4)));
        assert_eq!(
            byte_of_utf16(MIXED, Utf16Offset(4)),
            Err(OffsetError::InsideSurrogatePair { offset: 4 })
        );
        assert_eq!(byte_of_utf16(MIXED, Utf16Offset(5)), Ok(ByteOffset(8)));
        assert_eq!(byte_of_utf16(MIXED, Utf16Offset(6)), Ok(ByteOffset(9)));
        assert_eq!(
            byte_of_utf16(MIXED, Utf16Offset(7)),
            Err(OffsetError::OutOfRange { offset: 7, len: 6 })
        );
        assert_eq!(utf16_of_byte(MIXED, ByteOffset(8)), Ok(Utf16Offset(5)));
        assert_eq!(utf16_of_byte(MIXED, ByteOffset(9)), Ok(Utf16Offset(6)))
    }

    #[test]
    fn grapheme_indices_name_cluster_boundaries_only() {
        assert_eq!(byte_of_grapheme(MIXED, GraphemeIndex(2)), Ok(ByteOffset(4)));
        assert_eq!(byte_of_grapheme(MIXED, GraphemeIndex(4)), Ok(ByteOffset(9)));
        assert_eq!(
            byte_of_grapheme(MIXED, GraphemeIndex(5)),
            Err(OffsetError::OutOfRange { offset: 5, len: 4 })
        );
        assert_eq!(grapheme_of_byte(MIXED, ByteOffset(8)), Ok(GraphemeIndex(3)));
        assert_eq!(
            grapheme_of_byte(MIXED, ByteOffset(2)),
            Err(OffsetError::InsideGrapheme { offset: 2 })
        );
        assert_eq!(
            grapheme_of_byte(MIXED, ByteOffset(3)),
            Err(OffsetError::InsideScalar { offset: 3 })
        );
        assert_eq!(check_grapheme(MIXED, ByteOffset(4)), Ok(ByteOffset(4)));
        let combining = "e\u{301}x";
        assert_eq!(
            check_grapheme(combining, ByteOffset(1)),
            Err(OffsetError::InsideGrapheme { offset: 1 })
        );
        assert_eq!(grapheme_of_byte("", ByteOffset(0)), Ok(GraphemeIndex(0)))
    }

    #[test]
    fn ranges_are_half_open_checked_and_convert_both_ways() {
        let r = ByteRange::from_utf16(MIXED, Utf16Offset(3), Utf16Offset(5));
        assert_eq!(
            r,
            Ok(ByteRange {
                start: ByteOffset(4),
                end: ByteOffset(8)
            })
        );
        let Ok(r) = r else { return };
        assert_eq!(r.slice(MIXED), "\u{1F600}");
        assert_eq!(r.len(), 4);
        assert!(r.contains(ByteOffset(4)));
        assert!(!r.contains(ByteOffset(8)));
        assert_eq!(r.to_scalars(MIXED), Ok((ScalarIndex(3), ScalarIndex(4))));
        assert_eq!(r.to_utf16(MIXED), Ok((Utf16Offset(3), Utf16Offset(5))));
        assert_eq!(
            ByteRange::checked(MIXED, ByteOffset(4), ByteOffset(1)),
            Err(OffsetError::Reversed { start: 4, end: 1 })
        );
        assert_eq!(
            ByteRange::from_scalars(MIXED, ScalarIndex(2), ScalarIndex(1)),
            Err(OffsetError::Reversed { start: 2, end: 1 })
        );
        assert_eq!(
            ByteRange::checked(MIXED, ByteOffset(3), ByteOffset(4)),
            Err(OffsetError::InsideScalar { offset: 3 })
        )
    }

    #[test]
    fn line_index_splits_at_newlines_and_bounds_columns() {
        let index = LineIndex::new("ab\n\u{e9}x\r\n\n");
        assert_eq!(index.line_count(), 4);
        assert_eq!(
            index.line_range(1),
            Ok(ByteRange {
                start: ByteOffset(3),
                end: ByteOffset(7)
            })
        );
        assert_eq!(
            index.line_range(3),
            Ok(ByteRange {
                start: ByteOffset(9),
                end: ByteOffset(9)
            })
        );
        assert_eq!(
            index.line_range(4),
            Err(OffsetError::LineOutOfRange { line: 4, lines: 4 })
        );
        assert_eq!(
            index.position(ByteOffset(2)),
            Ok(LinePosition {
                line: 0,
                column: ByteOffset(2)
            })
        );
        assert_eq!(
            index.position(ByteOffset(5)),
            Ok(LinePosition {
                line: 1,
                column: ByteOffset(2)
            })
        );
        assert_eq!(
            index.position(ByteOffset(9)),
            Ok(LinePosition {
                line: 3,
                column: ByteOffset(0)
            })
        );
        assert_eq!(
            index.position(ByteOffset(4)),
            Err(OffsetError::InsideScalar { offset: 4 })
        );
        assert_eq!(
            index.offset(LinePosition {
                line: 1,
                column: ByteOffset(4)
            }),
            Ok(ByteOffset(7))
        );
        assert_eq!(
            index.offset(LinePosition {
                line: 1,
                column: ByteOffset(5)
            }),
            Err(OffsetError::OutOfRange { offset: 5, len: 4 })
        );
        assert_eq!(
            index.offset(LinePosition {
                line: 1,
                column: ByteOffset(1)
            }),
            Err(OffsetError::InsideScalar { offset: 4 })
        );
        assert_eq!(LineIndex::new("").line_count(), 1)
    }

    #[test]
    fn visual_positions_follow_display_order_in_mixed_direction_lines() {
        // "ab" then Hebrew alef, bet: the Hebrew run displays reversed.
        let text = "ab\u{5D0}\u{5D1}";
        let l = layout::layout(text, &TextStyle::sans(14.0), LayoutOptions::single_line());
        let carets = l.visual_carets(0);
        let mut sorted = carets.clone();
        sorted.sort();
        assert_eq!(sorted, vec![0, 1, 2, 4, 6]);
        let mut last = -1.0;
        for (slot, b) in carets.iter().enumerate() {
            assert_eq!(
                visual_position(&l, ByteOffset(*b)),
                Ok(VisualPosition {
                    line: 0,
                    slot: slot as i64
                })
            );
            assert_eq!(
                byte_of_visual(
                    &l,
                    VisualPosition {
                        line: 0,
                        slot: slot as i64
                    }
                ),
                Ok(ByteOffset(*b))
            );
            assert!(l.caret_x(*b) >= last);
            last = l.caret_x(*b);
        }
        assert_eq!(carets[0], 0);
        // The Hebrew run displays reversed: alef's start sits right of bet's.
        let alef = carets.iter().position(|c| *c == 2);
        let bet = carets.iter().position(|c| *c == 4);
        assert!(alef > bet);
        assert_eq!(
            visual_position(&l, ByteOffset(3)),
            Err(OffsetError::InsideScalar { offset: 3 })
        );
        assert_eq!(
            byte_of_visual(&l, VisualPosition { line: 1, slot: 0 }),
            Err(OffsetError::LineOutOfRange { line: 1, lines: 1 })
        );
        assert_eq!(
            byte_of_visual(&l, VisualPosition { line: 0, slot: 5 }),
            Err(OffsetError::OutOfRange { offset: 5, len: 4 })
        )
    }

    #[test]
    fn errors_render_their_unit_and_position() {
        assert_eq!(
            format!("{}", OffsetError::InsideSurrogatePair { offset: 4 }),
            "UTF-16 offset 4 splits a surrogate pair"
        );
        assert_eq!(
            format!("{}", OffsetError::LineOutOfRange { line: 4, lines: 4 }),
            "line 4 is outside 0..4"
        )
    }
}
