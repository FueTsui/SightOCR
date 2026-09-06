//! Explicit native font choices for Unicode script runs.
//!
//! Windows RichEdit's historical charset-based font binding can select a font
//! for a different script in mixed OCR results. Assigning the whole script run
//! lets its native shaper keep consonants, matras, joiners and accents together.
//! Family names were checked against the installed Windows fonts' name tables.

pub(super) fn font_runs(text: &str) -> Vec<(i32, i32, &'static str)> {
    let mut runs: Vec<(i32, i32, &'static str)> = Vec::new();
    let mut offset = 0i32;
    let mut previous = None;
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        let font = if is_inherited(character) {
            previous.unwrap_or_else(|| {
                // A leading mark has no preceding base; keep it with the next
                // script instead of splitting the sequence into different fonts.
                characters
                    .clone()
                    .find(|c| !is_inherited(*c))
                    .map(font_for_character)
                    .unwrap_or("Segoe UI")
            })
        } else {
            font_for_character(character)
        };
        let end = offset.saturating_add(character.len_utf16() as i32);
        if let Some(last) = runs.last_mut().filter(|run| run.2 == font) {
            last.1 = end;
        } else {
            runs.push((offset, end, font));
        }
        offset = end;
        if character == '\r' || character == '\n' {
            previous = None;
        } else if !character.is_whitespace() || previous.is_none() {
            previous = Some(font);
        }
    }
    runs
}

fn is_inherited(character: char) -> bool {
    matches!(
        character as u32,
        // Shared combining marks, bidi/zero-width controls, variation selectors,
        // combining half marks, emoji skin tones and emoji tag sequences.
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF
            | 0x200C..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069
            | 0x20D0..=0x20FF | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F
            | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F | 0xE0100..=0xE01EF
    )
}

fn font_for_character(character: char) -> &'static str {
    match character as u32 {
        // All characters in each Indic block include that script's combining
        // marks. Do not split those marks into a generic symbol/Latin font.
        0x0900..=0x0DFF | 0x1CD0..=0x1CFF | 0xA8E0..=0xA8FF => "Nirmala UI",
        0x0E00..=0x0EFF | 0x1780..=0x17FF | 0x19E0..=0x19FF => "Leelawadee UI",
        0x0F00..=0x0FFF => "Microsoft Himalaya",
        0x1000..=0x109F | 0xA9E0..=0xA9FF | 0xAA60..=0xAA7F => "Myanmar Text",
        0x1800..=0x18AF | 0x11660..=0x1167F => "Mongolian Baiti",
        0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xAC00..=0xD7FF | 0xFFA0..=0xFFDC => {
            "Malgun Gothic"
        }
        0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF61..=0xFF9F | 0x1B000..=0x1B16F => "Yu Gothic UI",
        0x2E80..=0x303F
        | 0x3100..=0x312F
        | 0x31A0..=0x31BF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF => "Microsoft YaHei UI",
        0x20000..=0x2A6DF | 0x2F800..=0x2FA1F => "SimSun-ExtB",
        0x30000..=0x3134F => "SimSun-ExtG",
        0x07C0..=0x07FF
        | 0x1200..=0x139F
        | 0x2D30..=0x2DDF
        | 0xA4D0..=0xA4FF
        | 0xA500..=0xA63F
        | 0xA6A0..=0xA6FF
        | 0xAB00..=0xAB2F
        | 0x10480..=0x104AF
        | 0x1E900..=0x1E95F => "Ebrima",
        0x13A0..=0x167F | 0x18B0..=0x18FF | 0xAB70..=0xABBF => "Gadugi",
        0x1950..=0x197F => "Microsoft Tai Le",
        0x1980..=0x19DF => "Microsoft New Tai Lue",
        0xA000..=0xA4CF => "Microsoft Yi Baiti",
        0xA840..=0xA87F => "Microsoft PhagsPa",
        0xA980..=0xA9DF => "Javanese Text",
        0x1F000..=0x1FAFF => "Segoe UI Emoji",
        0x2100..=0x23FF | 0x27C0..=0x27EF | 0x2980..=0x2AFF => "Segoe UI Symbol",
        // Segoe UI covers Latin (including Vietnamese), Greek, Cyrillic, Arabic,
        // Hebrew, Armenian and Georgian. Automatic binding remains the last
        // fallback for other scripts and for absent optional Windows fonts.
        _ => "Segoe UI",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_scripts_choose_complete_native_families() {
        let text = "العربية\rहिन्दी\rภาษาไทย\rខ្មែរ\rᠮᠣᠩᠭᠣᠯ\rதமிழ்\r안녕하세요\r日本語";
        let utf16: Vec<_> = text.encode_utf16().collect();
        let runs = font_runs(text);
        for (sample, family) in [
            ("العربية", "Segoe UI"),
            ("हिन्दी", "Nirmala UI"),
            ("ภาษาไทย", "Leelawadee UI"),
            ("ខ្មែរ", "Leelawadee UI"),
            ("ᠮᠣᠩᠭᠣᠯ", "Mongolian Baiti"),
            ("தமிழ்", "Nirmala UI"),
            ("안녕하세요", "Malgun Gothic"),
        ] {
            assert!(
                runs.iter().any(|&(start, end, font)| {
                    font == family
                        && String::from_utf16(&utf16[start as usize..end as usize])
                            .unwrap()
                            .contains(sample)
                }),
                "No complete {family} script run for {sample}"
            );
        }
    }

    #[test]
    fn grapheme_marks_joiners_and_surrogates_stay_with_their_script() {
        assert_eq!(font_runs("कि\u{200D}क्षि"), vec![(0, 7, "Nirmala UI")]);
        assert_eq!(font_runs("ก\u{0301}"), vec![(0, 2, "Leelawadee UI")]);
        assert_eq!(font_runs("\u{0301}ก"), vec![(0, 2, "Leelawadee UI")]);
        assert_eq!(font_runs("👩🏽\u{200D}💻"), vec![(0, 7, "Segoe UI Emoji")]);
        assert_eq!(font_runs("𠀀\u{E0100}"), vec![(0, 4, "SimSun-ExtB")]);
        assert_eq!(font_runs("e\u{0301}"), vec![(0, 2, "Segoe UI")]);
    }

    #[test]
    fn font_runs_partition_utf16_without_altering_text() {
        for text in ["", "ABC", "한\r\nالعربية 日本語 😃 हिन्दी", "\u{0301}"]
        {
            let runs = font_runs(text);
            let utf16: Vec<_> = text.encode_utf16().collect();
            let mut end = 0;
            let mut restored = String::new();
            for (start, next, _) in runs {
                assert_eq!(start, end);
                assert!(next > start);
                restored
                    .push_str(&String::from_utf16(&utf16[start as usize..next as usize]).unwrap());
                end = next;
            }
            assert_eq!(end as usize, utf16.len());
            assert_eq!(restored, text);
        }
    }

    #[test]
    fn country_names_keep_arabic_marks_and_french_accents_in_complete_font_runs() {
        let samples = [
            "البحرين",
            "الأردن",
            "عُمان",
            "قطر",
            "الكويت",
            "المملكة العربية السعودية",
            "مصر",
            "الإمارات العربية المتحدة",
            "Ré\u{200B}publique Centrafricaine",
            "Côte d’Ivoire",
            "Sénégal",
            "Guinée Équatoriale",
            "Bahrain / البحرين / Bahreïn 123",
            "123 - عُمان / Oman",
            "Côte d’Ivoire / Ivory Coast / ساحل العاج",
        ];
        for text in samples {
            assert_eq!(
                font_runs(text),
                vec![(0, text.encode_utf16().count() as i32, "Segoe UI")],
                "Arabic joining/harakat and French accents must stay with their base font: {text}",
            );
        }
    }
}
