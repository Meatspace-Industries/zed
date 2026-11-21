//! Shared paragraph line-break adjustments. The caller owns shaping and legal break points.

use unicode_segmentation::UnicodeSegmentation;

/// The nearest earlier legal break that removes a one-word final line. `legal_breaks` must be
/// sorted and must already exclude breaks inside shaped clusters. `fits` measures the entire
/// proposed last line, using the same shaping and trailing-space rules as the caller.
pub(super) fn avoid_widow(
    text: &str,
    previous_start: usize,
    last_start: usize,
    legal_breaks: &[usize],
    mut fits: impl FnMut(usize) -> bool,
) -> Option<usize> {
    // A forced break inside an overlong word is not a widow. Never move text across a hard
    // break, even when a caller supplies more than one paragraph.
    if legal_breaks.binary_search(&last_start).is_err()
        || text[last_start..].unicode_words().take(2).count() != 1
        || text[previous_start..].contains([
            '\n', '\r', '\u{000b}', '\u{000c}', '\u{0085}', '\u{2028}', '\u{2029}',
        ])
    {
        return None;
    }

    legal_breaks.iter().rev().copied().find(|&candidate| {
        candidate > previous_start
            && candidate < last_start
            && text
                .grapheme_indices(true)
                .any(|(index, _)| index == candidate)
            && text[previous_start..candidate]
                .unicode_words()
                .next()
                .is_some()
            && text[candidate..].unicode_words().take(2).count() == 2
            && fits(candidate)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_only_the_nearest_fitting_legal_break() {
        let text = "one two three four";
        assert_eq!(
            avoid_widow(text, 0, 14, &[0, 4, 8, 14, 18], |start| text[start..].len()
                <= 10),
            Some(8)
        );
        assert_eq!(
            avoid_widow(text, 0, 14, &[0, 4, 8, 14, 18], |start| text[start..].len()
                <= 9),
            None
        );
        // A shaped cluster can suppress the nearest Unicode break.
        assert_eq!(
            avoid_widow(text, 0, 14, &[0, 4, 14, 18], |start| text[start..].len()
                <= 14),
            Some(4)
        );
    }

    #[test]
    fn preserves_hard_breaks_and_nonempty_previous_lines() {
        for hard_break in [
            '\n', '\r', '\u{000b}', '\u{000c}', '\u{0085}', '\u{2028}', '\u{2029}',
        ] {
            let text = format!("one two{hard_break}three four");
            let last = text.rfind("four").unwrap();
            assert_eq!(avoid_widow(&text, 0, last, &[0, 4, last], |_| true), None);
        }
        assert_eq!(avoid_widow("one two", 0, 4, &[0, 4, 7], |_| true), None);
        assert_eq!(
            avoid_widow("one two three", 4, 8, &[0, 4, 8, 13], |_| true),
            None
        );
    }

    #[test]
    fn does_not_treat_a_forced_word_fragment_as_a_widow() {
        assert_eq!(
            avoid_widow("one extraordinary", 0, 10, &[0, 4, 17], |_| true),
            None
        );
        assert_eq!(
            avoid_widow("one two three four", 0, 8, &[0, 4, 8, 14, 18], |_| true),
            None
        );
    }

    #[test]
    fn cannot_split_a_grapheme_even_if_the_shaper_allows_it() {
        let text = "one e\u{301}cho data";
        let last = text.rfind("data").unwrap();
        // The artificial break at 5 is inside the base + combining-mark grapheme.
        assert_eq!(
            avoid_widow(text, 0, last, &[0, 5, last, text.len()], |_| true),
            None
        );
    }
}
