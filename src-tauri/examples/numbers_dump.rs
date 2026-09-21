//! Prints this implementation's number words for every key in a fixture, one
//! `section<TAB>key<TAB>value` line each, so the exhaustive parity check can diff it against
//! the reference without either side needing to know how the other is implemented.
//!
//!     cargo run --quiet --example numbers_dump -- /tmp/numbers_exhaustive.json

fn main() {
    let path = match std::env::args().nth(1) {
        Some(path) => path,
        None => {
            eprintln!("usage: numbers_dump <fixture.json>");
            std::process::exit(2);
        }
    };
    let raw = std::fs::read_to_string(&path).expect("read the fixture");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse the fixture");

    /// One section of the fixture: the name it is stored under, and how to render it.
    type Section = (&'static str, fn(u64) -> String);
    let sections: [Section; 3] = [
        ("cardinal", kiegen_lib::numbers::cardinal),
        ("ordinal", kiegen_lib::numbers::ordinal),
        ("year", kiegen_lib::numbers::year),
    ];
    for (section, render) in sections {
        for key in parsed[section].as_object().expect("section").keys() {
            let n: u64 = key.parse().expect("numeric key");
            println!("{section}\t{key}\t{}", render(n));
        }
    }

    for key in parsed["float"].as_object().expect("float section").keys() {
        let (whole, fraction) = key.split_once('.').expect("a decimal key");
        let whole: u64 = whole.parse().expect("whole part");
        println!(
            "float\t{key}\t{}",
            kiegen_lib::numbers::decimal(whole, fraction)
        );
    }
}
