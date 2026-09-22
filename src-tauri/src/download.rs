//! Fetching model weights from HuggingFace into the app's data directory.
//!
//! Both local engines' weights are *plain files the app owns*, and both come through here.
//! That distinction used to matter: the MLX engines kept their weights inside a HuggingFace
//! cache that their own Python runtime managed, so they had to be installed *by* that
//! runtime — hand-placing files into a cache layout we do not control is how you get a
//! "downloaded" model that the library then cannot find. That path went with Qwen3-TTS;
//! Chatterbox Multilingual is now an ONNX export the app fetches and verifies itself.
//!
//! Design points, measured rather than assumed:
//!
//! * **Parallel ranges.** A single connection from HuggingFace's CDN moved 0.42 MB/s on
//!   this machine while the multi-connection path reached 5.86 MB/s. On a 325 MB graph
//!   that is the difference between 13 minutes and one, so large files are fetched as a
//!   few range requests written straight into place.
//! * **Pinned revisions.** Files come from an explicit commit, never `main`, so an
//!   upstream change cannot silently alter what gets verified.
//! * **Verified.** HuggingFace reports the content sha256 in `x-linked-etag` for large
//!   files. That claim was checked against a locally cached copy of `voices/af_heart.bin`
//!   and the hashes matched, so it is a real content hash and not merely a server label.
//! * **Nothing partial survives.** A file lands as `<name>.part` and is renamed only once
//!   its hash checks out, so a half-downloaded graph can never look like an installed
//!   engine.

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};

/// Parallel range requests per file. See the module note for why this is not 1.
const CONNECTIONS: usize = 4;

/// Below this, connection setup costs more than the parallelism wins.
const PARALLEL_THRESHOLD: u64 = 4 * 1024 * 1024;

/// How often the caller's progress callback is fed while workers run.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

const HF_BASE: &str = "https://huggingface.co";

/// Kokoro's ONNX export, pinned to the commit these numbers were verified against.
const KOKORO_REPO: &str = "onnx-community/Kokoro-82M-v1.0-ONNX";
const KOKORO_COMMIT: &str = "1939ad2a8e416c0acfeecc08a694d14ef25f2231";

const KOKORO_GRAPH_BYTES: u64 = 325_532_232;
const KOKORO_TOKENIZER_BYTES: u64 = 3_497;

/// The G2P dictionaries, without which Kokoro cannot read a word: the engine takes
/// phonemes, and these are the tables that produce them.
///
/// Apache-2.0 string tables from `hexgrad/misaki`, pinned to a commit. Unlike the
/// HuggingFace entries these carry their hash in the plan, because the raw host reports no
/// content digest and both sizes here were confirmed against an independently fetched copy.
const LEXICON_BASE: &str = "https://raw.githubusercontent.com/hexgrad/misaki";
const LEXICON_COMMIT: &str = "fba1236595f2d2bf21d414ba6e57d25256afada3";
const LEXICON_FILES: [(&str, u64, &str); 2] = [
    (
        "lexicon/us_gold.json",
        3_000_469,
        "dc414872a49a28ae6c141463d502fd945f3b2fde040484fdc47d00cc4612686f",
    ),
    (
        "lexicon/us_silver.json",
        3_099_517,
        "de8f67be911bb6c659187b4a65fd966b6a30e56350e0f790d763210b053ac475",
    ),
];

/// Every shipped voice style table measured 522,240 bytes (510 rows x 256 float32). The
/// repo also carries `voices/af.bin` at 524,288 bytes — 512 rows, absent from Kokoro's
/// documented voice list — which is exactly why it is offered nowhere.
const VOICE_BYTES: u64 = 522_240;

/// Chatterbox Multilingual's ONNX export, pinned to the commit these sizes were read from:
/// `onnx-community/chatterbox-multilingual-ONNX`, MIT and ungated.
const CHATTERBOX_REPO: &str = "onnx-community/chatterbox-multilingual-ONNX";
const CHATTERBOX_COMMIT: &str = "452d3f434aa592098f1eedac9099f33642ab2da5";

/// The whole set, with the size the repository reports for each file, four graphs small-to-
/// large.
///
/// The layout is the ONNX exporter's: each graph is a tiny `.onnx` whose weights sit in a
/// sibling `*_onnx_data` blob, and neither half is usable alone — which is why both are
/// listed and why the pair travels together. Only the language model has quantised variants
/// (`q4f16` here); the encoder, the conditional decoder and the token embedding are fp32-only
/// in this export, so 591 MB and 534 MB are not choices to shrink.
///
/// `default_voice.wav` is the reference clip the zero-shot path falls back to, and
/// `Cangjie5_TC.json` is the Chinese character mapping the `zh` path needs (see
/// docs/DESIGN.md §5) — fetched now so `zh` is not a second download later.
const CHATTERBOX_FILES: [(&str, u64); 11] = [
    ("tokenizer.json", 71_798),
    ("Cangjie5_TC.json", 1_920_163),
    ("default_voice.wav", 714_320),
    ("onnx/embed_tokens.onnx", 13_286),
    ("onnx/embed_tokens.onnx_data", 68_390_912),
    ("onnx/language_model_q4f16.onnx", 229_388),
    ("onnx/language_model_q4f16.onnx_data", 304_737_408),
    ("onnx/conditional_decoder.onnx", 6_350_448),
    ("onnx/conditional_decoder.onnx_data", 533_970_816),
    ("onnx/speech_encoder.onnx", 1_184_608),
    ("onnx/speech_encoder.onnx_data", 591_274_880),
];

/// One file to fetch. `url` is already resolved to a pinned commit.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Repo-relative path, e.g. `onnx/model.onnx`. Also the path under the destination.
    pub path: String,
    /// Expected size, re-checked against the server before any byte is written.
    pub bytes: u64,
    pub url: String,
    /// Content hash to verify once the bytes are on disk.
    ///
    /// HuggingFace supplies one in `x-linked-etag`, which is why this was not a field
    /// before. GitHub's raw host supplies nothing, so an entry pinned only by revision would
    /// land unverified — anything fetching from there has to carry its own hash.
    pub sha256: Option<String>,
}

/// What Kokoro needs on disk: the graph, the phoneme tokenizer, and one style table per
/// *usable* voice.
///
/// Only the voices that can actually be used are fetched. The Japanese and Chinese front
/// ends are separate modules that cannot ship here, so their style tables would be 13 MB of
/// dead weight. The espeak-backed five languages are fetched **only when espeak-ng has been
/// found** — the same availability rule the catalogue shows, so installing espeak-ng adds
/// those voice tables to the next download rather than leaving them unreachable.
pub fn kokoro_plan() -> Result<Vec<Entry>, String> {
    kokoro_plan_with(crate::engine_paths::espeak_ng().is_some())
}

/// The same plan for a given espeak-ng state. Injected rather than probed so both plans are
/// assertable on any machine: whether espeak-ng is installed decides whether 13 more voice
/// tables belong in the download, and a test that could only ever observe one of those
/// states would let a regression in the other one ship.
pub fn kokoro_plan_with(espeak_ready: bool) -> Result<Vec<Entry>, String> {
    let voices: Vec<String> = crate::engines::kokoro_voices(espeak_ready)
        .into_iter()
        .filter(|voice| voice.unavailable.is_none())
        .map(|voice| format!("voices/{}.bin", voice.id))
        .collect();
    if voices.is_empty() {
        return Err("no usable Kokoro voices are defined".to_string());
    }

    // Cheap first: the 3.5 kB tokenizer, then the 0.5 MB voice tables, then 6 MB of
    // dictionaries, and the 325 MB graph last — so a failure on any of the small files
    // fails fast instead of after the expensive download.
    let mut plan: Vec<Entry> = vec![Entry {
        path: "tokenizer.json".to_string(),
        bytes: KOKORO_TOKENIZER_BYTES,
        url: format!("{HF_BASE}/{KOKORO_REPO}/resolve/{KOKORO_COMMIT}/tokenizer.json"),
        sha256: None,
    }];

    plan.extend(voices.into_iter().map(|path| Entry {
        url: format!("{HF_BASE}/{KOKORO_REPO}/resolve/{KOKORO_COMMIT}/{path}"),
        path,
        bytes: VOICE_BYTES,
        sha256: None,
    }));

    // The dictionaries: 6 MB that turn a pile of weights into an engine that can read.
    for (path, bytes, sha256) in LEXICON_FILES {
        let name = path.rsplit('/').next().unwrap_or(path);
        plan.push(Entry {
            path: path.to_string(),
            bytes,
            url: format!("{LEXICON_BASE}/{LEXICON_COMMIT}/misaki/data/{name}"),
            sha256: Some(sha256.to_string()),
        });
    }

    plan.push(Entry {
        path: "onnx/model.onnx".to_string(),
        bytes: KOKORO_GRAPH_BYTES,
        url: format!("{HF_BASE}/{KOKORO_REPO}/resolve/{KOKORO_COMMIT}/onnx/model.onnx"),
        sha256: None,
    });

    Ok(plan)
}

/// Total bytes `kokoro_plan` will fetch, derived from the plan itself so the figure the UI
/// shows cannot drift from the work actually done.
pub fn kokoro_bytes() -> u64 {
    kokoro_plan()
        .map(|plan| plan.iter().map(|entry| entry.bytes).sum())
        .unwrap_or(0)
}

/// Everything Chatterbox Multilingual needs on disk, smallest first.
///
/// No `Result`, unlike Kokoro's plan: there is no availability rule that can remove a file
/// here. The 23 languages live in one checkpoint and one tokenizer, so every entry is always
/// required — the only thing that varies is which language code the user picked, and that is
/// an argument to synthesis rather than a file to fetch.
pub fn chatterbox_plan() -> Vec<Entry> {
    CHATTERBOX_FILES
        .iter()
        .map(|(path, bytes)| Entry {
            path: (*path).to_string(),
            bytes: *bytes,
            url: format!("{HF_BASE}/{CHATTERBOX_REPO}/resolve/{CHATTERBOX_COMMIT}/{path}"),
            // HuggingFace reports a content sha256 in `x-linked-etag` for files this size,
            // and `fetch` verifies against that when the plan carries none of its own.
            sha256: None,
        })
        .collect()
}

/// Total bytes `chatterbox_plan` will fetch, derived the same way Kokoro's figure is.
pub fn chatterbox_bytes() -> u64 {
    chatterbox_plan().iter().map(|entry| entry.bytes).sum()
}

/// Splits `total` bytes into contiguous, inclusive ranges covering it exactly.
///
/// Separated out and tested because an off-by-one here is a corrupt file that only shows
/// up at the hash check — or worse, does not.
pub fn plan_ranges(total: u64, parts: usize) -> Vec<(u64, u64)> {
    if total == 0 || parts == 0 {
        return Vec::new();
    }
    let parts = parts.min(total as usize).max(1);
    let chunk = total.div_ceil(parts as u64);
    let mut ranges = Vec::with_capacity(parts);
    let mut start = 0u64;
    while start < total {
        let length = chunk.min(total - start);
        // `length - 1` because the end offset is inclusive: `bytes=0-99` is 100 bytes.
        ranges.push((start, start + length - 1));
        start += length;
    }
    ranges
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn sha256_of_reader(mut source: impl Read) -> Result<String, String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = source.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

pub fn sha256_of_file(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|e| format!("open {path:?}: {e}"))?;
    sha256_of_reader(file)
}

/// `bytes 0-0/3497` -> `Some(3497)`. Also handles `bytes 0-0/*`.
fn parse_content_range(value: &str) -> Option<u64> {
    value.rsplit('/').next()?.trim().parse::<u64>().ok()
}

/// The size the *linked file* reports. Authoritative whenever it is present.
fn linked_size(headers: &ureq::http::HeaderMap) -> Option<u64> {
    headers
        .get("x-linked-size")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

/// `Content-Length` is the file's length only on a plain 200. On a 307 it is the length of
/// the redirect's own body: this endpoint answers `content-length: 316` for
/// `tokenizer.json`, a 3,497-byte file. Treating that as the size would appear to work and
/// then produce a 316-byte "tokenizer".
fn plain_size(headers: &ureq::http::HeaderMap) -> Option<u64> {
    headers
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

fn sha_from_headers(headers: &ureq::http::HeaderMap) -> Option<String> {
    headers
        .get("x-linked-etag")
        .and_then(|value| value.to_str().ok())
        // The etag arrives quoted and is a sha256 only for large files — small ones use a
        // different scheme, so anything that is not 64 hex digits is not a hash.
        .and_then(|value| {
            let trimmed = value.trim_matches('"');
            (trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| trimmed.to_string())
        })
}

/// The size the server reports for a file, plus the sha256 it declares. Both come from
/// headers, so this costs no body bytes beyond one byte in the fallback path.
///
/// Every request asks for `identity` encoding, because a server that gzips the response
/// reports the *compressed* length in `Content-Length`: GitHub's raw host answers 737,890
/// bytes for a 3,000,469-byte dictionary, so a size check against that figure rejects a file
/// that is perfectly fine. Ranges are also meaningless over an encoded stream, and the hash
/// taken at the end must be over the bytes as stored.
pub fn probe(url: &str) -> Result<(u64, Option<String>), String> {
    if let Ok(response) = ureq::head(url).header("Accept-Encoding", "identity").call() {
        let headers = response.headers();
        if let Some(size) = linked_size(headers) {
            return Ok((size, sha_from_headers(headers)));
        }
        // Only on a 200. See `plain_size` for what a redirect's Content-Length means.
        if response.status().as_u16() == 200 {
            if let Some(size) = plain_size(headers) {
                return Ok((size, sha_from_headers(headers)));
            }
        }
    }

    // Some files carry neither header on a HEAD. A one-byte range request still reports the
    // full length in `Content-Range`, so the size check survives — and this is the only way
    // `tokenizer.json`, which the engine cannot start without, is fetched at all.
    let response = ureq::get(url)
        .header("Accept-Encoding", "identity")
        .header("Range", "bytes=0-0")
        .call()
        .map_err(|error| format!("probe {url}: {error}"))?;
    let headers = response.headers();
    let size = headers
        .get("content-range")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_content_range)
        .or_else(|| linked_size(headers))
        .ok_or_else(|| format!("no size reported for {url}"))?;
    Ok((size, sha_from_headers(headers)))
}

/// Fetches one entry into `dest_dir`, calling `on_bytes` with each increment of bytes
/// written so the caller can report aggregate progress across a whole plan.
///
/// A destination already at the right size is skipped, which is what makes retrying after
/// a failure cheap.
pub fn fetch(entry: &Entry, dest_dir: &Path, on_bytes: &mut dyn FnMut(u64)) -> Result<(), String> {
    let dest = dest_dir.join(&entry.path);
    if fs::metadata(&dest)
        .map(|meta| meta.len() == entry.bytes)
        .unwrap_or(false)
    {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
    }

    // Ask before writing anything: a length that disagrees with the plan means the pinned
    // revision is not what this code was written against.
    let (server_bytes, server_sha) = probe(&entry.url)?;
    if server_bytes != entry.bytes {
        return Err(format!(
            "{}: the server has {server_bytes} bytes where this revision should have {}",
            entry.path, entry.bytes
        ));
    }

    let part = dest.with_extension("part");
    {
        let file = File::create(&part).map_err(|e| format!("create {part:?}: {e}"))?;
        // Sized before any worker writes into it: a positional write past the end would
        // otherwise leave a hole.
        file.set_len(entry.bytes)
            .map_err(|e| format!("resize {part:?}: {e}"))?;
    }

    let ranges = if entry.bytes >= PARALLEL_THRESHOLD {
        plan_ranges(entry.bytes, CONNECTIONS)
    } else {
        vec![(0, entry.bytes.saturating_sub(1))]
    };

    // Workers accumulate here. The caller's callback cannot cross threads, so the main
    // thread drains this and reports the deltas.
    let written = Arc::new(AtomicU64::new(0));
    let outstanding = Arc::new(AtomicUsize::new(ranges.len()));
    let errors = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

    for (start, end) in ranges {
        let url = entry.url.clone();
        let part = part.clone();
        let written = Arc::clone(&written);
        let outstanding = Arc::clone(&outstanding);
        let errors = Arc::clone(&errors);
        std::thread::spawn(move || {
            use std::os::unix::fs::FileExt;
            let handle = match OpenOptions::new().write(true).open(&part) {
                Ok(handle) => handle,
                Err(error) => {
                    errors
                        .lock()
                        .unwrap()
                        .push(format!("open {part:?}: {error}"));
                    outstanding.fetch_sub(1, Ordering::Relaxed);
                    return;
                }
            };
            let mut response = match ureq::get(&url)
                .header("Accept-Encoding", "identity")
                .header("Range", format!("bytes={start}-{end}"))
                .call()
            {
                Ok(response) => response,
                Err(error) => {
                    errors
                        .lock()
                        .unwrap()
                        .push(format!("GET {url} [{start}-{end}]: {error}"));
                    outstanding.fetch_sub(1, Ordering::Relaxed);
                    return;
                }
            };
            let mut body = response.body_mut().as_reader();
            let mut buffer = vec![0u8; 256 * 1024];
            let mut offset = start;
            loop {
                let read = match body.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(error) => {
                        errors.lock().unwrap().push(format!("read {url}: {error}"));
                        outstanding.fetch_sub(1, Ordering::Relaxed);
                        return;
                    }
                };
                // Positional write: the workers share the file but never each other's bytes.
                if let Err(error) = handle.write_all_at(&buffer[..read], offset) {
                    errors
                        .lock()
                        .unwrap()
                        .push(format!("write {part:?}: {error}"));
                    outstanding.fetch_sub(1, Ordering::Relaxed);
                    return;
                }
                offset += read as u64;
                written.fetch_add(read as u64, Ordering::Relaxed);
            }
            outstanding.fetch_sub(1, Ordering::Relaxed);
        });
    }

    // Drain progress on this thread until every worker has finished.
    let mut reported = 0u64;
    loop {
        std::thread::sleep(PROGRESS_INTERVAL);
        let now = written.load(Ordering::Relaxed);
        if now > reported {
            on_bytes(now - reported);
            reported = now;
        }
        if outstanding.load(Ordering::Relaxed) == 0 {
            break;
        }
    }
    // Bytes written between the last drain and completion would otherwise never be
    // reported, and the caller's total would come out short.
    let now = written.load(Ordering::Relaxed);
    if now > reported {
        on_bytes(now - reported);
    }

    let failures = std::mem::take(&mut *errors.lock().unwrap());
    if !failures.is_empty() {
        let _ = fs::remove_file(&part);
        return Err(failures.join("; "));
    }

    let actual = fs::metadata(&part).map_err(|e| e.to_string())?.len();
    if actual != entry.bytes {
        let _ = fs::remove_file(&part);
        return Err(format!(
            "{}: landed {actual} bytes, expected {}",
            entry.path, entry.bytes
        ));
    }

    // Prefer the hash carried by the plan: it is the one whose value was established when
    // the entry was written, and some hosts report no digest at all.
    if let Some(expected) = entry.sha256.clone().or(server_sha) {
        let actual = sha256_of_file(&part)?;
        if actual != expected {
            let _ = fs::remove_file(&part);
            return Err(format!(
                "{}: sha256 {actual} does not match the server's {expected}; the file was discarded",
                entry.path
            ));
        }
    }

    fs::rename(&part, &dest).map_err(|e| format!("rename into {dest:?}: {e}"))
}

/// Installs Kokoro's weights into `dir`, reporting `(path, done, total)` as it goes.
///
/// The Tauri command is a thin wrapper that turns these reports into IPC events, so the
/// part that actually decides what lands on disk is reachable from an integration test
/// without an `AppHandle` — which is the only way to test it against real files.
pub fn install_kokoro_into(
    dir: &Path,
    on_progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<(), String> {
    let plan = kokoro_plan()?;
    install_plan_into(&plan, dir, on_progress)
}

/// Chatterbox's install: the same machinery, its own plan.
///
/// 1.5 GB across eleven files, four of which are the small halves of graph/weights pairs. The
/// only thing that makes this different from Kokoro's is the size, which is why it shares
/// `install_plan_into` rather than repeating it.
pub fn install_chatterbox_into(
    dir: &Path,
    on_progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<(), String> {
    let plan = chatterbox_plan();
    install_plan_into(&plan, dir, on_progress)
}

/// Fetch a whole plan, reporting `(path, done, total)` as it goes.
///
/// The Tauri commands are thin wrappers that turn these reports into IPC events, so the part
/// that actually decides what lands on disk is reachable from an integration test without an
/// `AppHandle` — which is the only way to test it against real files.
fn install_plan_into(
    plan: &[Entry],
    dir: &Path,
    on_progress: &mut dyn FnMut(&str, u64, u64),
) -> Result<(), String> {
    let total: u64 = plan.iter().map(|entry| entry.bytes).sum();
    let mut done: u64 = 0;

    for entry in plan {
        {
            let path = entry.path.as_str();
            let mut on_bytes = |delta: u64| {
                done += delta;
                on_progress(path, done, total);
            };
            fetch(entry, dir, &mut on_bytes)?;
        }
        // A final report per file, so small files still visibly advance.
        on_progress(entry.path.as_str(), done, total);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// The ranges must tile the file exactly: no gap, no overlap, inclusive ends. A hole
    /// here is a corrupt download that the hash check would catch late, if at all.
    #[test]
    fn ranges_tile_the_file_exactly() {
        for (total, parts) in [
            (100u64, 4usize),
            (1000, 4),
            (5, 4),
            (1, 4),
            (0, 4),
            (1_000_000, 7),
            (1u64 << 40, 4),
            (325_532_232, 4),
        ] {
            let ranges = plan_ranges(total, parts);
            if total == 0 {
                assert!(ranges.is_empty());
                continue;
            }
            assert_eq!(ranges[0].0, 0, "first range must start at 0");
            for window in ranges.windows(2) {
                assert_eq!(
                    window[1].0,
                    window[0].1 + 1,
                    "gap or overlap at {window:?} for total={total}"
                );
            }
            assert_eq!(
                ranges.last().unwrap().1,
                total - 1,
                "last range must end at total-1 for total={total}"
            );
            let covered: u64 = ranges.iter().map(|(s, e)| e - s + 1).sum();
            assert_eq!(covered, total, "covered {covered} of {total}");
            assert!(ranges.len() <= parts.max(1));
        }
    }

    #[test]
    fn a_short_file_still_produces_one_range() {
        assert_eq!(plan_ranges(10, 4), vec![(0, 2), (3, 5), (6, 8), (9, 9)]);
    }

    /// `Content-Range` is the only place some small files report their length, so the parse
    /// has to be right or the tokenizer cannot be fetched at all.
    #[test]
    fn content_range_parses_the_total_length() {
        assert_eq!(parse_content_range("bytes 0-0/3497"), Some(3497));
        assert_eq!(
            parse_content_range("bytes 0-0/325532232"),
            Some(325_532_232)
        );
        // Some servers answer a range request with a size they will not state.
        assert_eq!(parse_content_range("bytes 0-0/*"), None);
        assert_eq!(parse_content_range("garbage"), None);
        assert_eq!(parse_content_range(""), None);
    }

    /// Known-answer test for the hash every download is verified with.
    #[test]
    fn sha256_matches_a_known_vector() {
        assert_eq!(
            sha256_of_reader(Cursor::new(b"abc")).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_of_a_file_matches_the_reader_form() {
        let path = std::env::temp_dir().join("kiegen-download-hash-test.bin");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_of_file(&path).unwrap(),
            sha256_of_reader(Cursor::new(b"abc")).unwrap()
        );
        let _ = fs::remove_file(&path);
    }

    /// Every URL must name the pinned commit. A `/main/` URL would follow the repository
    /// forward, and these size checks would then start failing for unrelated reasons.
    #[test]
    fn every_url_is_pinned_to_a_commit() {
        let plan = kokoro_plan().expect("plan");
        assert!(!plan.is_empty());
        for entry in &plan {
            // A pinned revision is 40 hex digits. The HuggingFace entries all share one
            // commit and the dictionaries come from a different repository, so the
            // assertion is "pinned", not "pinned to Kokoro's commit".
            let pinned = entry
                .url
                .split('/')
                .any(|part| part.len() == 40 && part.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(pinned, "{} is not pinned: {}", entry.path, entry.url);
            assert!(!entry.url.contains("/main/"), "{}", entry.url);
            assert!(
                entry.url.contains(&entry.path.replace("lexicon/", "")),
                "{} does not appear in its own url: {}",
                entry.path,
                entry.url
            );
        }
    }

    /// The plan must cover the graph, the tokenizer, and one table per *usable* voice —
    /// not the ones that cannot work here, and not the 512-row stray. Checked in both
    /// espeak-ng states, because that is the only thing that moves the voice count.
    #[test]
    fn the_plan_covers_the_graph_tokenizer_and_usable_voices() {
        for (espeak_ready, expected_voices) in [(false, 28), (true, 41)] {
            let plan = kokoro_plan_with(espeak_ready).expect("plan");
            let paths: Vec<&str> = plan.iter().map(|entry| entry.path.as_str()).collect();
            assert!(paths.contains(&"onnx/model.onnx"));
            assert!(paths.contains(&"tokenizer.json"));

            let voices = paths
                .iter()
                .filter(|path| path.starts_with("voices/"))
                .count();
            assert_eq!(
                voices, expected_voices,
                "one style table per usable voice (espeak_ready={espeak_ready})"
            );
            assert!(paths.contains(&"voices/af_heart.bin"));
            assert!(paths.contains(&"voices/af_sky.bin"));
            // The stray and the Japanese/Chinese voices must stay out of the download in
            // both states.
            assert!(!paths.contains(&"voices/af.bin"), "512-row stray");
            assert!(
                !paths.contains(&"voices/zf_xiaoxiao.bin"),
                "needs a Chinese front end"
            );
            // The espeak-backed voices follow the install: absent without it, present with.
            assert_eq!(
                paths.contains(&"voices/ef_dora.bin"),
                espeak_ready,
                "espeak-backed voices should follow the espeak-ng install"
            );

            assert_eq!(
                plan.len(),
                expected_voices + 4,
                "voices + graph + tokenizer + 2 dictionaries"
            );

            // The dictionaries are part of the install: an engine without them cannot read a
            // word. They are also the only entries that carry their own hash, because the raw
            // host they come from advertises no content digest.
            let hashed: Vec<&str> = plan
                .iter()
                .filter(|entry| entry.sha256.is_some())
                .map(|entry| entry.path.as_str())
                .collect();
            assert_eq!(
                hashed,
                vec!["lexicon/us_gold.json", "lexicon/us_silver.json"],
                "the dictionaries must be in the plan, and they are the entries pinned by hash"
            );

            // Cheap-first ordering: the graph is 325 MB of the 340 MB, so nothing else should
            // have to wait behind it. A failure on a 3.5 kB file must fail fast.
            assert_eq!(
                plan.first().map(|entry| entry.path.as_str()),
                Some("tokenizer.json"),
                "the smallest required file should be fetched first"
            );
            assert_eq!(
                plan.last().map(|entry| entry.path.as_str()),
                Some("onnx/model.onnx"),
                "the 325 MB graph should be fetched last"
            );
        }
    }

    /// The byte total the UI shows is derived from the plan, so it cannot drift from the
    /// work: 325.5 MB graph + 3.5 kB tokenizer + one 522,240-byte table per usable voice +
    /// the two dictionaries. Asserted in both espeak-ng states, because the second one adds
    /// 13 tables and a stale hardcoded figure would understate the download by ~6.8 MB.
    #[test]
    fn the_byte_total_matches_the_plan() {
        let lexicon: u64 = LEXICON_FILES.iter().map(|(_, bytes, _)| *bytes).sum();

        let without = KOKORO_GRAPH_BYTES + KOKORO_TOKENIZER_BYTES + 28 * VOICE_BYTES + lexicon;
        // 340,158,449 before the dictionaries joined the plan; the UI reads "346 MB", and
        // that number comes from here rather than from this comment.
        assert_eq!(without, 346_258_435);

        let with = KOKORO_GRAPH_BYTES + KOKORO_TOKENIZER_BYTES + 41 * VOICE_BYTES + lexicon;
        assert_eq!(
            with, 353_047_555,
            "13 more voice tables than the English-only plan"
        );

        // And the state the machine is actually in agrees with `kokoro_bytes()`.
        let expected = match crate::engine_paths::espeak_ng() {
            Some(_) => with,
            None => without,
        };
        assert_eq!(kokoro_bytes(), expected);
    }

    /// A file already present at the right size must not be fetched again — this is what
    /// makes a retry after a failure cheap.
    #[test]
    fn a_complete_file_is_skipped_without_touching_the_network() {
        let dir = std::env::temp_dir().join("kiegen-download-skip");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("onnx")).unwrap();
        let entry = Entry {
            path: "onnx/model.onnx".into(),
            bytes: 3,
            // Deliberately unreachable: reaching the network here would be an error.
            url: "http://127.0.0.1:1/never".into(),
            sha256: None,
        };
        fs::write(dir.join("onnx/model.onnx"), b"abc").unwrap();
        let mut calls = 0;
        fetch(&entry, &dir, &mut |_| calls += 1).expect("a complete file is a no-op");
        assert_eq!(calls, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Every URL must name the pinned commit — the same rule Kokoro's plan is held to, and
    /// for the same reason: a `/main/` URL would follow the repository forward and these
    /// size checks would then start failing for unrelated reasons.
    #[test]
    fn every_chatterbox_url_is_pinned_to_a_commit() {
        let plan = chatterbox_plan();
        assert!(!plan.is_empty());
        for entry in &plan {
            let pinned = entry
                .url
                .split('/')
                .any(|part| part.len() == 40 && part.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(pinned, "{} is not pinned: {}", entry.path, entry.url);
            assert!(!entry.url.contains("/main/"), "{}", entry.url);
            assert!(
                entry.url.ends_with(&entry.path),
                "{} does not end in its own path: {}",
                entry.path,
                entry.url
            );
        }
    }

    /// The four graphs, and both halves of each. A plan carrying a `.onnx` without its
    /// `*_onnx_data` yields a graph that loads and then fails, so the pairs are asserted
    /// rather than the count alone.
    #[test]
    fn the_chatterbox_plan_covers_every_graph_with_its_weights() {
        let plan = chatterbox_plan();
        let paths: Vec<&str> = plan.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(
            plan.len(),
            11,
            "4 graphs x 2 halves + tokenizer + Cangjie mapping + voice clip"
        );

        for graph in [
            "onnx/embed_tokens",
            "onnx/language_model_q4f16",
            "onnx/conditional_decoder",
            "onnx/speech_encoder",
        ] {
            let head = format!("{graph}.onnx");
            let data = format!("{graph}.onnx_data");
            let at = paths
                .iter()
                .position(|path| *path == head)
                .unwrap_or_else(|| panic!("{head} missing"));
            assert_eq!(paths[at + 1], data, "the weights must follow their graph");
        }

        // The text front end and the zero-shot fallback clip are part of the install too.
        for required in ["tokenizer.json", "Cangjie5_TC.json", "default_voice.wav"] {
            assert!(paths.contains(&required), "{required} missing");
        }

        // Only the language model has quantised variants in this export, so nothing else may
        // be fetched in a quantised spelling the repository does not have.
        let quantised = paths
            .iter()
            .filter(|path| path.contains("_q4") || path.contains("_fp16"))
            .count();
        assert_eq!(
            quantised, 2,
            "the q4f16 language model pair, and nothing else"
        );
        assert!(paths.iter().all(|path| {
            !(path.contains("_q4") || path.contains("_fp16"))
                || path.contains("language_model_q4f16")
        }));

        // Cheap first: a failure on a 71 kB file must not wait behind 1.4 GB of graphs.
        assert_eq!(paths.first(), Some(&"tokenizer.json"));
    }

    /// The byte total the UI shows is derived from the plan, so it cannot drift from the
    /// work: 11 files, 1,508,858,027 bytes, and the pane's "Download 1.5 GB" comes from here
    /// rather than from this comment.
    #[test]
    fn the_chatterbox_byte_total_matches_the_plan() {
        let total: u64 = CHATTERBOX_FILES.iter().map(|(_, bytes)| *bytes).sum();
        assert_eq!(total, 1_508_858_027);
        assert_eq!(chatterbox_bytes(), total);
        assert_eq!(chatterbox_plan().len(), CHATTERBOX_FILES.len());
    }
}
