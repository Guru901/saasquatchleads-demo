/// Nominatim special phrases used for bounded POI searches.
/// If the public instance does not recognize a phrase, runtime discovery retries
/// with the category and geocoded place name.
pub fn keyword(category: &str) -> Option<&'static str> {
    match category {
        "Dental clinics" => Some("dentist"),
        "Restaurants" => Some("restaurant"),
        "Gyms" => Some("fitness centre"),
        "Auto repair" => Some("car repair"),
        "Salons" => Some("hairdresser"),
        "Pharmacies" => Some("pharmacy"),
        "Veterinary clinics" => Some("veterinary"),
        "Hotels" => Some("hotel"),
        "Real estate agencies" => Some("estate agent"),
        "Grocery stores" => Some("supermarket"),
        "Clothing stores" => Some("clothes"),
        "Law firms" => Some("lawyer"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::keyword;

    #[test]
    fn all_supported_categories_have_a_bounded_search_phrase() {
        for (category, expected) in [
            ("Dental clinics", "dentist"),
            ("Restaurants", "restaurant"),
            ("Gyms", "fitness centre"),
            ("Auto repair", "car repair"),
            ("Salons", "hairdresser"),
            ("Pharmacies", "pharmacy"),
            ("Veterinary clinics", "veterinary"),
            ("Hotels", "hotel"),
            ("Real estate agencies", "estate agent"),
            ("Grocery stores", "supermarket"),
            ("Clothing stores", "clothes"),
            ("Law firms", "lawyer"),
        ] {
            assert_eq!(keyword(category), Some(expected));
        }
        assert_eq!(keyword("Software engineering companies"), None);
    }
}
