//! Language code normalisation.
//!
//! Jellyfin reports ISO 639-2/B codes (`dut`, `eng`), subtitle providers use
//! ISO 639-1 (`nl`, `en`) and users type whatever they like. Everything is
//! normalised to a lowercase ISO 639-1 code where one exists.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Lang(String);

/// (639-1, 639-2/B, 639-2/T, English name)
const TABLE: &[(&str, &str, &str, &str)] = &[
    ("nl", "dut", "nld", "dutch"),
    ("en", "eng", "eng", "english"),
    ("de", "ger", "deu", "german"),
    ("fr", "fre", "fra", "french"),
    ("es", "spa", "spa", "spanish"),
    ("it", "ita", "ita", "italian"),
    ("pt", "por", "por", "portuguese"),
    ("sv", "swe", "swe", "swedish"),
    ("da", "dan", "dan", "danish"),
    ("no", "nor", "nor", "norwegian"),
    ("fi", "fin", "fin", "finnish"),
    ("pl", "pol", "pol", "polish"),
    ("ru", "rus", "rus", "russian"),
    ("tr", "tur", "tur", "turkish"),
    ("ar", "ara", "ara", "arabic"),
    ("ja", "jpn", "jpn", "japanese"),
    ("ko", "kor", "kor", "korean"),
    ("zh", "chi", "zho", "chinese"),
    ("hi", "hin", "hin", "hindi"),
];

impl Lang {
    /// Parses any common spelling of a language. Returns `None` for empty,
    /// `und` (undetermined) and unknown-length garbage.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        // Strip region suffixes: "nl-BE", "pt_BR".
        let base = s.split(['-', '_']).next().unwrap_or("");
        if base.is_empty() || base == "und" || base == "mul" || base == "zxx" {
            return None;
        }
        for &(iso1, b, t, name) in TABLE {
            if base == iso1 || base == b || base == t || base == name {
                return Some(Self(iso1.to_owned()));
            }
        }
        // Unknown but plausible codes pass through unchanged.
        (base.len() == 2 || base.len() == 3).then(|| Self(base.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// English name for display ("Dutch"), or the upper-cased code.
    pub fn name(&self) -> String {
        match TABLE.iter().find(|row| row.0 == self.0) {
            Some(row) => {
                let mut n = row.3.to_owned();
                n[..1].make_ascii_uppercase();
                n
            }
            None => self.0.to_uppercase(),
        }
    }

    /// ISO 639-2/B code, as Jellyfin's subtitle upload and remote search expect.
    pub fn iso639_2b(&self) -> &str {
        TABLE
            .iter()
            .find(|row| row.0 == self.0)
            .map_or(self.0.as_str(), |row| row.1)
    }
}

impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_spellings() {
        for s in ["nl", "NL", "dut", "nld", "Dutch", "nl-BE", "nl_NL"] {
            assert_eq!(Lang::parse(s).unwrap().as_str(), "nl", "{s}");
        }
        assert_eq!(Lang::parse("eng").unwrap().as_str(), "en");
    }

    #[test]
    fn rejects_undetermined() {
        assert_eq!(Lang::parse(""), None);
        assert_eq!(Lang::parse("und"), None);
        assert_eq!(Lang::parse("toolongcode"), None);
    }

    #[test]
    fn display_names() {
        assert_eq!(Lang::parse("dut").unwrap().name(), "Dutch");
        assert_eq!(Lang::parse("xx").unwrap().name(), "XX");
    }

    #[test]
    fn round_trips_to_639_2b() {
        assert_eq!(Lang::parse("nl").unwrap().iso639_2b(), "dut");
        assert_eq!(Lang::parse("xx").unwrap().iso639_2b(), "xx");
    }
}
