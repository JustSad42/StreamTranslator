//! Language names and NLLB codes.

pub const LANGUAGES: &[(&str, &str)] = &[
    ("ar", "Arabic"), ("bg", "Bulgarian"), ("cs", "Czech"), ("da", "Danish"), ("de", "German"),
    ("el", "Greek"), ("en", "English"), ("es", "Spanish"), ("fi", "Finnish"), ("fr", "French"),
    ("he", "Hebrew"), ("hi", "Hindi"), ("hr", "Croatian"), ("hu", "Hungarian"), ("id", "Indonesian"),
    ("it", "Italian"), ("ja", "Japanese"), ("ko", "Korean"), ("lt", "Lithuanian"), ("ms", "Malay"),
    ("nl", "Dutch"), ("no", "Norwegian"), ("pl", "Polish"), ("pt", "Portuguese"), ("ro", "Romanian"),
    ("ru", "Russian"), ("sk", "Slovak"), ("sr", "Serbian"), ("sv", "Swedish"), ("th", "Thai"),
    ("tl", "Tagalog"), ("tr", "Turkish"), ("uk", "Ukrainian"), ("vi", "Vietnamese"), ("zh", "Chinese"),
];

pub const NLLB_CODES: &[(&str, &str)] = &[
    ("ar", "arb_Arab"), ("bg", "bul_Cyrl"), ("cs", "ces_Latn"), ("da", "dan_Latn"), ("de", "deu_Latn"), ("el", "ell_Grek"),
    ("es", "spa_Latn"), ("fi", "fin_Latn"), ("fr", "fra_Latn"), ("he", "heb_Hebr"), ("hi", "hin_Deva"), ("hr", "hrv_Latn"),
    ("hu", "hun_Latn"), ("id", "ind_Latn"), ("it", "ita_Latn"), ("ja", "jpn_Jpan"), ("ko", "kor_Hang"), ("lt", "lit_Latn"),
    ("ms", "zsm_Latn"), ("nl", "nld_Latn"), ("no", "nob_Latn"), ("pl", "pol_Latn"), ("pt", "por_Latn"), ("ro", "ron_Latn"),
    ("ru", "rus_Cyrl"), ("sk", "slk_Latn"), ("sr", "srp_Cyrl"), ("sv", "swe_Latn"), ("th", "tha_Thai"), ("tl", "tgl_Latn"),
    ("tr", "tur_Latn"), ("uk", "ukr_Cyrl"), ("vi", "vie_Latn"), ("zh", "zho_Hans"),
];

/// "es" -> "Spanish"; unknown codes are shown as they are.
pub fn lang_name(code: Option<&str>) -> String {
    let c = code.unwrap_or("");
    match LANGUAGES.iter().find(|l| l.0 == c) {
        Some(l) => l.1.to_string(),
        None if c.is_empty() => "unknown".into(),
        None => c.into(),
    }
}

pub fn nllb_code(lang: &str) -> Option<&'static str> {
    NLLB_CODES.iter().find(|l| l.0 == lang).map(|l| l.1)
}
