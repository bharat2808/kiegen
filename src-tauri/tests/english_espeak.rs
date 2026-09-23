use kiegen_lib::{espeak::EspeakNg, g2p::G2p};

fn dictionary() -> G2p {
    G2p::load(
        r#"{"hello":"həlˈO","K":"kˈA","I":"ˈI","E":"ˈi","G":"ʤˈi","N":"ˈɛn"}"#,
        "{}",
    )
    .unwrap()
}

#[test]
fn missing_cli_keeps_words_audible_and_dictionary_unchanged() {
    let g = dictionary();
    assert_eq!(
        g.phonemize_with_espeak("hello kiegen.", None, false),
        "həlˈO kˌAˌIˌiʤˌiˌiˈɛn."
    );
    assert!(!g.phonemize("kiegen").is_empty());
}

#[test]
fn installed_cli_pronounces_unknown_words_without_changing_known_words() {
    let Some(cli) = EspeakNg::detect() else {
        eprintln!("skipping: no espeak-ng");
        return;
    };
    let g = dictionary();
    assert_eq!(
        g.phonemize_with_espeak("hello kiegen.", Some(&cli), false),
        "həlˈO kˈiʤən."
    );
    assert_eq!(
        g.phonemize_with_espeak("kiegen", Some(&cli), true),
        "kˈiːʤən"
    );
    assert_eq!(
        g.phonemize_with_espeak("KIEGEN", Some(&cli), false),
        g.phonemize("KIEGEN")
    );
}
