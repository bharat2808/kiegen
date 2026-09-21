//! Number to words, matching `num2words` (English) as closely as a fixture can tell.
//!
//! This is not cosmetic. misaki normalizes numbers through `num2words`, and the shape of
//! the words changes the phonemes Kokoro receives: `21` is "twenty-one" (one word, one
//! stress pattern), `3.5` is "three point five", and `1990` is "nineteen ninety" rather
//! than "one thousand nine hundred and ninety". Getting these wrong does not sound slightly
//! off; it produces a different sentence.
//!
//! Every rule below was derived from the reference output rather than from intuition — see
//! `docs/DESIGN.md`, and the fixtures under `src-tauri/tests/fixtures/`.

const UNITS: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];

const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

const ORDINAL_UNITS: [&str; 20] = [
    "zeroth",
    "first",
    "second",
    "third",
    "fourth",
    "fifth",
    "sixth",
    "seventh",
    "eighth",
    "ninth",
    "tenth",
    "eleventh",
    "twelfth",
    "thirteenth",
    "fourteenth",
    "fifteenth",
    "sixteenth",
    "seventeenth",
    "eighteenth",
    "nineteenth",
];

const ORDINAL_TENS: [&str; 10] = [
    "",
    "",
    "twentieth",
    "thirtieth",
    "fortieth",
    "fiftieth",
    "sixtieth",
    "seventieth",
    "eightieth",
    "ninetieth",
];

/// Scale names, largest first, so the first match is the highest group.
const SCALES: [(u64, &str); 5] = [
    (1_000_000_000_000, "trillion"),
    (1_000_000_000, "billion"),
    (1_000_000, "million"),
    (1_000, "thousand"),
    (100, "hundred"),
];

/// 0..=999.
fn under_thousand(n: u64) -> String {
    if n < 20 {
        return UNITS[n as usize].to_string();
    }
    if n < 100 {
        let (tens, rest) = (n / 10, n % 10);
        return if rest == 0 {
            TENS[tens as usize].to_string()
        } else {
            // "twenty-one": hyphenated, so it is one word to the phonemiser.
            format!("{}-{}", TENS[tens as usize], UNITS[rest as usize])
        };
    }
    let (hundreds, rest) = (n / 100, n % 100);
    if rest == 0 {
        format!("{} hundred", UNITS[hundreds as usize])
    } else {
        format!(
            "{} hundred and {}",
            UNITS[hundreds as usize],
            under_thousand(rest)
        )
    }
}

pub fn cardinal(n: u64) -> String {
    if n < 1000 {
        return under_thousand(n);
    }
    for (value, name) in SCALES.iter().take(4) {
        if n >= *value {
            let (quotient, rest) = (n / value, n % value);
            let head = format!("{} {name}", cardinal(quotient));
            return if rest == 0 {
                head
            } else if rest < 100 {
                // "one thousand and one", but "one thousand, one hundred".
                format!("{head} and {}", cardinal(rest))
            } else {
                format!("{head}, {}", cardinal(rest))
            };
        }
    }
    unreachable!("every value >= 1000 falls into a scale group")
}

pub fn ordinal(n: u64) -> String {
    if n < 20 {
        return ORDINAL_UNITS[n as usize].to_string();
    }
    if n < 100 {
        let (tens, rest) = (n / 10, n % 10);
        return if rest == 0 {
            ORDINAL_TENS[tens as usize].to_string()
        } else {
            format!("{}-{}", TENS[tens as usize], ORDINAL_UNITS[rest as usize])
        };
    }
    if n < 1000 {
        let (hundreds, rest) = (n / 100, n % 100);
        return if rest == 0 {
            format!("{} hundredth", UNITS[hundreds as usize])
        } else {
            format!("{} hundred and {}", UNITS[hundreds as usize], ordinal(rest))
        };
    }
    for (value, name) in SCALES.iter().take(4) {
        if n >= *value {
            let (quotient, rest) = (n / value, n % value);
            let head = format!("{} {name}", cardinal(quotient));
            return if rest == 0 {
                // "one thousandth", "one millionth".
                format!("{head}th")
            } else if rest < 100 {
                format!("{head} and {}", ordinal(rest))
            } else {
                format!("{head}, {}", ordinal(rest))
            };
        }
    }
    unreachable!("every value >= 1000 falls into a scale group")
}

/// Years read as pairs: 1990 is "nineteen ninety", not "one thousand nine hundred and
/// ninety". The boundaries were pinned against the reference over 1000..=2100, because
/// they are not guessable — the first decade of a millennium is spoken differently from the
/// rest of it ("one thousand and one" but "nineteen oh-one").
pub fn year(n: u64) -> String {
    if !(1000..10000).contains(&n) {
        return cardinal(n);
    }
    let (high, low) = (n / 100, n % 100);
    // 1000 and 2000 are "one thousand" and "two thousand", not "ten hundred".
    if n % 1000 == 0 {
        return cardinal(n);
    }
    if low == 0 {
        // 1100 is "eleven hundred", 1900 is "nineteen hundred".
        return format!("{} hundred", cardinal(high));
    }
    if (1..10).contains(&low) {
        return match high {
            // The millennium itself is named, then "and".
            10 | 20 => format!("{} and {}", cardinal(high * 100), cardinal(low)),
            // Every other century says the zero: "nineteen oh-eight".
            _ => format!("{} oh-{}", cardinal(high), cardinal(low)),
        };
    }
    format!("{} {}", cardinal(high), cardinal(low))
}

/// `3.14159` -> "three point one four one five nine": digits after the point are said one at
/// a time, which is what the reference does and what a listener expects for a decimal.
///
/// A fraction that is only zeros is not spoken at all — the reference turns 12.0 into
/// "twelve", so `12.0` must not become "twelve point zero".
pub fn decimal(whole: u64, fraction_digits: &str) -> String {
    // Trailing zeros are not spoken: 12.0 is "twelve" and 1.50 is "one point five".
    let trimmed = fraction_digits.trim_end_matches('0');
    let digits: Vec<&str> = trimmed
        .chars()
        .filter(|c| c.is_ascii_digit())
        .map(|c| UNITS[c as usize - '0' as usize])
        .collect();
    if digits.is_empty() {
        return cardinal(whole);
    }
    format!("{} point {}", cardinal(whole), digits.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cardinals_use_and_commas_where_the_reference_does() {
        assert_eq!(cardinal(0), "zero");
        assert_eq!(cardinal(15), "fifteen");
        assert_eq!(cardinal(21), "twenty-one");
        assert_eq!(cardinal(100), "one hundred");
        assert_eq!(cardinal(101), "one hundred and one");
        assert_eq!(cardinal(123), "one hundred and twenty-three");
        assert_eq!(cardinal(1000), "one thousand");
        assert_eq!(cardinal(1001), "one thousand and one");
        // No "and" before a hundreds group, a comma instead: "one thousand, one hundred".
        assert_eq!(cardinal(1100), "one thousand, one hundred");
        assert_eq!(cardinal(1234), "one thousand, two hundred and thirty-four");
        assert_eq!(
            cardinal(1234567),
            "one million, two hundred and thirty-four thousand, five hundred and sixty-seven"
        );
        assert_eq!(cardinal(1000000000), "one billion");
    }

    #[test]
    fn ordinals_irregular_where_english_is_irregular() {
        assert_eq!(ordinal(1), "first");
        assert_eq!(ordinal(2), "second");
        assert_eq!(ordinal(3), "third");
        assert_eq!(ordinal(11), "eleventh");
        assert_eq!(ordinal(21), "twenty-first");
        assert_eq!(ordinal(100), "one hundredth");
        assert_eq!(ordinal(101), "one hundred and first");
        assert_eq!(ordinal(1000), "one thousandth");
        assert_eq!(ordinal(1000000), "one millionth");
    }

    #[test]
    fn years_read_as_pairs_not_powers() {
        assert_eq!(year(1900), "nineteen hundred");
        assert_eq!(year(1990), "nineteen ninety");
        assert_eq!(year(2024), "twenty twenty-four");
        assert_eq!(year(2000), "two thousand");
        assert_eq!(year(2005), "two thousand and five");
        assert_eq!(year(1908), "nineteen oh-eight");
        assert_eq!(year(1001), "one thousand and one");
        assert_eq!(year(1100), "eleven hundred");
    }

    #[test]
    fn decimals_are_said_digit_by_digit() {
        assert_eq!(decimal(3, "14159"), "three point one four one five nine");
        assert_eq!(decimal(0, "5"), "zero point five");
        assert_eq!(decimal(100, "5"), "one hundred point five");
    }

    /// The whole committed fixture, so a rule change cannot quietly break a shape that only
    /// appears for one value.
    #[test]
    fn the_committed_fixture_matches() {
        let raw = include_str!("../tests/fixtures/numbers_fixture.json");
        let parsed: serde_json::Value = serde_json::from_str(raw).expect("fixture parses");

        let mut checked = 0usize;
        for (section, f) in [
            ("cardinal", cardinal as fn(u64) -> String),
            ("ordinal", ordinal as fn(u64) -> String),
            ("year", year as fn(u64) -> String),
        ] {
            for (key, expected) in parsed[section].as_object().expect("section") {
                let n: u64 = key.parse().expect("numeric key");
                let expected = expected.as_str().expect("string value");
                assert_eq!(f(n), expected, "{section}({n}) diverged from the reference");
                checked += 1;
            }
        }
        for (key, expected) in parsed["float"].as_object().expect("float section") {
            let (whole, fraction) = key.split_once('.').expect("a decimal key");
            let whole: u64 = whole.parse().expect("whole part");
            assert_eq!(
                decimal(whole, fraction),
                expected.as_str().expect("string value"),
                "decimal({key}) diverged from the reference"
            );
            checked += 1;
        }
        assert!(checked > 100, "the fixture should not shrink unnoticed");
    }
}
