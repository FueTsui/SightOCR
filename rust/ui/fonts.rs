use eframe::egui::{self, FontDefinitions, FontFamily};
use std::path::{Path, PathBuf};

pub(super) fn install(ctx: &egui::Context) {
    ctx.set_fonts(definitions(&windows_fonts_dir()));
}

fn windows_fonts_dir() -> PathBuf {
    std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:/Windows"))
        .join("Fonts")
}

fn definitions(directory: &Path) -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let both_families = [FontFamily::Proportional, FontFamily::Monospace];
    add_first_available(
        &mut fonts,
        directory,
        "chinese",
        &["msyh.ttc", "simhei.ttf", "simsun.ttc"],
        &both_families,
        true,
    );
    // Keep the existing Windows UI and Chinese appearance.
    add_first_available(
        &mut fonts,
        directory,
        "windows-ui",
        &["SegUIVar.ttf", "segoeui.ttf"],
        &[FontFamily::Proportional],
        true,
    );
    // egui only searches explicitly registered fonts; it does not use Windows
    // font linking. Segoe UI and YaHei lack Hangul, so valid Korean OCR text
    // otherwise becomes replacement squares in both result editors and tables.
    add_first_available(
        &mut fonts,
        directory,
        "korean",
        &["malgun.ttf", "gulim.ttc", "batang.ttc"],
        &both_families,
        false,
    );
    add_first_available(
        &mut fonts,
        directory,
        "japanese",
        &["YuGothM.ttc", "meiryo.ttc", "msgothic.ttc"],
        &both_families,
        false,
    );
    // Segoe UI Variable is an appearance choice, not a superset of Segoe UI:
    // Windows ships its Arabic/Hebrew and other script coverage in segoeui.ttf.
    // Register that separately, including in tables' monospace fallback chain.
    // The compact script families below are part of the Windows language/font
    // fallback system. egui cannot consult that system itself. Loading one regular
    // face per script group covers international OCR without reading the entire
    // Fonts directory (hundreds of files, including many duplicate bold faces).
    for &(key, candidates) in SCRIPT_FALLBACKS {
        add_first_available(
            &mut fonts,
            directory,
            key,
            candidates,
            &both_families,
            false,
        );
    }
    fonts
}

const SCRIPT_FALLBACKS: &[(&str, &[&str])] = &[
    (
        "international-ui",
        &["segoeui.ttf", "tahoma.ttf", "arial.ttf"],
    ),
    // Devanagari, Bengali, Gurmukhi, Gujarati, Oriya, Tamil, Telugu, Kannada,
    // Malayalam, Sinhala, and their combining marks.
    ("indic", &["Nirmala.ttf", "NirmalaS.ttf"]),
    ("southeast-asian", &["LeelawUI.ttf", "LeelUIsl.ttf"]),
    ("mongolian", &["monbaiti.ttf"]),
    ("myanmar", &["mmrtext.ttf"]),
    ("african", &["ebrima.ttf"]),
    ("american", &["gadugi.ttf"]),
    ("tibetan", &["himalaya.ttf"]),
    ("javanese", &["javatext.ttf"]),
    ("yi", &["msyi.ttf"]),
    ("tai-lue", &["ntailu.ttf"]),
    ("tai-le", &["taile.ttf"]),
    ("phags-pa", &["phagspa.ttf"]),
    ("symbols", &["seguisym.ttf"]),
];

fn add_first_available(
    fonts: &mut FontDefinitions,
    directory: &Path,
    key: &str,
    candidates: &[&str],
    families: &[FontFamily],
    primary: bool,
) {
    for candidate in candidates {
        if let Ok(bytes) = std::fs::read(directory.join(candidate)) {
            fonts
                .font_data
                .insert(key.into(), egui::FontData::from_owned(bytes).into());
            for family in families {
                let names = fonts.families.entry(family.clone()).or_default();
                if primary {
                    names.insert(0, key.into());
                } else {
                    names.push(key.into());
                }
            }
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Native-script samples for every language selectable in the application.
    // Include accents/combining marks and the non-Latin variants whose coverage
    // cannot be inferred from English or simplified Chinese.
    const LANGUAGE_SAMPLES: &[(&str, &str)] = &[
        ("auto", ""),
        ("zh-Hans", "中文识别结果，数字与标点。"),
        ("zh-Hant", "繁體中文辨識結果，謝謝您。"),
        ("yue", "廣東話：佢哋喺邊度？唔該晒。"),
        ("en", "English text 0123456789"),
        ("ja", "日本語：こんにちは、カタカナ。"),
        ("ko", "안녕하세요감사합니다한국어힣한ㄱㅏ"),
        ("fr", "Français : éèêë àâ ç œ îï ô ùûü ÿ"),
        ("es", "Español: áéíóú ñ ü ¿Qué tal? ¡Hola!"),
        ("ru", "Русский язык: Съешь ещё этих мягких булок."),
        ("de", "Deutsch: ÄÖÜ äöü ß Grüße"),
        ("it", "Italiano: àèéìíîòóùú"),
        ("tr", "Türkçe: Çç Ğğ İı Öö Şş Üü"),
        ("pt-PT", "Português: áàâã ç éê í óôõ ú"),
        ("pt", "Português brasileiro: Olá, você está bem?"),
        (
            "vi",
            "Tiếng Việt: Nguyễn Trường, ăâđêôơư ắằẳẵặ ấầẩẫậ ếềểễệ ốồổỗộ ớờởỡợ ứừửữự",
        ),
        ("id", "Bahasa Indonesia: Selamat pagi"),
        ("th", "ภาษาไทย สวัสดีครับ กำลัง ทดสอบน้ำเสียง"),
        ("ms", "Bahasa Melayu: Selamat datang"),
        ("ar", "العَرَبِيَّة السَّلَامُ عَلَيْكُمْ ٠١٢٣٤٥٦٧٨٩"),
        ("hi", "हिन्दी नमस्ते क्ष त्र ज्ञ ऋ ष ण ॐ ०१२३४५६७८९"),
        ("mn-Cyrl", "Монгол хэл Өө Үү Ёё"),
        ("mn-Mong", "ᠮᠣᠩᠭᠣᠯ ᠪᠢᠴᠢᠭ ᠰᠠᠶᠢᠨ"),
        ("km", "ភាសាខ្មែរ សួស្តី អរគុណ ខ្ញុំ"),
        ("nb", "Norsk bokmål: Ææ Øø Åå"),
        ("nn", "Norsk nynorsk: Ææ Øø Åå"),
        ("fa", "فارسی سلام پ چ ژ گ ی ک ۰۱۲۳۴۵۶۷۸۹"),
        ("sv", "Svenska: Åå Ää Öö"),
        ("pl", "Polski: Ąą Ćć Ęę Łł Ńń Óó Śś Źź Żż"),
        ("nl", "Nederlands: België één geïnstalleerd"),
        ("uk", "Українська мова Ґґ Єє Іі Її"),
        ("uz", "O‘zbekcha oʻzbek Ўў Ққ Ғғ Ҳҳ"),
    ];

    #[test]
    #[cfg(windows)]
    #[ignore = "requires optional Windows language fonts; run explicitly on the release validation image"]
    fn installed_windows_fonts_cover_every_configured_language() {
        let configured: Vec<_> = sightocr::config::LANGUAGES
            .iter()
            .map(|(id, _)| *id)
            .collect();
        let sampled: Vec<_> = LANGUAGE_SAMPLES.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            configured, sampled,
            "Add native-script coverage samples for new languages"
        );
        let fonts = egui::epaint::text::Fonts::new(1.5, 4096, definitions(&windows_fonts_dir()));
        let mut missing = Vec::new();
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            let font = egui::FontId::new(16.0, family);
            for (language, text) in LANGUAGE_SAMPLES {
                for character in text.chars().filter(|c| !c.is_whitespace()) {
                    if !fonts.has_glyph(&font, character) {
                        missing.push(format!(
                            "{language}/{:?}: {character} U+{:04X}",
                            font.family, character as u32
                        ));
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "Missing glyphs:\n{}",
            missing.join("\n")
        );
    }

    #[test]
    #[cfg(windows)]
    #[ignore = "requires optional Windows language fonts; run explicitly on the release validation image"]
    fn installed_windows_fonts_cover_additional_international_scripts() {
        // Automatic OCR can return scripts beyond the translation language menu.
        // These samples exercise the system fallback families as well, in source,
        // translated, and monospace table text. Glyph availability is distinct
        // from contextual shaping/BiDi, which the native result editor handles.
        let samples = [
            ("Hebrew", "עברית שלום עולם"),
            ("Greek", "Ελληνικά Καλημέρα κόσμε"),
            ("Armenian", "Հայերեն բարեւ աշխարհ"),
            ("Georgian", "ქართული გამარჯობა"),
            ("Bengali", "বাংলা নমস্কার"),
            ("Punjabi", "ਪੰਜਾਬੀ ਸਤਿ ਸ੍ਰੀ ਅਕਾਲ"),
            ("Gujarati", "ગુજરાતી નમસ્તે"),
            ("Odia", "ଓଡ଼ିଆ ନମସ୍କାର"),
            ("Tamil", "தமிழ் வணக்கம்"),
            ("Telugu", "తెలుగు నమస్కారం"),
            ("Kannada", "ಕನ್ನಡ ನಮಸ್ಕಾರ"),
            ("Malayalam", "മലയാളം നമസ്കാരം"),
            ("Sinhala", "සිංහල ආයුබෝවන්"),
            ("Lao", "ພາສາລາວ ສະບາຍດີ"),
            ("Myanmar", "မြန်မာစာ မင်္ဂလာပါ"),
            ("Tibetan", "བོད་ཡིག བཀྲ་ཤིས་བདེ་ལེགས"),
            ("Amharic", "አማርኛ ሰላም"),
            ("Tifinagh", "ⵜⴰⵎⴰⵣⵉⵖⵜ"),
            ("Cherokee", "ᏣᎳᎩ ᎣᏏᏲ"),
            ("Inuktitut", "ᐃᓄᒃᑎᑐᑦ"),
            ("Javanese", "ꦧꦱꦗꦮ"),
            ("Yi", "ꆈꌠꉙ"),
            ("New Tai Lue", "ᦅᧄᦺᦑᦟᦹᧉ"),
            ("Tai Le", "ᥖᥭᥰᥘᥫᥴ"),
            ("Phags-pa", "ꡂꡜꡞꡋꡖꡟꡚ"),
            ("Math", "∑∫∂√∞∈∉⊂⊆∀∃≠≈≤≥ ℝ ℤ ℚ"),
        ];
        let fonts = egui::epaint::text::Fonts::new(1.5, 4096, definitions(&windows_fonts_dir()));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            let font = egui::FontId::new(16.0, family);
            for (script, text) in samples {
                for character in text.chars().filter(|c| !c.is_whitespace()) {
                    assert!(
                        fonts.has_glyph(&font, character),
                        "Missing {script} {character:?} (U+{:04X}) in {:?}",
                        character as u32,
                        font.family,
                    );
                }
            }
        }
    }

    #[test]
    fn missing_system_fonts_preserve_egui_defaults() {
        let empty = tempfile::tempdir().unwrap();
        let fonts = definitions(empty.path());
        let defaults = FontDefinitions::default();
        assert_eq!(fonts.families, defaults.families);
        assert_eq!(fonts.font_data.len(), defaults.font_data.len());
    }

    #[test]
    #[cfg(windows)]
    fn installed_windows_fonts_render_korean_in_both_result_families() {
        let definitions = definitions(&windows_fonts_dir());
        assert!(
            definitions.font_data.contains_key("korean"),
            "The Windows Korean font (Malgun Gothic, Gulim or Batang) is required"
        );
        let fonts = egui::epaint::text::Fonts::new(1.5, 4096, definitions);
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            let font = egui::FontId::new(16.0, family);
            // Include the reported greeting, further syllables, composed Jamo,
            // compatibility Jamo, and mixed Chinese/English/Japanese OCR output.
            for character in "안녕하세요감사합니다안녕히주무세요한국어힣한ㄱㅏ你好嗎原文繁體中文ABCabc123日本語こんにちはカタカナ".chars() {
                assert!(
                    fonts.has_glyph(&font, character),
                    "Missing {character:?} (U+{:04X}) in {:?}",
                    character as u32,
                    font.family,
                );
            }
        }
    }
}
