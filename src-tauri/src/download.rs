//! Fetching model weights from HuggingFace into the app's data directory.
//!
//! Only engines whose weights are *plain files the app owns* go through here. The MLX
//! engines (Qwen, Chatterbox) keep their weights inside the HuggingFace cache that their
//! own Python runtime manages, so they must be installed *by* that runtime — hand-placing
//! files into a cache layout we do not control is how you get a "downloaded" model that
//! the library then cannot find.
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

/// Every shipped voice style table measured 522,240 bytes (510 rows x 256 float32). The
/// repo also carries `voices/af.bin` at 524,288 bytes — 512 rows, absent from Kokoro's
/// documented voice list — which is exactly why it is offered nowhere.
const VOICE_BYTES: u64 = 522_240;

/// One file to fetch. `url` is already resolved to a pinned commit.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Repo-relative path, e.g. `onnx/model.onnx`. Also the path under the destination.
    pub path: String,
    /// Expected size, re-checked against the server before any byte is written.
    pub bytes: u64,
    pub url: String,
}

/// What Kokoro needs on disk: the graph, the phoneme tokenizer, and one style table per
/// *usable* voice.
///
/// Only the 28 English voices are fetched. The other 26 cannot be used without a front end
/// that cannot ship here — espeak-ng is GPL-3.0, and the Japanese and Chinese front ends
/// are separate modules — so their style tables would be 13 MB of dead weight.
pub fn kokoro_plan() -> Result<Vec<Entry>, String> {
    let voices: Vec<String> = crate::engines::kokoro_voices()
        .into_iter()
        .filter(|voice| voice.unavailable.is_none())
        .map(|voice| format!("voices/{}.bin", voice.id))
        .collect();
    if voices.is_empty() {
        return Err("no usable Kokoro voices are defined".to_string());
    }

    let mut files: Vec<(String, u64)> =
        vec![("tokenizer.json".to_string(), KOKORO_TOKENIZER_BYTES)];
    files.extend(voices.into_iter().map(|path| (path, VOICE_BYTES)));
    // The graph is 325 MB of the 340 MB total, so it is fetched last: a failure on any of
    // the small files must not have already cost a 325 MB download.
    files.push(("onnx/model.onnx".to_string(), KOKORO_GRAPH_BYTES));

    Ok(files
        .into_iter()
        .map(|(path, bytes)| Entry {
            url: format!("{HF_BASE}/{KOKORO_REPO}/resolve/{KOKORO_COMMIT}/{path}"),
            path,
            bytes,
        })
        .collect())
}

/// Total bytes `kokoro_plan` will fetch, derived from the plan itself so the figure the UI
/// shows cannot drift from the work actually done.
pub fn kokoro_bytes() -> u64 {
    kokoro_plan()
        .map(|plan| plan.iter().map(|entry| entry.bytes).sum())
        .unwrap_or(0)
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
pub fn probe(url: &str) -> Result<(u64, Option<String>), String> {
    if let Ok(response) = ureq::head(url).call() {
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

    if let Some(expected) = server_sha {
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
    let total: u64 = plan.iter().map(|entry| entry.bytes).sum();
    let mut done: u64 = 0;

    for entry in &plan {
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
    fn every_url_is_pinned_to_the_commit() {
        let plan = kokoro_plan().expect("plan");
        assert!(!plan.is_empty());
        for entry in &plan {
            assert!(
                entry.url.contains(KOKORO_COMMIT),
                "{} is not pinned: {}",
                entry.path,
                entry.url
            );
            assert!(!entry.url.contains("/main/"), "{}", entry.url);
        }
    }

    /// The plan must cover the graph, the tokenizer, and one table per *usable* voice —
    /// not the 26 that cannot work here, and not the 512-row stray.
    #[test]
    fn the_plan_covers_the_graph_tokenizer_and_usable_voices() {
        let plan = kokoro_plan().expect("plan");
        let paths: Vec<&str> = plan.iter().map(|entry| entry.path.as_str()).collect();
        assert!(paths.contains(&"onnx/model.onnx"));
        assert!(paths.contains(&"tokenizer.json"));

        let voices = paths
            .iter()
            .filter(|path| path.starts_with("voices/"))
            .count();
        assert_eq!(voices, 28, "one style table per usable voice");
        assert!(paths.contains(&"voices/af_heart.bin"));
        assert!(paths.contains(&"voices/af_sky.bin"));
        // The stray and the espeak-backed voices must stay out of the download.
        assert!(!paths.contains(&"voices/af.bin"), "512-row stray");
        assert!(!paths.contains(&"voices/ef_dora.bin"), "espeak-backed");
        assert!(
            !paths.contains(&"voices/zf_xiaoxiao.bin"),
            "needs a Chinese front end"
        );

        assert_eq!(plan.len(), 30, "28 voices + graph + tokenizer");

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

    /// The byte total the UI shows is derived from the plan, so it cannot drift from the
    /// work: 325.5 MB graph + 3.5 kB tokenizer + 28 x 522,240.
    #[test]
    fn the_byte_total_matches_the_plan() {
        let expected = KOKORO_GRAPH_BYTES + KOKORO_TOKENIZER_BYTES + 28 * VOICE_BYTES;
        assert_eq!(kokoro_bytes(), expected);
        assert_eq!(expected, 340_158_449);
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
        };
        fs::write(dir.join("onnx/model.onnx"), b"abc").unwrap();
        let mut calls = 0;
        fetch(&entry, &dir, &mut |_| calls += 1).expect("a complete file is a no-op");
        assert_eq!(calls, 0);
        let _ = fs::remove_dir_all(&dir);
    }
}
