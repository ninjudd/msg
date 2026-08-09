//! Where a needle is allowed to match.
//!
//! One predicate, because a name and a message body have to agree about what a
//! word start is. `search-boundaries.md` argues the rule for message bodies and
//! `naming-a-conversation.md §8` argues that finding a person is one operation;
//! both land here.

/// Does `needle` begin a word somewhere in `haystack`?
///
/// The needle is expected lowercased already — one query is matched against
/// many candidates, so folding it once is cheaper than folding it per
/// candidate. The haystack is taken as it is, and that is load-bearing rather
/// than a convenience: folding it first can invent a boundary. `İ` lowercases
/// to `i` plus a combining dot, the dot is not alphanumeric, and so a
/// lowercased `İart` shows a word start before `art` that the original does
/// not have. Case is folded per character during the scan instead, and the
/// boundary is read from the unfolded text.
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
        return run_contains(haystack, needle);
    }
    haystack.char_indices().any(|(at, ch)| {
        folds_to(ch, first)
            && starts_with_ignoring_case(&haystack[at..], needle)
            && match haystack[..at].chars().next_back() {
                None => true,
                Some(before) => !before.is_alphanumeric(),
            }
    })
}

/// Does `needle` occur in `haystack`, folding case per character?
///
/// Almost every position fails on its first character, so that test is worth
/// making cheap: comparing it before building the two folding iterators is
/// most of the difference between this and a scan that folds everything.
pub(crate) fn run_contains(haystack: &str, needle: &str) -> bool {
    let Some(first) = needle.chars().flat_map(char::to_lowercase).next() else {
        return true;
    };
    haystack
        .char_indices()
        .any(|(at, ch)| folds_to(ch, first) && starts_with_ignoring_case(&haystack[at..], needle))
}

fn folds_to(ch: char, lowered: char) -> bool {
    if ch.is_ascii() {
        return ch.to_ascii_lowercase() == lowered;
    }
    ch.to_lowercase().next() == Some(lowered)
}

fn starts_with_ignoring_case(haystack: &str, needle: &str) -> bool {
    let mut found = haystack.chars().flat_map(char::to_lowercase);
    needle
        .chars()
        .flat_map(char::to_lowercase)
        .all(|wanted| found.next() == Some(wanted))
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
        | '\u{31f0}'..='\u{31ff}'  // Katakana Phonetic Extensions
        | '\u{3400}'..='\u{4dbf}'  // CJK Unified Ideographs Extension A
        | '\u{4e00}'..='\u{9fff}'  // CJK Unified Ideographs
        | '\u{a960}'..='\u{a97f}'  // Hangul Jamo Extended-A
        | '\u{ac00}'..='\u{d7af}'  // Hangul syllables
        | '\u{d7b0}'..='\u{d7ff}'  // Hangul Jamo Extended-B
        | '\u{f900}'..='\u{faff}'  // CJK Compatibility Ideographs
        | '\u{ff66}'..='\u{ff9f}'  // Halfwidth Katakana
        | '\u{1aff0}'..='\u{1b16f}' // Kana Extended-A and -B, the Kana
                                     // Supplement, and Small Kana Extension
        | '\u{20000}'..='\u{3ffff}' // The supplementary and tertiary
                                     // ideographic planes, whole: every
                                     // assignment there is an ideograph, and
                                     // extensions keep landing in them — G and
                                     // H already sit past the 2FA1F this
                                     // stopped at when it only knew B
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

    /// Every block the comment above the carve-out names, including the ones
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
        // Extension G, past the 2FA1F the range used to stop at.
        assert!(begins_a_word("\u{30000}\u{30001}\u{30002}", "\u{30001}"));
        // Katakana phonetic extensions, past the main kana block.
        assert!(begins_a_word("\u{31f0}\u{31f1}\u{31f2}", "\u{31f1}"));
    }

    /// The needle is the caller's to fold; the haystack must go in unfolded.
    #[test]
    fn the_boundary_is_read_from_the_unfolded_text() {
        // Case is folded per character during the scan.
        assert!(begins_a_word("Ana Duarte", "ana"));
        assert!(!begins_a_word("ana duarte", "Ana"), "the needle is not");
        // Folding the haystack first would invent the boundary here: `İ`
        // lowercases to `i` plus a combining dot, and the dot is not
        // alphanumeric, so `İart` folded shows `art` starting a word. Unfolded,
        // `İ` is a letter and the occurrence is interior.
        assert!(!begins_a_word("is that İart deco", "art"));
        assert!(
            !begins_a_word("İart deco", "İart"),
            "İ never survives a fold"
        );
        assert!(begins_a_word("İart deco", "i\u{307}art"), "its fold does");
    }
}
