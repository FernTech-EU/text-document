//! The number an ordered list may start at.
//!
//! # The failure this prevents
//!
//! A list keeps the number its first item wears (`List::start`), read from Djot, Markdown,
//! HTML and a pasted fragment. The Djot and HTML readers took any number a 64-bit integer
//! holds: a list starting at the largest one overflowed the Djot writer's count at its
//! second item, which aborts a save in a debug build and writes a wrong number in a release
//! one, and a LaTeX export set a counter past the 2^31 - 1 TeX holds, which the writer's
//! LaTeX then refused.
//!
//! # The limit
//!
//! [`MAX_LIST_START`] is 999,999,999: nine digits, the most a CommonMark list marker reads,
//! which the Markdown writer already stops at. A list with that many items more still fits
//! the 32-bit counters of LaTeX and DOCX. A start past it is read as the limit, and a start
//! below 0, which no saved syntax writes, as 1.

/// The largest number a list's first item may wear.
pub const MAX_LIST_START: i64 = 999_999_999;

/// The start a list keeps for a first item written `start`: `None` for 1, which every list
/// starts at unless it says otherwise, and every number brought within `0..=`
/// [`MAX_LIST_START`], a number below 0 as 1.
pub fn list_start(start: i64) -> Option<i64> {
    let start = if start < 0 {
        1
    } else {
        start.min(MAX_LIST_START)
    };
    (start != 1).then_some(start)
}

/// As [`list_start`], for a number a parser read as unsigned.
pub fn list_start_from_unsigned(start: u64) -> Option<i64> {
    list_start(i64::try_from(start).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_start_is_brought_within_the_limit() {
        assert_eq!(list_start(1), None);
        assert_eq!(list_start(0), Some(0));
        assert_eq!(list_start(3), Some(3));
        assert_eq!(list_start(-3), None);
        assert_eq!(list_start(MAX_LIST_START), Some(MAX_LIST_START));
        assert_eq!(list_start(i64::MAX), Some(MAX_LIST_START));
        assert_eq!(list_start_from_unsigned(u64::MAX), Some(MAX_LIST_START));
        assert_eq!(list_start_from_unsigned(1), None);
    }
}
