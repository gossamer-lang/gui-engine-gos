use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, GeneralCategoryGroup, UnicodeGeneralCategory};
use unicode_segmentation::UnicodeSegmentation;

/// Whether `c` is in a letter category (`L*`).
pub fn is_letter(c: char) -> bool {
    c.general_category_group() == GeneralCategoryGroup::Letter
}

/// Whether `c` is in a number category (`N*`).
pub fn is_number(c: char) -> bool {
    c.general_category_group() == GeneralCategoryGroup::Number
}

/// Whether `c` is a combining mark (`M*`).
pub fn is_mark(c: char) -> bool {
    c.general_category_group() == GeneralCategoryGroup::Mark
}

/// Whether `c` is an uppercase letter (`Lu`).
pub fn is_upper(c: char) -> bool {
    c.general_category() == GeneralCategory::UppercaseLetter
}

/// Whether `c` separates words: a separator category (`Z*`) or a C0/C1
/// layout control.
pub fn is_space(c: char) -> bool {
    c.general_category_group() == GeneralCategoryGroup::Separator
        || matches!(c, '\t' | '\n' | '\u{000B}' | '\u{000C}' | '\r' | '\u{0085}')
}

/// The full case folding of `text`, equal for strings that match ignoring case.
pub fn fold_case(text: &str) -> String {
    unicase::UniCase::new(text).to_folded_case()
}

/// The canonical decomposition (NFD) of `text`.
pub fn nfd(text: &str) -> String {
    text.nfd().collect()
}

/// The extended grapheme clusters of `text`, in order.
pub fn graphemes(text: &str) -> impl Iterator<Item = &str> {
    text.graphemes(true)
}

/// How many extended grapheme clusters `text` holds.
pub fn grapheme_count(text: &str) -> i64 {
    text.graphemes(true).count() as i64
}
