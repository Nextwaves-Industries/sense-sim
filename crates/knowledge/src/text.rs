//! Tokenizer and query expansion, tuned for engineering text: part numbers and
//! standards stay whole ("802.15.4z"), compounds match both ways ("DS-TWR",
//! "ds twr", "dstwr") and Vietnamese matches with or without diacritics.

/// Lowercases and strips Vietnamese / Latin diacritics.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            'à' | 'á' | 'ạ' | 'ả' | 'ã' | 'â' | 'ầ' | 'ấ' | 'ậ' | 'ẩ' | 'ẫ' | 'ă' | 'ằ' | 'ắ' | 'ặ' | 'ẳ' | 'ẵ' | 'ä' => out.push('a'),
            'è' | 'é' | 'ẹ' | 'ẻ' | 'ẽ' | 'ê' | 'ề' | 'ế' | 'ệ' | 'ể' | 'ễ' | 'ë' => out.push('e'),
            'ì' | 'í' | 'ị' | 'ỉ' | 'ĩ' | 'ï' => out.push('i'),
            'ò' | 'ó' | 'ọ' | 'ỏ' | 'õ' | 'ô' | 'ồ' | 'ố' | 'ộ' | 'ổ' | 'ỗ' | 'ơ' | 'ờ' | 'ớ' | 'ợ' | 'ở' | 'ỡ' | 'ö' => out.push('o'),
            'ù' | 'ú' | 'ụ' | 'ủ' | 'ũ' | 'ư' | 'ừ' | 'ứ' | 'ự' | 'ử' | 'ữ' | 'ü' => out.push('u'),
            'ỳ' | 'ý' | 'ỵ' | 'ỷ' | 'ỹ' => out.push('y'),
            'đ' => out.push('d'),
            // Combining marks of decomposed input.
            '\u{300}'..='\u{36f}' => {}
            c => out.push(c),
        }
    }
    out
}

const STOP: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for", "from", "how", "i", "if", "in", "into", "is", "it", "its", "me", "my", "of", "on", "or", "should", "so", "than", "that", "the",
    "their", "then", "there", "these", "this", "to", "us", "was", "we", "what", "when", "where", "which", "why", "will", "with", "would", "you", "your", // Vietnamese function words.
    "va", "cua", "cho", "cac", "mot", "nhung", "la", "voi", "trong", "nao", "the", "gi", "co", "khong", "nhu", "bao", "nhieu",
];

fn stem(w: &str) -> String {
    if !w.is_ascii() || w.len() < 4 || w.bytes().any(|b| b.is_ascii_digit()) {
        return w.to_string();
    }
    if let Some(s) = w.strip_suffix("ies") {
        return format!("{s}y");
    }
    if w.len() > 6
        && let Some(s) = w.strip_suffix("ing")
    {
        return s.to_string();
    }
    if w.len() > 5
        && let Some(s) = w.strip_suffix("ed")
    {
        return s.to_string();
    }
    if w.ends_with('s') && !w.ends_with("ss") && !w.ends_with("us") && !w.ends_with("is") {
        return w[..w.len() - 1].to_string();
    }
    w.to_string()
}

/// Splits into raw words, keeping dots between digits and the compound joiners.
fn words(text: &str) -> Vec<String> {
    let folded = fold(text);
    let cs: Vec<char> = folded.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    for (i, &c) in cs.iter().enumerate() {
        let digit_dot = c == '.' && i > 0 && cs[i - 1].is_ascii_digit() && cs.get(i + 1).is_some_and(char::is_ascii_digit);
        let joiner = c == '-' && !cur.is_empty() && cs.get(i + 1).is_some_and(|n| n.is_alphanumeric());
        if c.is_alphanumeric() || digit_dot || joiner {
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Index and query tokens, in order. A compound also yields its joined form,
/// and a standard with a suffix also yields the base ("802.15.4z" -> "802.15.4").
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for w in words(text) {
        if w.contains('-') {
            let parts: Vec<&str> = w.split('-').filter(|p| !p.is_empty()).collect();
            for p in &parts {
                if !STOP.contains(p) {
                    out.push(stem(p));
                }
            }
            out.push(parts.concat());
            continue;
        }
        if STOP.contains(&w.as_str()) {
            continue;
        }
        if w.contains('.') && w.ends_with(|c: char| c.is_ascii_alphabetic()) {
            out.push(w.trim_end_matches(|c: char| c.is_ascii_alphabetic()).to_string());
        }
        out.push(stem(&w));
    }
    out
}

/// Words that mean the same thing, both ways round, for engineers and in
/// Vietnamese. Each group is written in plain text and tokenized at load.
pub const SYNONYMS: &[&[&str]] = &[
    &["vswr", "return loss", "s11", "impedance match"],
    &["psd", "power spectral density", "eirp density"],
    &["mpe", "maximum permissible exposure", "rf exposure", "human exposure"],
    &["fspl", "free space path loss"],
    &["path loss", "propagation loss", "suy hao đường truyền"],
    &["nlos", "non line of sight", "blocked line of sight"],
    &["los", "line of sight"],
    &["eirp", "effective isotropic radiated power"],
    &["erp", "effective radiated power"],
    &["twr", "two way ranging"],
    &["tdoa", "time difference of arrival"],
    &["toa", "time of arrival"],
    &["aoa", "angle of arrival", "direction finding"],
    &["aod", "angle of departure"],
    &["rtt", "round trip time", "ftm", "fine timing measurement"],
    &["rssi", "received signal strength"],
    &["gdop", "dilution of precision", "dop", "anchor geometry"],
    &["cep", "circular error probable"],
    &["uwb", "ultra wideband", "ultra-wideband"],
    &["ble", "bluetooth low energy", "bluetooth"],
    &["wifi", "wi-fi", "wlan", "802.11"],
    &["rfid", "gen2", "epc", "rain", "18000-63"],
    &["rtls", "real time location", "indoor positioning", "định vị"],
    &["fdtd", "finite difference time domain", "openems"],
    &["mom", "method of moments", "nec"],
    &["pml", "perfectly matched layer", "absorbing boundary"],
    &["pifa", "planar inverted f antenna"],
    &["antenna", "antennae", "anten", "ăng ten", "ăng-ten"],
    &["gain", "độ lợi"],
    &["attenuation", "suy hao", "penetration loss", "wall loss"],
    &["frequency", "tần số"],
    &["coverage", "phủ sóng", "vùng phủ"],
    &["accuracy", "error", "sai số", "độ chính xác"],
    &["multipath", "đa đường"],
    &["polarization", "polarisation", "phân cực"],
    &["interference", "nhiễu"],
    &["regulation", "regulatory", "quy định", "emission limit", "quy chuẩn"],
    &["tag", "thẻ"],
    &["reader", "đầu đọc", "interrogator"],
    &["wall", "tường"],
    &["concrete", "bê tông"],
    &["metal", "metallic", "steel", "kim loại", "thép"],
    &["ceiling", "trần", "trần nhà"],
    &["roof", "mái", "mái nhà", "truss", "giàn mái"],
    &["column", "cột", "pillar"],
    &["aisle", "lối đi", "lane"],
    &["rack", "kệ", "kệ hàng", "shelf", "shelves", "shelving", "racking"],
    &["forklift", "xe nâng", "lift truck", "reach truck"],
    &["pallet", "pa lét", "pallet gỗ"],
    &["warehouse", "nhà kho"],
    &["factory", "nhà xưởng", "nhà máy", "plant"],
    &["pipe", "ống", "đường ống", "piping"],
    &["pump", "máy bơm", "bơm"],
    &["conveyor", "băng tải"],
];

/// One thing the query asks for: the wording used in the query first, then
/// equivalent wordings. A wording of several words only counts as a phrase, so
/// "ăng ten" never matches the English word "ten".
#[derive(Debug, Clone, PartialEq)]
pub struct Concept {
    pub alts: Vec<Vec<String>>,
}

fn find(hay: &[String], needle: &[String]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Groups query tokens into concepts, longest synonym wording first, so "suy
/// hao" counts once and brings in "attenuation".
pub fn concepts(query: &str) -> Vec<Concept> {
    let toks = tokens(query);
    let mut used = vec![false; toks.len()];
    let mut out = Vec::new();
    let mut forms: Vec<(usize, Vec<String>)> = Vec::new();
    for (g, group) in SYNONYMS.iter().enumerate() {
        for w in *group {
            forms.push((g, tokens(w)));
        }
    }
    forms.sort_by_key(|(_, t)| std::cmp::Reverse(t.len()));
    let mut seen = Vec::new();
    for (g, form) in &forms {
        let Some(at) = find(&toks, form) else { continue };
        if used[at..at + form.len()].iter().any(|u| *u) {
            continue;
        }
        used[at..at + form.len()].iter_mut().for_each(|u| *u = true);
        if seen.contains(g) {
            continue;
        }
        seen.push(*g);
        let mut alts = vec![form.clone()];
        for w in SYNONYMS[*g] {
            let alt = tokens(w);
            if !alt.is_empty() && !alts.contains(&alt) {
                alts.push(alt);
            }
        }
        out.push(Concept { alts });
    }
    for (k, t) in toks.iter().enumerate() {
        if !used[k] && !out.iter().any(|c: &Concept| c.alts[0].len() == 1 && c.alts[0][0] == *t) {
            out.push(Concept { alts: vec![vec![t.clone()]] });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_standards_and_splits_compounds() {
        let t = tokens("IEEE 802.15.4z DS-TWR at 6.5 GHz, -41.3 dBm/MHz");
        for want in ["802.15.4z", "802.15.4", "ds", "twr", "dstwr", "6.5", "ghz", "41.3", "dbm", "mhz"] {
            assert!(t.iter().any(|x| x == want), "{want} missing from {t:?}");
        }
    }

    #[test]
    fn plural_and_diacritics_fold() {
        assert_eq!(tokens("antennas"), tokens("antenna"));
        assert_eq!(tokens("Mái nhà"), tokens("mai nha"));
        assert_eq!(fold("đầu đọc"), "dau doc");
        // Decomposed input (common from some keyboards) folds the same way.
        assert_eq!(fold("ma\u{301}i"), "mai");
    }

    #[test]
    fn vietnamese_phrase_expands_to_english_as_one_concept() {
        let c = concepts("suy hao bê tông");
        assert_eq!(c.len(), 2, "{c:?}");
        let has = |w: &str| c.iter().any(|c| c.alts.iter().any(|a| a.len() == 1 && a[0] == w));
        assert!(has("attenuation"));
        assert!(has("concrete"));
    }

    #[test]
    fn plain_words_stay_single_concepts() {
        let c = concepts("clock drift budget");
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].alts, vec![vec!["clock"]]);
    }
}
