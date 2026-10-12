use serde::Deserialize;

#[derive(Clone, Deserialize)]
pub struct ScoringConfig {
    pub signals: Signals,
    pub chains: std::collections::HashMap<String, Vec<String>>,
}

#[derive(Clone, Deserialize)]
pub struct Signals {
    pub no_website: i16,
    pub brand_affiliated: i16,
    pub no_https: i16,
    pub not_mobile_friendly: i16,
    pub no_analytics: i16,
    pub old_copyright: i16,
    pub no_online_booking: i16,
    pub no_contact_form: i16,
    pub no_chat_widget: i16,
}

pub fn config() -> ScoringConfig {
    serde_json::from_str(include_str!("../scoring.json")).expect("scoring.json must be valid")
}

pub fn whole_word_contains(text: &str, phrase: &str) -> bool {
    let text = text.to_lowercase();
    let phrase = phrase.to_lowercase();
    text.match_indices(&phrase).any(|(start, _)| {
        let end = start + phrase.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric());
        !word(before) && !word(after)
    })
}

pub fn is_brand_affiliated(
    category: &str,
    name: &str,
    tags: &std::collections::HashMap<String, String>,
    cfg: &ScoringConfig,
) -> bool {
    let has_value = |key: &str| tags.get(key).is_some_and(|v| !v.trim().is_empty());
    if has_value("brand") || has_value("brand:wikidata") || has_value("operator") {
        return true;
    }
    if cfg
        .chains
        .get(category)
        .is_some_and(|list| list.iter().any(|brand| whole_word_contains(name, brand)))
    {
        return true;
    }
    if category == "Auto repair" {
        let authorised =
            whole_word_contains(name, "authorised") || whole_word_contains(name, "authorized");
        let service_centre = whole_word_contains(name, "service centre")
            || whole_word_contains(name, "service center");
        let brands = cfg.chains.get(category).cloned().unwrap_or_default();
        let oem = brands.iter().any(|brand| whole_word_contains(name, brand));
        return authorised || (oem && service_centre);
    }
    false
}

pub fn score(reasons: Vec<(String, i16)>) -> (u8, Vec<serde_json::Value>) {
    let raw = reasons
        .iter()
        .map(|(_, points)| i32::from(*points))
        .sum::<i32>();
    let score = raw.clamp(0, 100) as u8;
    let reasons = reasons
        .into_iter()
        .map(|(label, points)| serde_json::json!({"label": label, "points": points}))
        .collect();
    (score, reasons)
}

pub fn listing_score(
    website_listed: bool,
    brand_affiliated: bool,
    cfg: &ScoringConfig,
) -> (u8, Vec<serde_json::Value>) {
    let mut reasons = Vec::new();
    if !website_listed {
        reasons.push((
            "No website listed in OpenStreetMap (unverified)".to_string(),
            cfg.signals.no_website,
        ));
    }
    if brand_affiliated {
        reasons.push((
            "Brand-authorised or chain location".to_string(),
            cfg.signals.brand_affiliated,
        ));
    }
    score(reasons)
}

pub fn ordering_key(
    score: u8,
    has_phone: bool,
    name: &str,
) -> (std::cmp::Reverse<u8>, std::cmp::Reverse<bool>, String) {
    (
        std::cmp::Reverse(score),
        std::cmp::Reverse(has_phone),
        name.to_lowercase(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn no_website_reason_uses_configured_weight() {
        let cfg = config();
        let (score, reasons) = listing_score(false, false, &cfg);
        assert_eq!(score, 40);
        assert_eq!(
            reasons[0]["label"],
            "No website listed in OpenStreetMap (unverified)"
        );
        assert_eq!(reasons[0]["points"], 40);
    }

    #[test]
    fn detects_chain_and_oem_but_not_substrings() {
        let cfg = config();
        assert!(is_brand_affiliated(
            "Auto repair",
            "Maruti Suzuki Service Centre",
            &HashMap::new(),
            &cfg
        ));
        assert!(is_brand_affiliated(
            "Auto repair",
            "Independent authorised garage",
            &HashMap::new(),
            &cfg
        ));
        assert!(!is_brand_affiliated(
            "Auto repair",
            "Marutis Garage",
            &HashMap::new(),
            &cfg
        ));
        assert!(!is_brand_affiliated(
            "Auto repair",
            "Shelvi Automobile & Electricals",
            &HashMap::new(),
            &cfg
        ));
        assert!(is_brand_affiliated(
            "Auto repair",
            "Ekna",
            &HashMap::from([("brand".into(), "Bosch".into())]),
            &cfg
        ));
    }

    #[test]
    fn clamps_score_and_keeps_raw_reason_points() {
        assert_eq!(score(vec![("a".into(), 90), ("b".into(), 30)]).0, 100);
        assert_eq!(score(vec![("a".into(), 20), ("b".into(), -30)]).0, 0);
    }

    #[test]
    fn chain_penalty_is_applied_to_no_website_score() {
        let (score, reasons) = listing_score(false, true, &config());
        assert_eq!(score, 10);
        assert_eq!(
            reasons
                .iter()
                .map(|r| r["points"].as_i64().unwrap())
                .sum::<i64>(),
            i64::from(score)
        );
    }

    #[test]
    fn sorts_score_then_phone() {
        let mut rows = [(30, false, "a"), (30, true, "b"), (40, false, "c")];
        rows.sort_by_key(|(score, phone, name)| ordering_key(*score, *phone, name));
        assert_eq!(rows.map(|row| row.2), ["c", "b", "a"]);
    }
}
