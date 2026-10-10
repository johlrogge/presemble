//! The indefinite article for a name in a message.

/// `name` behind the right indefinite article: "an item", "a list", "an HTML
/// block".
///
/// The article follows the sound, not the letter, for the names messages use:
/// a vowel letter takes "an" (but "a user", "a unit", "a one"), and so does a
/// name that starts with the letters "html" or "hour" ("an html-block"), which
/// are read out letter by letter or with a silent `h`. An empty name gets "a".
pub fn with_article(name: &str) -> String {
    format!("{} {name}", article_for(name))
}

fn article_for(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    let starts = |prefix: &str| lower.starts_with(prefix);
    let vowel = lower.starts_with(['a', 'e', 'i', 'o', 'u']);
    // Vowel letters read as a consonant: "you", "won".
    let consonant_sound = starts("uni")
        || starts("use")
        || starts("usa")
        || starts("ur")
        || starts("one")
        || starts("eu");
    // Consonant letters read as a vowel: letter-by-letter "aitch", silent `h`.
    let vowel_sound = starts("html") || starts("hour") || starts("honest");
    if (vowel && !consonant_sound) || vowel_sound {
        "an"
    } else {
        "a"
    }
}

#[cfg(test)]
mod tests {
    use super::with_article;

    #[test]
    fn the_article_follows_the_sound() {
        for (name, expected) in [
            ("item", "an item"),
            ("list", "a list"),
            ("html-block", "an html-block"),
            ("HTML block", "an HTML block"),
            ("paragraph", "a paragraph"),
            ("integer", "an integer"),
            ("unassigned", "an unassigned"),
            ("unit", "a unit"),
            ("user", "a user"),
            ("record", "a record"),
            ("", "a "),
        ] {
            assert_eq!(with_article(name), expected, "{name:?}");
        }
    }
}
