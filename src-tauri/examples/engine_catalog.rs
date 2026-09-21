//! Prints the engine catalogue as JSON, byte-for-byte what the settings UI receives.
//!
//! `scripts/preview.sh` feeds this into the browser harness so the preview shows the real
//! catalogue instead of a hand-written copy that would drift the moment an engine or a
//! voice changed. Run it directly to see what the UI will be told:
//!
//!     cargo run --quiet --example engine_catalog | python3 -m json.tool

fn main() {
    let settings = kiegen_lib::config::Settings::default();
    let catalog = kiegen_lib::engines::catalog(&settings);
    let json = serde_json::to_string(&catalog).expect("catalogue must serialize");
    println!("{json}");
}
