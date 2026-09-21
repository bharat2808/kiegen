//! Fetches one file from the Kokoro plan through the real download path, reports the
//! measured throughput, and hashes the result so it can be compared with a known-good copy.
//!
//!     cargo run --quiet --example fetch_one                          # 522 kB voice
//!     cargo run --quiet --example fetch_one -- onnx/model.onnx       # the 325 MB graph
//!
//! Exists so the downloader is exercised as the app exercises it — parallel ranges,
//! header verification, hash check, atomic rename — rather than only through unit tests.

fn main() {
    let wanted = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "voices/af_heart.bin".to_string());

    let plan = kiegen_lib::download::kokoro_plan().expect("the plan must build");
    let entry = plan
        .iter()
        .find(|entry| entry.path == wanted)
        .unwrap_or_else(|| panic!("{wanted} is not in the plan"));
    println!("url      {}", entry.url);

    let dir = std::env::temp_dir().join("kiegen-fetch-one");
    let target = dir.join(&entry.path);
    // Always start clean, so a previous run cannot make this look like a download.
    let _ = std::fs::remove_file(&target);

    let mut total = 0u64;
    let started = std::time::Instant::now();
    let mut last_report = started;

    let result = kiegen_lib::download::fetch(entry, &dir, &mut |delta| {
        total += delta;
        if last_report.elapsed().as_secs_f32() >= 1.0 {
            let elapsed = started.elapsed().as_secs_f32();
            eprintln!(
                "  {:.1} MB so far — {:.2} MB/s",
                total as f32 / 1e6,
                total as f32 / 1e6 / elapsed.max(0.001)
            );
            last_report = std::time::Instant::now();
        }
    });

    let elapsed = started.elapsed().as_secs_f32();
    if let Err(error) = result {
        eprintln!("FAILED: {error}");
        std::process::exit(1);
    }

    let hash = kiegen_lib::download::sha256_of_file(&target).expect("hash the result");
    let rate = total as f32 / 1e6 / elapsed.max(0.001);
    println!("bytes    {total} in {elapsed:.2}s ({rate:.2} MB/s)");
    println!("sha256   {hash}");

    if entry.path == "voices/af_heart.bin" {
        // The server's x-linked-etag and a locally cached copy of this file agree on this
        // value, so it is a real content hash and not just a server claim.
        let expected = "d583ccff3cdca2f7fae535cb998ac07e9fcb90f09737b9a41fa2734ec44a8f0b";
        println!("expected {expected}");
        if hash == expected {
            println!("MATCH — byte-identical to the verified copy");
        } else {
            println!("MISMATCH");
            std::process::exit(1);
        }
    }
}
