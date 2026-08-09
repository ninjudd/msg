//! Where a needle is allowed to match.
//!
//! One predicate, because a name and a message body have to agree about what a
//! word start is. `search-boundaries.md` argues the rule for message bodies and
//! `naming-a-conversation.md §8` argues that finding a person is one operation;
//! both land here.

/// Does `needle` begin a word somewhere in `haystack`?
///
/// Both are expected to be lowercased already: one query is matched against
/// many candidates, so folding the query once is cheaper than folding every
/// candidate, and every caller here already had a lowercased needle.
///
/// The rule is asymmetric on purpose. A match has to *start* where a word
/// starts, and nothing is asserted about where it ends, so `start` still finds
/// `starting` while `art` stops finding `apartment`. Requiring both ends is
/// whole-word matching, which breaks typing a prefix — most of what anyone does
/// in a search box (`search-boundaries.md §2`).
///
/// Existential over occurrences: a haystack qualifies when *any* occurrence
/// begins a word. `we started at six, is that art deco` matches `art` on its
/// second occurrence, and an implementation that tested only the first would
/// answer no.
pub fn begins_a_word(haystack: &str, needle: &str) -> bool {
    let Some(first) = needle.chars().next() else {
        // What `contains("")` answered, and what every caller relied on.
        return true;
    };
    // A needle that is not word-shaped has no word start to sit on. `😂` after
    // a letter is a perfectly good match, and `?!` never begins a word at all,
    // so the rule is skipped rather than applied and always failing.
    if !first.is_alphanumeric() || is_scriptio_continua(first) {
        return haystack.contains(needle);
    }
    haystack
        .match_indices(needle)
        .any(|(at, _)| match haystack[..at].chars().next_back() {
            None => true,
            Some(before) => !before.is_alphanumeric(),
        })
}

/// Is this character from a script written without spaces between words?
///
/// Han, Hiragana, Katakana and Hangul are alphanumeric and are written with no
/// spaces, so the rule above would reject nearly every match in Chinese,
/// Japanese or Korean — turning working search into almost none, which is far
/// worse than the noise the rule removes. Real word segmentation needs a
/// dictionary this program is not going to carry, so the rule simply steps
/// aside (`search-boundaries.md §4`).
///
/// The arms have to cover every block the sentence above names, which is easier
/// to get wrong than it looks: halfwidth katakana sits far from the main kana
/// block, Hangul compatibility jamo falls in the gap between kana and the
/// Hangul syllables, and the ideographs continue into the supplementary plane.
/// A character these miss is one the rule is applied to and cannot pass.
fn is_scriptio_continua(ch: char) -> bool {
    matches!(ch,
        '\u{1100}'..='\u{11ff}'    // Hangul Jamo
        | '\u{3040}'..='\u{30ff}'  // Hiragana and Katakana
        | '\u{3130}'..='\u{318f}'  // Hangul Compatibility Jamo
        | '\u{3400}'..='\u{4dbf}'  // CJK Unified Ideographs Extension A
        | '\u{4e00}'..='\u{9fff}'  // CJK Unified Ideographs
        | '\u{ac00}'..='\u{d7af}'  // Hangul syllables
        | '\u{f900}'..='\u{faff}'  // CJK Compatibility Ideographs
        | '\u{ff66}'..='\u{ff9f}'  // Halfwidth Katakana
        | '\u{20000}'..='\u{2fa1f}' // CJK Extension B onwards, and the
                                     // compatibility supplement
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four cases `search-boundaries.md §2` asks a test to pin.
    #[test]
    fn a_match_has_to_start_where_a_word_starts() {
        assert!(begins_a_word("starting tomorrow", "start"), "a prefix");
        assert!(!begins_a_word("we started at six", "art"), "inside a word");
        assert!(
            !begins_a_word("the apartment above", "art"),
            "inside another"
        );
        assert!(begins_a_word("is that art deco", "art"), "a whole word");
        // The one that separates the rule from a plausible implementation of
        // it: the first occurrence is interior and the second is a real hit.
        assert!(
            begins_a_word("we started at six, is that art deco", "art"),
            "any occurrence, not the first"
        );
    }

    /// The case that prompted it: a first name inside a surname.
    #[test]
    fn a_first_name_does_not_match_inside_a_surname() {
        assert!(begins_a_word("ana duarte", "ana"));
        assert!(!begins_a_word("dana reyes", "ana"));
        assert!(!begins_a_word("susana vidal", "ana"));
        // A surname is still a word, so it is still reachable.
        assert!(begins_a_word("dana reyes", "reyes"));
    }

    /// Punctuation is a boundary, which is what makes a hyphenated name work.
    #[test]
    fn anything_that_is_not_alphanumeric_opens_a_word() {
        assert!(begins_a_word("jean-luc picard", "luc"));
        assert!(begins_a_word("o'brien", "brien"));
        assert!(begins_a_word("(310) 555-1234", "555"));
        // A digit is alphanumeric, so this is interior and stays out.
        assert!(!begins_a_word("covid19", "19"));
    }

    /// A needle with no word start of its own is matched as a substring.
    #[test]
    fn a_needle_that_is_not_word_shaped_skips_the_rule() {
        assert!(begins_a_word("lol😂", "😂"), "after a letter");
        assert!(begins_a_word("wait?!", "?!"));
        assert!(begins_a_word("see https://example.com", "://"));
        assert!(begins_a_word("anything", ""), "what contains(\"\") gave");
    }

    /// Scripts written without spaces would otherwise lose almost every match.
    #[test]
    fn a_script_without_spaces_is_matched_as_a_substring() {
        // Interior by the alphanumeric rule, and the only way these are ever
        // written, so the rule steps aside rather than answering no.
        assert!(begins_a_word("私は東京に行きます", "東京"));
        assert!(begins_a_word("我住在北京市", "北京"));
        assert!(begins_a_word("서울에서 만나요", "울에"));
    }

    /// Every block the comment above the carve-out names, including the three
    /// that sit away from the obvious ranges.
    #[test]
    fn the_carve_out_covers_the_blocks_it_claims() {
        // Halfwidth katakana, which is the one of these that turns up in
        // ordinary Japanese messages rather than in rare-ideograph territory.
        assert!(begins_a_word("ｱｲｶﾀｶﾅ", "ｶﾀ"));
        // Hangul compatibility jamo, between the kana and syllable blocks.
        assert!(begins_a_word("ㄱㄴㄷㄹ", "ㄴㄷ"));
        // Ideographs in the supplementary plane.
        assert!(begins_a_word("\u{20000}\u{20001}\u{20002}", "\u{20001}"));
    }

    /// Case folding is the caller's job, and stays the caller's job.
    #[test]
    fn both_sides_are_expected_lowercased() {
        assert!(begins_a_word("ana duarte", "ana"));
        // Not folded here, which is why every caller lowercases first.
        assert!(!begins_a_word("Ana Duarte", "ana"));
    }
}
