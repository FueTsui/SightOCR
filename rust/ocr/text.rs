//! Joining recognized text fragments. Chinese and Japanese are written without
//! spaces, so a wrapped line or a letter-spaced label must not gain one; Korean,
//! Latin and other scripts keep word spaces.

/// Characters of scripts written without inter-word spaces, plus their
/// punctuation and full-width forms. Hangul is excluded deliberately.
pub fn is_unspaced_script(c: char) -> bool {
    matches!(c as u32,
        0x2E80..=0x2FDF      // CJK radicals, Kangxi radicals
        | 0x3000..=0x303F    // CJK symbols and punctuation
        | 0x3040..=0x30FF    // Hiragana, Katakana
        | 0x3100..=0x312F    // Bopomofo
        | 0x3190..=0x33FF    // Kanbun … CJK compatibility (after Hangul jamo)
        | 0x3400..=0x4DBF    // CJK extension A
        | 0x4E00..=0x9FFF    // CJK unified ideographs
        | 0xF900..=0xFAFF    // CJK compatibility ideographs
        | 0xFE30..=0xFE4F    // CJK compatibility forms
        | 0xFF00..=0xFF60    // Full-width ASCII forms and punctuation
        | 0xFF61..=0xFF9F    // Half-width CJK punctuation and Katakana
        | 0x20000..=0x3FFFF) // CJK extensions B–H
}

/// Whether two fragments from different visual lines (a wrapped cell or paragraph)
/// need a space between them.
pub fn wrap_needs_space(left: &str, right: &str) -> bool {
    let (Some(last), Some(first)) = (left.chars().next_back(), right.chars().next()) else {
        return false;
    };
    // A line broken inside a range, path, hyphenated word or bracket continues
    // directly; closing punctuation never starts a new word.
    !(is_unspaced_script(last)
        || is_unspaced_script(first)
        || matches!(last, '-' | '~' | '/' | '(' | '[' | '{')
        || matches!(
            first,
            ')' | ']' | '}' | ',' | '.' | ';' | ':' | '%' | '!' | '?'
        ))
}

/// Append a wrapped fragment, inserting a space only where the script uses one.
pub fn push_wrapped(target: &mut String, fragment: &str) {
    let fragment = fragment.trim();
    if fragment.is_empty() {
        return;
    }
    if wrap_needs_space(target, fragment) {
        target.push(' ');
    }
    target.push_str(fragment);
}

/// Full-width punctuation: a space beside it is never meaningful.
fn is_full_width_punctuation(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F
        | 0xFE30..=0xFE4F
        | 0xFF01..=0xFF0F
        | 0xFF1A..=0xFF20
        | 0xFF3B..=0xFF40
        | 0xFF5B..=0xFF65)
}

/// Collapse whitespace inside one recognized line: runs become one space, and a
/// space between two unspaced-script characters (`锅 炉`, `合 计`) or beside
/// full-width punctuation (`Ⅳ 、V`) is removed.
pub fn tidy_line(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        let joined = matches!(
            (output.chars().next_back(), word.chars().next()),
            (Some(last), Some(first))
                if (is_unspaced_script(last) && is_unspaced_script(first))
                    || is_full_width_punctuation(last)
                    || is_full_width_punctuation(first)
        );
        if !output.is_empty() && !joined {
            output.push(' ');
        }
        output.push_str(word);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_chinese_and_japanese_join_without_spaces_but_other_scripts_keep_them() {
        for (left, right, joined) in [
            ("中国建筑", "合同管理表格", "中国建筑合同管理表格"),
            ("现行国家标", "准《公共建", "现行国家标准《公共建"),
            ("设计标准》", "GB 50189", "设计标准》GB 50189"),
            ("提高", "16%", "提高16%"),
            ("多联式空调", "(热泵)机组", "多联式空调(热泵)机组"),
            ("Ⅲ、Ⅳ", "、V", "Ⅲ、Ⅳ、V"),
            ("平均4~", "6层", "平均4~6层"),
            ("能效比(EER)、", "能源消耗效率", "能效比(EER)、能源消耗效率"),
            ("東京都", "千代田区", "東京都千代田区"),
            ("multi-", "line", "multi-line"),
            ("Total", "amount", "Total amount"),
            ("안녕하세요", "감사합니다", "안녕하세요 감사합니다"),
            ("Price", "(USD)", "Price (USD)"),
            ("12", "%", "12%"),
        ] {
            let mut cell = left.to_owned();
            push_wrapped(&mut cell, right);
            assert_eq!(cell, joined, "{left} + {right}");
        }
        let mut empty = String::new();
        push_wrapped(&mut empty, "  首行  ");
        assert_eq!(empty, "首行");
    }

    #[test]
    fn tidy_line_removes_only_spaces_inside_unspaced_scripts() {
        assert_eq!(tidy_line("锅 炉"), "锅炉");
        assert_eq!(tidy_line("合  计 Total"), "合计 Total");
        assert_eq!(tidy_line("GB  50189\t标准"), "GB 50189 标准");
        assert_eq!(tidy_line("한국어  텍스트"), "한국어 텍스트");
        assert_eq!(tidy_line("A\tB\nC"), "A B C");
        assert_eq!(tidy_line("Ⅲ、Ⅳ 、V"), "Ⅲ、Ⅳ、V");
        assert_eq!(tidy_line("设计标准》 GB 50189"), "设计标准》GB 50189");
        assert_eq!(tidy_line("提高 16%"), "提高 16%");
    }
}
