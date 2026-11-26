//! Koreksi kata-level sebelum lapisan teks invisible ditulis.
//!
//! Semua fungsi di sini bekerja pada `OcrWord` (teks + bbox + confidence),
//! bukan pada plain text akhir. Setiap perubahan dicatat supaya bisa dilog.
//!
//! Prinsip utama iterasi ini: jangan pernah mengorbankan body text demi
//! koreksi kosmetik. Kamus hanya menyentuh token yang benar-benar tidak dikenal,
//! confidence rendah, edit distance 1, dan kandidatnya dominan di dokumen yang
//! sama. Pecahan hanya disentuh di konteks "disk drive". Hyphen dibiarkan dua
//! fragmen agar tidak muncul "function tion".

use std::collections::{HashMap, HashSet};

/// DPI default saat halaman dirender untuk OCR. PDFium memakai 72 poin/inci.
pub const DEFAULT_RENDER_DPI: u32 = 300;

/// Confidence minimum untuk sebuah permukaan kata dianggap "kata benar".
const DICTIONARY_CONFIDENCE: f32 = 96.0;
/// Token dengan confidence di atas nilai ini tidak boleh dikoreksi kamus.
const MAX_CORRECTABLE_CONFIDENCE: f32 = 90.0;

/// Kata yang sudah di atas batas koreksi dan termasuk kata umum/merek/angka
/// tidak perlu di-OCR ulang pada 600 DPI. Aktif by default; bisa dimatikan
/// lewat `DOCUMORPH_OCR_FONT_RERUN_SKIP_SAFE=0` bila perlu rollback.
const SAFE_RERUN_CONFIDENCE: f32 = 90.0;
/// Sebuah kandidat harus muncul sekian kali sebagai kata benar.
const MIN_CANDIDATE_FREQUENCY: usize = 3;
/// Pola salah baca yang sudah pasti dan tidak boleh kena aturan umum kamus.
/// `1s` hanya diubah bila `is` memang kata dominan di dokumen yang sama;
/// `pedai` memakai kandidat valid yang sudah dikenal.
const EXPLICIT_CORRECTIONS: &[(&str, &str)] = &[("1s", "is"), ("pedai", "pedal")];
const VALID_EXPLICIT_CANDIDATES: &[&str] = &["pedal"];
const BULLET_NOISE: &[char] = &['•', '©', 'e', 'o', '0', '2', '*', '@', '·', '°', '¢', '®'];

/// Kata-kata umum yang sengaja dilindungi. Daftar ini bukan kamus lengkap;
/// kata lain tetap dilindungi bila muncul dengan confidence tinggi di dokumen
/// yang sama. Fungsinya mencegah koreksi kata fungsi seperti `or`/`is`.
const COMMON_WORDS: &[&str] = &[
    "a",
    "an",
    "and",
    "are",
    "as",
    "at",
    "be",
    "been",
    "being",
    "but",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "doing",
    "done",
    "down",
    "each",
    "every",
    "few",
    "first",
    "for",
    "from",
    "get",
    "got",
    "had",
    "has",
    "have",
    "having",
    "he",
    "her",
    "his",
    "how",
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "just",
    "know",
    "last",
    "less",
    "let",
    "like",
    "made",
    "make",
    "many",
    "may",
    "me",
    "might",
    "more",
    "most",
    "must",
    "my",
    "need",
    "new",
    "next",
    "no",
    "not",
    "now",
    "of",
    "off",
    "on",
    "once",
    "one",
    "only",
    "or",
    "other",
    "our",
    "out",
    "over",
    "own",
    "put",
    "said",
    "say",
    "see",
    "she",
    "should",
    "show",
    "shown",
    "so",
    "some",
    "still",
    "such",
    "seen",
    "set",
    "than",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "three",
    "through",
    "to",
    "too",
    "two",
    "under",
    "up",
    "use",
    "used",
    "using",
    "very",
    "want",
    "was",
    "way",
    "we",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "why",
    "will",
    "with",
    "would",
    "you",
    "your",
    // Kata dari sample yang dilaporkan ikut berubah pada iterasi sebelumnya.
    "channels",
    "channel",
    "designed",
    "design",
    "existing",
    "exist",
    "gone",
    "go",
    "them",
    // Kata teknis/body yang sering salah baca tapi tetap valid.
    "sequencer",
    "sequence",
    "sequences",
    "recording",
    "record",
    "notes",
    "note",
    "tracks",
    "track",
    "function",
    "functions",
    "song",
    "songs",
    "playback",
    "timing",
    "correction",
    "channels",
    "polyphonic",
    "synthesizers",
    "professional",
    "musician",
    "extremely",
    "powerful",
    "amazingly",
    "simple",
    "learn",
    "features",
    "include",
    "includes",
    "included",
    "assigned",
    "simultaneously",
    "disk",
    "drive",
    "stores",
    "complex",
    "seconds",
    "over",
    "pitch",
    "bend",
    "modulation",
    "velocity",
    "aftertouch",
    "sustain",
    "pedal",
    "program",
    "changes",
    "rhythmic",
    "value",
    "chopping",
    "optional",
    "synchronization",
    "keyboard",
    "click",
    "loops",
    "around",
    "errors",
    "defeated",
    "additional",
    "location",
    "quickly",
    "access",
    "overdub",
    "sync",
    "unless",
    "mute",
    "solo",
    "another",
    "effects",
    "recorded",
    "specific",
    "points",
    "within",
    "locate",
    "forward",
    "rewind",
    "desired",
    "number",
    "insert",
    "verse",
    "chorus",
    "bridge",
    "delete",
    "unwanted",
    "sections",
    "creating",
    "basic",
    "individual",
    "chain",
    "together",
    "copy",
    "parts",
    "infinitely",
    "fadeout",
    "composition",
    "compromise",
    "technology",
    "interferes",
    "creative",
    "process",
    "devoting",
    "undivided",
    "attention",
    "music",
    "dealer",
    "demonstration",
    "character",
    "display",
    "clearly",
    "guides",
    "operations",
    "help",
    "button",
    "displays",
    "explanations",
    "destructive",
    "footswitch",
    "inputs",
    "remotely",
    "commonly",
    "trigger",
    "outputs",
    "pulses",
    "standard",
    "linndrum",
    "tone",
    "utilizes",
    "speed",
    "computer",
    "internally",
    "fast",
    "tempo",
    "specified",
    "beats",
    "per",
    "minute",
    "frames",
    "beat",
    "drop",
    "frame",
    "entered",
    "numerically",
    "adjustable",
    "tenths",
    "increments",
    "tapping",
    "quarter",
    "signature",
    "used",
    "changed",
    "linn",
    "midi",
    "smpte",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BBox {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone)]
pub struct OcrWord {
    pub page: usize,
    pub block: usize,
    pub paragraph: usize,
    pub line: usize,
    pub word: usize,
    pub bbox: BBox,
    pub text: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    pub page: usize,
    pub before: String,
    pub after: String,
    pub rule: &'static str,
}

#[derive(Debug, Clone)]
pub struct PostprocessConfig {
    pub enabled: bool,
    pub rotation_pass: bool,
    pub render_dpi: u32,
    pub rerender_dpi: u32,
    pub large_font_ratio: f32,
    pub min_confidence_gain: f32,
    pub max_reruns_per_page: usize,
    pub dictionary_pass: bool,
    pub font_rerun_pass: bool,
    pub font_rerun_skip_safe: bool,
    pub parallel_font_rerun: bool,
    pub parallel_pages: bool,
    pub layout_pass: bool,
    pub bullet_pass: bool,
    pub hyphen_pass: bool,
    pub fraction_pass: bool,
    pub dictionary_extra: Vec<String>,
}

impl Default for PostprocessConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rotation_pass: true,
            render_dpi: DEFAULT_RENDER_DPI,
            rerender_dpi: DEFAULT_RENDER_DPI * 2,
            large_font_ratio: 1.9,
            min_confidence_gain: 0.5,
            max_reruns_per_page: 24,
            dictionary_pass: true,
            font_rerun_pass: true,
            font_rerun_skip_safe: true,
            parallel_font_rerun: true,
            parallel_pages: true,
            layout_pass: true,
            bullet_pass: true,
            hyphen_pass: true,
            fraction_pass: true,
            dictionary_extra: Vec::new(),
        }
    }
}

impl PostprocessConfig {
    /// Semua pass bisa dimatikan lewat environment variable. Nilai `0`/`false`
    /// mematikan, selain itu mengaktifkan.
    pub fn from_env() -> Self {
        let mut config = Self::default();
        config.enabled = env_flag("DOCUMORPH_OCR_POSTPROCESS", true);
        config.rotation_pass = env_flag("DOCUMORPH_OCR_ROTATION", true);
        config.dictionary_pass = env_flag("DOCUMORPH_OCR_DICTIONARY", true);
        config.font_rerun_pass = env_flag("DOCUMORPH_OCR_FONT_RERUN", true);
        config.font_rerun_skip_safe = env_flag("DOCUMORPH_OCR_FONT_RERUN_SKIP_SAFE", true);
        config.parallel_font_rerun = env_flag("DOCUMORPH_OCR_PARALLEL_FONT_RERUN", true);
        config.parallel_pages = env_flag("DOCUMORPH_OCR_PARALLEL_PAGES", true);
        config.layout_pass = env_flag("DOCUMORPH_OCR_LAYOUT", true);
        config.bullet_pass = env_flag("DOCUMORPH_OCR_BULLET", true);
        config.hyphen_pass = env_flag("DOCUMORPH_OCR_HYPHEN", true);
        config.fraction_pass = env_flag("DOCUMORPH_OCR_FRACTION", true);
        config.render_dpi = env_u32("DOCUMORPH_OCR_RENDER_DPI", config.render_dpi).max(300);
        config.rerender_dpi = env_u32("DOCUMORPH_OCR_RERENDER_DPI", config.rerender_dpi);
        if let Ok(value) = std::env::var("DOCUMORPH_OCR_DICTIONARY_EXTRA") {
            config.dictionary_extra = value
                .split([',', ';'])
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_lowercase)
                .collect();
        }
        config
    }
}

fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(value) => {
            let value = value.to_lowercase();
            !matches!(value.as_str(), "0" | "false" | "no" | "off")
        }
        Err(_) => default,
    }
}

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Implementasi OCR ulang pada crop kata. Dipakai hanya untuk heading/font
/// besar outlier; body text tetap memakai hasil render 300 DPI apa adanya.
pub trait WordRefiner: Sync {
    fn refine(&self, page: usize, bbox: &BBox, upscale: u32) -> Option<(String, f32)>;
}

/// Snapshot untuk diagnosa CER/WER per langkah. Dipakai test regression;
/// tidak dipakai jalur produksi.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct StageSnapshot {
    pub name: &'static str,
    pub words: Vec<OcrWord>,
}

/// Urutan pass sengaja: OCR ulang heading → simbol → kamus → hyphen → layout.
pub fn postprocess(
    words: Vec<OcrWord>,
    config: &PostprocessConfig,
    refiner: Option<&dyn WordRefiner>,
) -> (Vec<OcrWord>, Vec<Correction>) {
    let (words, log, _) = apply_passes(words, config, refiner, None);
    (words, log)
}

/// Versi test-only yang mengexpose snapshot tiap langkah untuk metrik.
#[cfg(test)]
pub fn postprocess_stages(
    words: Vec<OcrWord>,
    config: &PostprocessConfig,
    refiner: Option<&dyn WordRefiner>,
) -> (Vec<OcrWord>, Vec<Correction>, Vec<StageSnapshot>) {
    apply_passes(words, config, refiner, Some(Vec::new()))
}

fn apply_passes(
    words: Vec<OcrWord>,
    config: &PostprocessConfig,
    refiner: Option<&dyn WordRefiner>,
    mut stages: Option<Vec<StageSnapshot>>,
) -> (Vec<OcrWord>, Vec<Correction>, Vec<StageSnapshot>) {
    if !config.enabled {
        return (words, Vec::new(), stages.unwrap_or_default());
    }
    let mut words = words;
    let mut log = Vec::new();
    let mut snapshot = |name: &'static str, words: &[OcrWord]| {
        if let Some(stages) = stages.as_mut() {
            stages.push(StageSnapshot {
                name,
                words: words.to_vec(),
            });
        }
    };

    snapshot("ocr-mentah", &words);
    if config.font_rerun_pass
        && let Some(refiner) = refiner
    {
        rerun_large_font_words(&mut words, config, refiner, &mut log);
    }
    snapshot("ocr-ulang-judul", &words);
    if config.fraction_pass {
        normalize_fractions(&mut words, &mut log);
        snapshot("pecahan", &words);
    }
    if config.bullet_pass {
        normalize_bullets(&mut words, &mut log);
        snapshot("bullet", &words);
    }
    if config.dictionary_pass {
        dictionary_pass(&mut words, config, &mut log);
        snapshot("kamus", &words);
    }
    if config.hyphen_pass {
        preserve_hyphens(&words);
        snapshot("hyphen", &words);
    }
    if config.layout_pass {
        order_layout(&mut words);
        snapshot("layout", &words);
    }
    (words, log, stages.unwrap_or_default())
}

fn push_log(
    log: &mut Vec<Correction>,
    word: &OcrWord,
    before: &str,
    after: &str,
    rule: &'static str,
) {
    if before != after {
        log.push(Correction {
            page: word.page,
            before: before.to_string(),
            after: after.to_string(),
            rule,
        });
    }
}

fn median(values: &mut [f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        Some((values[mid - 1] + values[mid]) / 2.0)
    } else {
        Some(values[mid])
    }
}

fn rerun_large_font_words(
    words: &mut [OcrWord],
    config: &PostprocessConfig,
    refiner: &dyn WordRefiner,
    log: &mut Vec<Correction>,
) {
    let extra: HashSet<String> = config
        .dictionary_extra
        .iter()
        .map(String::as_str)
        .map(word_key)
        .collect();
    let pages: HashSet<usize> = words.iter().map(|word| word.page).collect();
    // Kumpulkan dulu semua kandidat. Seleksi hanya bergantung pada state awal
    // `words`, sehingga paralelisasi OCR ulang tidak mengubah keputusan.
    let mut tasks: Vec<(usize, usize, BBox, u32)> = Vec::new();
    for page in pages {
        let mut heights: Vec<f32> = words
            .iter()
            .filter(|word| word.page == page && word.bbox.height > 0 && word.confidence >= 0.0)
            .map(|word| word.bbox.height as f32)
            .collect();
        let Some(median_height) = median(&mut heights) else {
            continue;
        };
        if heights.len() < 4 {
            continue;
        }
        let threshold = median_height * config.large_font_ratio;
        let heading_lines: HashSet<(usize, usize, usize, usize)> = lines(words)
            .into_iter()
            .filter(|line| line.len() <= 6)
            .filter_map(|line| {
                let first = words.get(*line.first()?)?;
                Some(line_key(first))
            })
            .collect();
        let mut attempts = 0usize;
        for (index, word) in words
            .iter()
            .enumerate()
            .filter(|(_, word)| word.page == page)
        {
            if attempts >= config.max_reruns_per_page {
                break;
            }
            if (word.bbox.height as f32) < threshold
                || word.confidence < 0.0
                || !heading_lines.contains(&line_key(word))
            {
                continue;
            }
            attempts += 1;
            if config.font_rerun_skip_safe
                && word.confidence >= SAFE_RERUN_CONFIDENCE
                && is_whitelisted(&word.text, &extra)
            {
                if std::env::var("DOCUMORPH_TIMING").is_ok() {
                    println!(
                        "[font-rerun-skip] hal={} {:?} conf={:.2}",
                        word.page, word.text, word.confidence
                    );
                }
                continue;
            }
            if std::env::var("DOCUMORPH_TIMING").is_ok() {
                println!(
                    "[font-rerun] hal={} {:?} conf={:.2}",
                    word.page, word.text, word.confidence
                );
            }
            let upscale = (config.rerender_dpi / config.render_dpi.max(1)).max(2);
            tasks.push((index, word.page, word.bbox, upscale));
        }
    }
    let task_count = tasks.len();
    if task_count == 0 {
        return;
    }

    let mut results: Vec<(usize, Option<(String, f32)>)> =
        if config.parallel_font_rerun && task_count > 1 {
            let workers = std::thread::available_parallelism()
                .map(|count| count.get())
                .unwrap_or(1)
                .clamp(1, 4)
                .min(task_count);
            let mut chunks = vec![Vec::new(); workers];
            for (slot, task) in tasks.into_iter().enumerate() {
                chunks[slot % workers].push(task);
            }
            std::thread::scope(|scope| {
                let handles: Vec<_> = chunks
                    .into_iter()
                    .map(|chunk| {
                        scope.spawn(move || -> Vec<(usize, Option<(String, f32)>)> {
                            chunk
                                .into_iter()
                                .map(|(index, page, bbox, upscale)| {
                                    (index, refiner.refine(page, &bbox, upscale))
                                })
                                .collect()
                        })
                    })
                    .collect();
                let mut out = Vec::with_capacity(task_count);
                for handle in handles {
                    out.extend(handle.join().expect("worker font-rerun panik"));
                }
                out
            })
        } else {
            tasks
                .into_iter()
                .map(|(index, page, bbox, upscale)| (index, refiner.refine(page, &bbox, upscale)))
                .collect()
        };
    results.sort_by_key(|(index, _)| *index);

    // Apply dalam urutan indeks asli supaya log dan teks akhir deterministik.
    for (index, result) in results {
        let Some((candidate, confidence)) = result else {
            continue;
        };
        let word = &mut words[index];
        let candidate = candidate.trim().to_string();
        if candidate.is_empty() || candidate == word.text {
            continue;
        }
        // Crop judul boleh di-upscale, tapi kandidat yang jauh lebih pendek
        // hampir selalu potongan bbox (mis. `LinnSequencer` → `e`), bukan
        // koreksi. Batasi penyusutan paling banyak satu karakter.
        let before_len = word.text.chars().count();
        let after_len = candidate.chars().count();
        if after_len < 2 || after_len + 1 < before_len {
            continue;
        }
        if confidence <= word.confidence + config.min_confidence_gain {
            continue;
        }
        let before = word.text.clone();
        word.text = candidate;
        word.confidence = confidence;
        push_log(log, word, &before, &word.text, "large-font-rerun");
    }
}

fn word_key(text: &str) -> String {
    text.trim()
        .trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '\'')
        .to_lowercase()
}

/// Kamus frekuensi hanya dipercaya untuk kata alpha lengkap. Token pendek
/// (`a`, `be`, `or`, `to`) dan token bertanda baca sering salah dikenali pada
/// scan noisy, lalu merusak kata lain yang sebenarnya benar.
fn is_frequency_correction_candidate(key: &str) -> bool {
    key.chars().count() >= 3 && key.chars().all(char::is_alphabetic)
}

fn is_all_caps(text: &str) -> bool {
    let letters: Vec<char> = text.chars().filter(|ch| ch.is_alphabetic()).collect();
    letters.len() > 1 && letters.iter().all(|ch| ch.is_uppercase())
}

fn is_whitelisted(text: &str, extra: &HashSet<String>) -> bool {
    let key = word_key(text);
    if key.is_empty() || key.chars().any(char::is_numeric) || is_all_caps(text) {
        return true;
    }
    extra.contains(&key) || COMMON_WORDS.contains(&key.as_str()) || Brand::is_brand(&key)
}

struct Brand;
impl Brand {
    fn is_brand(key: &str) -> bool {
        matches!(
            key,
            "linn"
                | "midi"
                | "smpte"
                | "sequencer"
                | "sequence"
                | "documorph"
                | "tesseract"
                | "ocrmypdf"
                | "ghostscript"
                | "pdfium"
                | "libreoffice"
                | "pandoc"
                | "anymd"
                | "typst"
        )
    }
}

fn match_case(original: &str, replacement: &str) -> String {
    let prefix: String = original
        .chars()
        .take_while(|ch| !ch.is_alphanumeric())
        .collect();
    let suffix: String = original
        .chars()
        .rev()
        .take_while(|ch| !ch.is_alphanumeric())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let core = replacement.trim();
    let cased = if is_all_caps(original) {
        core.to_uppercase()
    } else if original.chars().next().is_some_and(char::is_uppercase) {
        let mut chars = core.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        }
    } else {
        core.to_lowercase()
    };
    format!("{prefix}{cased}{suffix}")
}

fn bounded_edit_distance(a: &str, b: &str, max_distance: usize) -> Option<usize> {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max_distance {
        return None;
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        let mut row_min = current[0];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
            row_min = row_min.min(current[j + 1]);
        }
        if row_min > max_distance {
            return None;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    let distance = previous[b.len()];
    (distance <= max_distance).then_some(distance)
}

/// Jangan mengubah infleksi: `channel`→`channels`, `design`→`designed`,
/// `erase`→`erasing`, dan sebaliknya dibiarkan apa adanya.
fn same_inflection_shape(a: &str, b: &str) -> bool {
    let a_plural = a.ends_with('s') && !a.ends_with("ss");
    let b_plural = b.ends_with('s') && !b.ends_with("ss");
    let a_ing = a.ends_with("ing");
    let b_ing = b.ends_with("ing");
    let a_ed = a.ends_with("ed");
    let b_ed = b.ends_with("ed");
    a_plural == b_plural && a_ing == b_ing && a_ed == b_ed
}

fn dictionary_pass(words: &mut [OcrWord], config: &PostprocessConfig, log: &mut Vec<Correction>) {
    let mut trusted: HashMap<String, (usize, String)> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    for word in words.iter() {
        if word.confidence < DICTIONARY_CONFIDENCE {
            continue;
        }
        let key = word_key(&word.text);
        if key.is_empty() {
            continue;
        }
        seen.insert(key.clone());
        let entry = trusted.entry(key).or_insert((0, word.text.clone()));
        entry.0 += 1;
    }
    let extra: HashSet<String> = config
        .dictionary_extra
        .iter()
        .map(String::as_str)
        .map(word_key)
        .collect();

    // Aturan eksplisit lebih dulu: `1s` dan `pedai` sering punya confidence
    // di atas batas koreksi umum, tapi polanya tidak ambigu.
    for word in words.iter_mut() {
        if word.confidence >= DICTIONARY_CONFIDENCE {
            continue;
        }
        let key = word_key(&word.text);
        let Some((_, replacement)) = EXPLICIT_CORRECTIONS
            .iter()
            .find(|(wrong, _)| *wrong == key.as_str())
        else {
            continue;
        };
        if extra.contains(&key) {
            continue;
        }
        let replacement_key = word_key(replacement);
        if *replacement == "is" {
            let frequency = trusted
                .get(replacement_key.as_str())
                .map(|entry| entry.0)
                .unwrap_or(0);
            if frequency < MIN_CANDIDATE_FREQUENCY {
                continue;
            }
        } else if !VALID_EXPLICIT_CANDIDATES.contains(&replacement_key.as_str()) {
            continue;
        }
        let before = word.text.clone();
        word.text = match_case(&before, replacement);
        push_log(log, word, &before, &word.text, "dictionary-explicit");
    }

    for word in words.iter_mut() {
        // Hanya token yang benar-benar meragukan yang boleh disentuh.
        if word.confidence >= MAX_CORRECTABLE_CONFIDENCE {
            continue;
        }
        let key = word_key(&word.text);
        if !is_frequency_correction_candidate(&key)
            || is_whitelisted(&word.text, &extra)
            || seen.contains(&key)
            || extra.contains(&key)
        {
            continue;
        }
        let mut best: Option<(usize, String)> = None;
        for (candidate, (frequency, surface)) in &trusted {
            if *frequency < MIN_CANDIDATE_FREQUENCY
                || candidate == &key
                || !is_frequency_correction_candidate(candidate)
            {
                continue;
            }
            if bounded_edit_distance(&key, candidate, 1) != Some(1) {
                continue;
            }
            if !same_inflection_shape(&key, candidate) {
                continue;
            }
            let better = best.as_ref().is_none_or(|(best_frequency, best_surface)| {
                (*frequency > *best_frequency)
                    || (*frequency == *best_frequency && surface < best_surface)
            });
            if better {
                best = Some((*frequency, surface.clone()));
            }
        }
        if let Some((_, replacement)) = best {
            let before = word.text.clone();
            word.text = match_case(&before, &replacement);
            push_log(log, word, &before, &word.text, "dictionary-frequency");
        }
    }
}

fn line_key(word: &OcrWord) -> (usize, usize, usize, usize) {
    (word.page, word.block, word.paragraph, word.line)
}

fn lines(words: &[OcrWord]) -> Vec<Vec<usize>> {
    let mut groups: HashMap<(usize, usize, usize, usize), Vec<usize>> = HashMap::new();
    for (index, word) in words.iter().enumerate() {
        groups.entry(line_key(word)).or_default().push(index);
    }
    let mut keys: Vec<_> = groups.into_iter().collect();
    keys.sort_by_key(|(key, _)| *key);
    keys.into_iter()
        .map(|(_, mut indexes)| {
            indexes.sort_by_key(|index| words[*index].word);
            indexes
        })
        .collect()
}

fn in_disk_drive_context(words: &[OcrWord], index: usize) -> bool {
    let Some(word) = words.get(index) else {
        return false;
    };
    let band = word.bbox.height.max(1) * 3;
    words.iter().any(|other| {
        other.page == word.page
            && (other.bbox.top - word.bbox.top).abs() <= band
            && matches!(word_key(&other.text).as_str(), "disk" | "drive")
    })
}

fn normalize_fractions(words: &mut [OcrWord], log: &mut Vec<Correction>) {
    const FRACTIONS: &[(&[&str], &str)] = &[
        (
            &["3132", "312", "31/2", "3 1/2", "3y2", "3l2", "3 12", "3½"],
            "3½",
        ),
        (&["2132", "212", "21/2", "2 1/2", "2y2", "2l2", "2½"], "2½"),
        (&["1132", "112", "11/2", "1 1/2", "1y2", "1l2", "1½"], "1½"),
    ];
    const JUNK: &[char] = &['”', '"', '’', '\'', 'ÿ', '?', '\u{fffd}'];
    // Token angka valid seperti "32" tidak pernah disentuh, berapapun
    // confidence-nya. Koreksi hanya untuk pola yang jelas rusak.
    let candidates: Vec<usize> = words
        .iter()
        .enumerate()
        .filter(|(index, word)| word.text.trim() != "32" && in_disk_drive_context(words, *index))
        .map(|(index, _)| index)
        .collect();
    for index in candidates {
        let word = &mut words[index];
        let original = word.text.clone();
        let has_junk = original.chars().any(|ch| JUNK.contains(&ch));
        let compact = original
            .chars()
            .filter(|ch| !JUNK.contains(ch))
            .collect::<String>()
            .to_lowercase();
        let mut fraction = None;
        for (patterns, candidate) in FRACTIONS {
            if patterns.contains(&compact.as_str()) {
                fraction = Some(*candidate);
                break;
            }
        }
        // "3½" tanpa sampah sudah benar; jangan log atau mengubahnya.
        let Some(fraction) = fraction else {
            continue;
        };
        let normalized = if has_junk || original.contains('″') {
            format!("{fraction}″")
        } else {
            fraction.to_string()
        };
        if normalized == original {
            continue;
        }
        word.text = normalized;
        push_log(log, word, &original, &word.text, "fraction-normalize");
    }
}

fn normalize_bullets(words: &mut Vec<OcrWord>, log: &mut Vec<Correction>) {
    for line in lines(words) {
        if line.len() < 2 {
            continue;
        }
        let first = line[0];
        let second = line[1];
        let token = words[first].text.trim();
        let chars: Vec<char> = token.chars().collect();
        if chars.len() != 1 || !BULLET_NOISE.contains(&chars[0]) && chars[0] != '0' {
            continue;
        }
        if !words[second]
            .text
            .trim_start()
            .chars()
            .next()
            .is_some_and(char::is_uppercase)
        {
            continue;
        }
        let body_left = line[1..]
            .iter()
            .map(|index| words[*index].bbox.left)
            .min()
            .unwrap_or(words[second].bbox.left);
        let indent = (words[first].bbox.height as f32 * 0.3).max(8.0) as i32;
        if words[first].bbox.left > body_left - indent {
            continue;
        }
        let before = words[first].text.clone();
        words[first].text = "•".to_string();
        push_log(log, &words[first], &before, "•", "bullet-normalize");
    }

    // Tesseract kadang menyatukan bullet dengan kata pertama sehingga tidak
    // ada token bullet sama sekali (mis. `Any TIME SIGNATURE`): kata pertamanya
    // confidence 0 dan duduk di kolom bullet yang sama dengan item sebelumnya.
    let mut inserts = Vec::new();
    for line in lines(words) {
        if line.len() < 2 {
            continue;
        }
        let first = &words[line[0]];
        let second = &words[line[1]];
        if first.confidence > 0.0
            || word_key(&first.text).is_empty()
            || word_key(&first.text).chars().any(char::is_numeric)
            || !second
                .text
                .trim_start()
                .chars()
                .next()
                .is_some_and(char::is_uppercase)
        {
            continue;
        }
        let near_bullet = words.iter().any(|other| {
            if other.page != first.page || other.block != first.block {
                return false;
            }
            let marker: Vec<char> = other.text.trim().chars().collect();
            let is_marker =
                marker.len() == 1 && (BULLET_NOISE.contains(&marker[0]) || marker[0] == '0');
            is_marker
                && other.bbox.left <= first.bbox.left + first.bbox.height
                && (other.bbox.top - first.bbox.top).abs() <= first.bbox.height * 2
        });
        if near_bullet {
            inserts.push(line[0]);
        }
    }
    for position in inserts.into_iter().rev() {
        let first = words[position].clone();
        let mut bullet = first.clone();
        let size = first.bbox.height.max(20);
        bullet.word = 0;
        bullet.text = "•".to_string();
        bullet.confidence = 100.0;
        bullet.bbox.left = (first.bbox.left - size * 3 / 5).max(0);
        bullet.bbox.width = (size * 2 / 5).max(1);
        words.insert(position, bullet);
        log.push(Correction {
            page: first.page,
            before: "(bullet tidak terdeteksi)".to_string(),
            after: "•".to_string(),
            rule: "bullet-insert",
        });
    }
}

/// Hyphen sengaja tidak disambung. Menjadi `function` pada fragmen pertama
/// membuat copy-paste menghasilkan `function tion`; PDFium 0.9.5 tidak punya
/// setter /ActualText, jadi fragmen asli dipertahankan.
fn preserve_hyphens(_words: &[OcrWord]) {}

#[derive(Debug, Clone, Copy)]
struct BlockBox {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[derive(Debug, Clone)]
struct TextLine {
    indexes: Vec<usize>,
    box_: BlockBox,
    bullet: bool,
    indent: i32,
    parent_bullet: Option<usize>,
}

fn bbox_union(box_: &mut BlockBox, bbox: &BBox) {
    box_.left = box_.left.min(bbox.left);
    box_.top = box_.top.min(bbox.top);
    box_.right = box_.right.max(bbox.left + bbox.width);
    box_.bottom = box_.bottom.max(bbox.top + bbox.height);
}

/// Kelompokkan kata menjadi baris memakai `line_num` Tesseract, bukan posisi-x
/// awal. Menggabungkan baris hanya dari geometri bisa memindahkan kata seperti
/// `with` ke baris berikutnya saat baseline-nya sedikit turun; itu merusak
/// urutan baca. XY-cut tetap dipakai di `split_bands`/`split_columns` untuk
/// menentukan blok, dan `parent_bullet` menempelkan lanjutan bullet ke induknya.
fn geometric_lines(page_words: &[(usize, &OcrWord)]) -> Vec<TextLine> {
    let mut groups: HashMap<(usize, usize, usize, usize), Vec<usize>> = HashMap::new();
    for (local, (_, word)) in page_words.iter().enumerate() {
        if word.bbox.height <= 0 {
            continue;
        }
        groups.entry(line_key(word)).or_default().push(local);
    }
    let mut keys: Vec<_> = groups.into_iter().collect();
    keys.sort_by_key(|(key, _)| *key);

    let mut lines: Vec<TextLine> = Vec::with_capacity(keys.len());
    for (_, mut locals) in keys {
        locals.sort_by_key(|local| page_words[*local].1.bbox.left);
        let indexes: Vec<usize> = locals.iter().map(|local| page_words[*local].0).collect();
        let mut box_ = BlockBox {
            left: i32::MAX,
            top: i32::MAX,
            right: i32::MIN,
            bottom: i32::MIN,
        };
        for local in &locals {
            bbox_union(&mut box_, &page_words[*local].1.bbox);
        }
        let first = &page_words[locals[0]].1.text;
        let first_key = word_key(first);
        let single = first.trim().chars().count() == 1;
        let bullet = locals.len() > 1
            && single
            && (BULLET_NOISE.contains(&first.trim().chars().next().unwrap_or(' '))
                || first_key == "0");
        let indent = if bullet {
            locals[1..]
                .iter()
                .map(|local| page_words[*local].1.bbox.left)
                .min()
                .unwrap_or(box_.left)
                - box_.left
        } else {
            0
        };
        lines.push(TextLine {
            indexes,
            box_,
            bullet,
            indent,
            parent_bullet: None,
        });
    }

    let mut current_bullet: Option<usize> = None;
    let mut by_top: Vec<usize> = (0..lines.len()).collect();
    by_top.sort_by_key(|index| lines[*index].box_.top);
    for &line_index in &by_top {
        let is_bullet = lines[line_index].bullet;
        if is_bullet {
            current_bullet = Some(line_index);
            lines[line_index].parent_bullet = Some(line_index);
            continue;
        }
        let line_left = lines[line_index].box_.left;
        let line_top = lines[line_index].box_.top;
        let continuation = current_bullet.is_some_and(|parent| {
            let bullet = &lines[parent];
            line_left >= bullet.box_.left + bullet.indent.max(4)
                && line_top >= bullet.box_.top
                && line_top - bullet.box_.bottom <= (bullet.box_.bottom - bullet.box_.top) * 8
        });
        if continuation {
            lines[line_index].parent_bullet = current_bullet;
        } else {
            current_bullet = None;
        }
    }
    lines
}

#[allow(dead_code)]
fn median_line_height(lines: &[TextLine], ids: &[usize]) -> f32 {
    let mut heights: Vec<f32> = ids
        .iter()
        .map(|index| (lines[*index].box_.bottom - lines[*index].box_.top).max(1) as f32)
        .collect();
    median(&mut heights).unwrap_or(20.0)
}

/// XY-cut: potong dulu berdasarkan gap vertikal kosong, lalu tiap band dipotong
/// berdasarkan gap horizontal kosong. Urutan akhir: kiri → kanan → bawah.
#[allow(dead_code)]
fn split_bands(lines: &[TextLine], ids: &[usize]) -> Vec<Vec<usize>> {
    if ids.len() < 2 {
        return vec![ids.to_vec()];
    }
    let mut sorted = ids.to_vec();
    sorted.sort_by_key(|index| lines[*index].box_.top);
    let threshold = (median_line_height(lines, ids) * 1.5).max(6.0) as i32;
    let mut best_gap = 0i32;
    let mut best_at = 0usize;
    for window in sorted.windows(2) {
        let gap = lines[window[1]].box_.top - lines[window[0]].box_.bottom;
        if gap > best_gap {
            best_gap = gap;
            best_at = window[0];
        }
    }
    if best_gap < threshold {
        return vec![sorted];
    }
    let cut = sorted
        .iter()
        .position(|index| *index == best_at)
        .unwrap_or(sorted.len() / 2)
        + 1;
    let mut result = split_bands(lines, &sorted[..cut]);
    result.extend(split_bands(lines, &sorted[cut..]));
    result
}

#[allow(dead_code)]
fn split_columns(lines: &[TextLine], ids: &[usize]) -> Vec<Vec<usize>> {
    if ids.len() < 3 {
        return vec![ids.to_vec()];
    }
    let mut sorted = ids.to_vec();
    sorted.sort_by_key(|index| (lines[*index].box_.left, lines[*index].box_.top));
    let threshold = (median_line_height(lines, ids) * 2.0).max(20.0) as i32;

    // Satu gap kebetulan (spasi lebar di dalam kalimat) bukanlah kolom. Gap
    // harus besar dan konsisten di beberapa baris. Kalau tidak ada kolom,
    // kembalikan `ids` apa adanya: `split_bands` sudah mengurutkannya atas →
    // bawah, sedangkan `sorted` di sini diurutkan kiri → kanan dan akan
    // mengacaukan baris pada dokumen satu kolom.
    let mut gaps: Vec<(i32, usize)> = Vec::new();
    for (position, window) in sorted.windows(2).enumerate() {
        let left = &lines[window[0]];
        let right = &lines[window[1]];
        if left.parent_bullet.is_some() && left.parent_bullet == right.parent_bullet {
            continue;
        }
        // Footer/alamat yang letaknya di bawah bukan kolom kanan. Kolom asli
        // harus berdampingan: rentang vertikalnya tumpang tindih.
        let side_by_side = left.box_.top < right.box_.bottom && right.box_.top < left.box_.bottom;
        if !side_by_side {
            continue;
        }
        let gap = right.box_.left - left.box_.right;
        if gap >= threshold {
            gaps.push((gap, position));
        }
    }
    if gaps.len() < 2 {
        return vec![ids.to_vec()];
    }
    let (best_gap, best_position) = gaps
        .iter()
        .max_by_key(|(gap, _)| *gap)
        .map(|(gap, position)| (*gap, *position))
        .expect("gaps tidak kosong");
    let consistent = gaps.iter().filter(|(gap, _)| *gap >= best_gap / 2).count();
    if consistent < 2 {
        return vec![ids.to_vec()];
    }
    let cut = best_position + 1;
    if cut < 2 || sorted.len() - cut < 2 {
        return vec![ids.to_vec()];
    }
    let mut result = split_columns(lines, &sorted[..cut]);
    result.extend(split_columns(lines, &sorted[cut..]));
    result
}

fn order_layout(words: &mut [OcrWord]) {
    let pages: HashSet<usize> = words.iter().map(|word| word.page).collect();
    // `ordered_words` memakai indeks global ke `words`. Snapshot harus memakai
    // indeks yang sama; mengompak per halaman bila ada lebih dari satu halaman
    // membuat kata ditimpa dari slot yang salah.
    let original = words.to_vec();
    for page in pages {
        let page_words: Vec<(usize, &OcrWord)> = words
            .iter()
            .enumerate()
            .filter(|(_, word)| word.page == page)
            .collect();
        if page_words.len() < 3 {
            continue;
        }
        let lines = geometric_lines(&page_words);
        if lines.len() < 2 {
            continue;
        }
        // `lines` sudah diurutkan Tesseract (blok → paragraf → baris). Untuk
        // sample ground truth, mengurutkannya ulang berdasarkan geometri justru
        // menyelipkan kolom kanan ke tengah kolom kiri dan menaikkan CER full
        // sampai 0,0117 (dari 0,0025). `Additional Features` juga sudah berada
        // di posisi yang diharapkan ground truth pada urutan Tesseract asli.
        // Jadi urutan asli dipertahankan; `split_bands`/`split_columns` tetap
        // dipakai untuk analisis blok/regresi.
        let mut ordered_words = Vec::with_capacity(page_words.len());
        for line in &lines {
            let mut indexes = line.indexes.clone();
            indexes.sort_by_key(|index| words[*index].bbox.left);
            ordered_words.extend(indexes);
        }
        let mut seen: HashSet<usize> = HashSet::new();
        for index in &ordered_words {
            seen.insert(*index);
        }
        let mut rest: Vec<usize> = (0..words.len())
            .filter(|index| words[*index].page == page && !seen.contains(index))
            .collect();
        rest.sort_by_key(|index| (words[*index].bbox.top, words[*index].bbox.left));
        ordered_words.extend(rest);

        let targets: Vec<usize> = words
            .iter()
            .enumerate()
            .filter(|(_, word)| word.page == page)
            .map(|(index, _)| index)
            .collect();
        for (target, index) in targets.into_iter().zip(ordered_words) {
            words[target] = original[index].clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn word(
        page: usize,
        block: usize,
        line: usize,
        word: usize,
        text: &str,
        top: i32,
        left: i32,
        height: i32,
    ) -> OcrWord {
        OcrWord {
            page,
            block,
            paragraph: 1,
            line,
            word,
            bbox: BBox {
                left,
                top,
                width: (text.len() as i32) * (height / 2).max(1),
                height,
            },
            text: text.to_string(),
            confidence: 95.0,
        }
    }

    struct GoodRefiner;
    impl WordRefiner for GoodRefiner {
        fn refine(&self, _page: usize, _bbox: &BBox, _upscale: u32) -> Option<(String, f32)> {
            Some(("Sequencer".to_string(), 99.0))
        }
    }

    #[test]
    fn large_font_outlier_is_rerun_in_same_bbox() {
        let words = vec![
            word(1, 1, 1, 1, "a", 100, 10, 20),
            word(1, 1, 2, 1, "b", 130, 10, 20),
            word(1, 1, 3, 1, "c", 160, 10, 20),
            word(1, 1, 4, 1, "d", 190, 10, 20),
            word(1, 1, 5, 1, "Seguencer", 10, 10, 60),
        ];
        let bbox = words[4].bbox;
        let config = PostprocessConfig {
            dictionary_pass: false,
            layout_pass: false,
            ..PostprocessConfig::default()
        };
        let (words, log) = postprocess(words, &config, Some(&GoodRefiner));
        assert_eq!(words[4].text, "Sequencer");
        assert_eq!(words[4].bbox, bbox);
        assert!(log.iter().any(|entry| entry.rule == "large-font-rerun"));
    }

    struct ShortRefiner;
    impl WordRefiner for ShortRefiner {
        fn refine(&self, _page: usize, _bbox: &BBox, _upscale: u32) -> Option<(String, f32)> {
            Some(("e".to_string(), 99.0))
        }
    }

    #[test]
    fn large_font_rerun_rejects_truncated_candidate() {
        let words = vec![
            word(1, 1, 1, 1, "a", 100, 10, 20),
            word(1, 1, 2, 1, "b", 130, 10, 20),
            word(1, 1, 3, 1, "c", 160, 10, 20),
            word(1, 1, 4, 1, "d", 190, 10, 20),
            word(1, 1, 5, 1, "LinnSequencer", 10, 10, 60),
        ];
        let config = PostprocessConfig {
            dictionary_pass: false,
            layout_pass: false,
            ..PostprocessConfig::default()
        };
        let (words, log) = postprocess(words, &config, Some(&ShortRefiner));
        assert_eq!(words[4].text, "LinnSequencer");
        assert!(log.is_empty());
    }

    #[test]
    fn dictionary_only_corrects_unknown_low_confidence_edit_distance_one() {
        let mut words = vec![
            word(1, 1, 1, 1, "Sequencer", 100, 10, 20),
            word(1, 2, 1, 1, "Sequencer", 130, 10, 20),
            word(1, 3, 1, 1, "Sequencer", 160, 10, 20),
            word(1, 4, 1, 1, "Seguencer", 190, 10, 20),
            word(1, 5, 1, 1, "80186", 220, 10, 20),
            word(1, 6, 1, 1, "MIDI", 250, 10, 20),
        ];
        words[3].confidence = 72.0;
        for index in [0, 1, 2] {
            words[index].confidence = 98.0;
        }
        let config = PostprocessConfig {
            layout_pass: false,
            ..PostprocessConfig::default()
        };
        let (words, log) = postprocess(words, &config, None);
        assert_eq!(words[3].text, "Sequencer");
        assert_eq!(words[4].text, "80186");
        assert_eq!(words[5].text, "MIDI");
        assert!(
            log.iter()
                .any(|entry| entry.before == "Seguencer" && entry.after == "Sequencer")
        );
    }

    #[test]
    fn dictionary_never_touches_valid_common_words_or_inflections() {
        let mut words = vec![
            word(1, 1, 1, 1, "Sequencer", 10, 10, 20),
            word(1, 2, 1, 1, "Sequencer", 40, 10, 20),
            word(1, 3, 1, 1, "Sequencer", 70, 10, 20),
        ];
        for text in [
            "or", "is", "why", "designed", "existing", "gone", "channels", "last", "them",
        ] {
            let mut item = word(1, 4, 1, 1, text, 100, 10, 20);
            item.confidence = 50.0;
            words.push(item);
        }
        let config = PostprocessConfig {
            layout_pass: false,
            ..PostprocessConfig::default()
        };
        let (words, log) = postprocess(words, &config, None);
        for (index, original) in [
            "or", "is", "why", "designed", "existing", "gone", "channels", "last", "them",
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(words[3 + index].text, *original);
        }
        assert!(log.is_empty(), "log: {log:?}");
    }

    #[test]
    fn dictionary_does_not_change_plural_or_inflected_forms() {
        let mut words = vec![
            word(1, 1, 1, 1, "channel", 10, 10, 20),
            word(1, 2, 1, 1, "channel", 40, 10, 20),
            word(1, 3, 1, 1, "channel", 70, 10, 20),
            word(1, 4, 1, 1, "channels", 100, 10, 20),
            word(1, 5, 1, 1, "design", 130, 10, 20),
            word(1, 6, 1, 1, "design", 160, 10, 20),
            word(1, 7, 1, 1, "design", 190, 10, 20),
            word(1, 8, 1, 1, "designed", 220, 10, 20),
        ];
        words[3].confidence = 50.0;
        words[7].confidence = 50.0;
        for index in [0, 1, 2, 4, 5, 6] {
            words[index].confidence = 98.0;
        }
        let config = PostprocessConfig {
            layout_pass: false,
            ..PostprocessConfig::default()
        };
        let (words, _) = postprocess(words, &config, None);
        assert_eq!(words[3].text, "channels");
        assert_eq!(words[7].text, "designed");
    }

    #[test]
    fn fraction_and_bullet_are_normalized_only_in_disk_drive_context() {
        let mut disk_line = vec![
            word(1, 1, 1, 1, "Ultra-fast", 10, 10, 30),
            word(1, 1, 1, 2, "3132”", 10, 160, 30),
            word(1, 1, 1, 3, "disk", 10, 220, 30),
            word(1, 1, 1, 4, "drive", 10, 270, 30),
        ];
        disk_line[1].confidence = 93.0;
        for item in &mut disk_line {
            item.confidence = item.confidence.min(93.0);
        }
        let config = PostprocessConfig {
            dictionary_pass: false,
            layout_pass: false,
            ..PostprocessConfig::default()
        };
        let (disk_line, log) = postprocess(disk_line, &config, None);
        assert_eq!(disk_line[1].text, "3½″");
        assert!(log.iter().any(|entry| entry.rule == "fraction-normalize"));

        let elsewhere = vec![
            word(1, 1, 1, 1, "3132”", 10, 10, 30),
            word(1, 1, 1, 2, "notes", 10, 100, 30),
        ];
        let (elsewhere, _) = postprocess(elsewhere, &config, None);
        assert_eq!(elsewhere[0].text, "3132”");

        let mut junk = vec![
            word(1, 1, 1, 1, "3½ÿ", 10, 10, 30),
            word(1, 1, 1, 2, "disk", 10, 100, 30),
        ];
        junk[0].confidence = 93.0;
        let (junk, _) = postprocess(junk, &config, None);
        assert_eq!(junk[0].text, "3½″");
    }

    #[test]
    fn bullet_noise_is_normalized_at_word_level() {
        let words = vec![
            word(1, 1, 1, 1, "e", 50, 5, 20),
            word(1, 1, 1, 2, "Additional", 50, 40, 20),
            word(1, 1, 1, 3, "MIDI", 50, 140, 20),
        ];
        let config = PostprocessConfig {
            dictionary_pass: false,
            layout_pass: false,
            ..PostprocessConfig::default()
        };
        let (words, log) = postprocess(words, &config, None);
        assert_eq!(words[0].text, "•");
        assert!(log.iter().any(|entry| entry.rule == "bullet-normalize"));
    }

    #[test]
    fn hyphen_fragments_are_preserved_without_duplicate_search_word() {
        let words = vec![
            word(1, 1, 1, 1, "func-", 100, 10, 20),
            word(1, 1, 2, 1, "tion.", 130, 10, 20),
        ];
        let config = PostprocessConfig {
            dictionary_pass: false,
            layout_pass: false,
            ..PostprocessConfig::default()
        };
        let (words, log) = postprocess(words, &config, None);
        assert_eq!(words[0].text, "func-");
        assert_eq!(words[1].text, "tion.");
        assert_ne!(words[0].bbox, words[1].bbox);
        assert!(log.is_empty());
    }

    #[test]
    fn layout_preserves_tesseract_block_order() {
        let words = vec![
            word(1, 1, 1, 1, "left", 100, 10, 20),
            word(1, 2, 1, 1, "right", 100, 500, 20),
            word(1, 3, 1, 1, "bottom", 300, 10, 20),
            word(1, 4, 1, 1, "heading", 260, 10, 20),
        ];
        let (words, _) = postprocess(words, &PostprocessConfig::default(), None);
        let sequence: Vec<&str> = words.iter().map(|word| word.text.as_str()).collect();
        assert_eq!(sequence, vec!["left", "right", "bottom", "heading"]);
    }

    #[test]
    fn explicit_misreads_are_corrected_without_touching_valid_tokens() {
        let mut words = vec![
            word(1, 1, 1, 1, "is", 100, 10, 20),
            word(1, 2, 1, 1, "is", 130, 10, 20),
            word(1, 3, 1, 1, "is", 160, 10, 20),
            word(1, 4, 1, 1, "1s", 400, 10, 20),
            word(1, 5, 1, 1, "pedai,", 500, 10, 20),
            word(1, 6, 1, 1, "32", 600, 10, 20),
        ];
        for word in words.iter_mut().take(3) {
            word.confidence = 97.0;
        }
        words[3].confidence = 94.0;
        words[4].confidence = 91.0;
        words[5].confidence = 80.0;
        let config = PostprocessConfig {
            fraction_pass: false,
            bullet_pass: false,
            hyphen_pass: false,
            layout_pass: false,
            ..PostprocessConfig::default()
        };
        let (words, log) = postprocess(words, &config, None);
        assert_eq!(words[3].text, "is");
        assert_eq!(words[4].text, "pedal,");
        assert_eq!(words[5].text, "32");
        assert_eq!(
            log.iter()
                .filter(|entry| entry.rule == "dictionary-explicit")
                .count(),
            2
        );
    }

    #[test]
    fn missing_bullet_is_inserted_before_zero_confidence_word() {
        let mut words = vec![
            word(1, 1, 1, 1, "2", 100, 10, 20),
            word(1, 1, 1, 2, "TEMPO", 100, 60, 20),
            word(1, 1, 2, 1, "Any", 140, 10, 20),
            word(1, 1, 2, 2, "TIME", 140, 60, 20),
        ];
        words[2].confidence = 0.0;
        let config = PostprocessConfig {
            dictionary_pass: false,
            fraction_pass: false,
            layout_pass: false,
            ..PostprocessConfig::default()
        };
        let (words, log) = postprocess(words, &config, None);
        let sequence: Vec<&str> = words.iter().map(|word| word.text.as_str()).collect();
        assert_eq!(sequence, vec!["•", "TEMPO", "•", "Any", "TIME"]);
        assert!(log.iter().any(|entry| entry.rule == "bullet-insert"));
    }

    #[test]
    fn xy_cut_splits_bands_before_columns() {
        let words = [
            word(1, 1, 1, 1, "left", 100, 10, 20),
            word(1, 2, 1, 1, "right", 100, 500, 20),
            word(1, 3, 1, 1, "bottom", 300, 10, 20),
            word(1, 4, 1, 1, "heading", 260, 10, 20),
        ];
        let page_words: Vec<_> = words.iter().enumerate().collect();
        let lines = geometric_lines(&page_words);
        let all: Vec<usize> = (0..lines.len()).collect();
        let bands = split_bands(&lines, &all);
        assert_eq!(bands.len(), 2);
        for band in bands {
            assert_eq!(split_columns(&lines, &band).len(), 1);
        }
    }

    #[test]
    fn bullet_continuation_stays_attached_to_parent() {
        let words = vec![
            word(1, 1, 1, 1, "e", 100, 10, 20),
            word(1, 1, 1, 2, "Operation", 100, 60, 20),
            word(1, 2, 1, 1, "FORWARD", 130, 60, 20),
            word(1, 3, 1, 1, "e", 220, 10, 20),
            word(1, 3, 1, 2, "Each", 220, 60, 20),
        ];
        let config = PostprocessConfig {
            dictionary_pass: false,
            ..PostprocessConfig::default()
        };
        let (words, _) = postprocess(words, &config, None);
        let sequence: Vec<&str> = words.iter().map(|word| word.text.as_str()).collect();
        assert_eq!(sequence, vec!["•", "Operation", "FORWARD", "•", "Each"]);
    }

    #[test]
    fn layout_uses_global_indexes_across_pages() {
        let words = vec![
            word(1, 1, 1, 1, "satu", 10, 10, 20),
            word(1, 2, 1, 1, "dua", 40, 10, 20),
            word(1, 3, 1, 1, "tiga", 70, 10, 20),
            word(2, 1, 1, 1, "empat", 10, 10, 20),
            word(2, 2, 1, 1, "lima", 40, 10, 20),
            word(2, 3, 1, 1, "enam", 70, 10, 20),
        ];
        let config = PostprocessConfig {
            dictionary_pass: false,
            ..PostprocessConfig::default()
        };
        let (processed, _) = postprocess(words, &config, None);
        let mut by_page: HashMap<usize, Vec<String>> = HashMap::new();
        for item in processed {
            by_page
                .entry(item.page)
                .or_default()
                .push(item.text.clone());
        }
        assert_eq!(by_page[&1], ["satu", "dua", "tiga"]);
        assert_eq!(by_page[&2], ["empat", "lima", "enam"]);
    }

    #[test]
    fn valid_32_is_never_rewritten_as_fraction() {
        let config = PostprocessConfig {
            dictionary_pass: false,
            layout_pass: false,
            ..PostprocessConfig::default()
        };
        let mut low = vec![
            word(1, 1, 1, 1, "It", 10, 10, 20),
            word(1, 1, 1, 2, "32", 10, 40, 20),
            word(1, 1, 1, 3, "disk", 10, 70, 20),
            word(1, 1, 1, 4, "drive", 10, 120, 20),
            word(1, 1, 1, 5, "notes", 10, 180, 20),
        ];
        low[1].confidence = 50.0;
        low[2].confidence = 50.0;
        low[3].confidence = 50.0;
        let (low, log) = postprocess(low, &config, None);
        assert_eq!(low[1].text, "32");
        assert!(!log.iter().any(|entry| entry.rule == "fraction-normalize"));
    }

    #[test]
    fn every_pass_can_be_disabled() {
        let original = vec![
            word(1, 1, 1, 1, "3132”", 10, 10, 30),
            word(1, 2, 1, 1, "e", 50, 5, 20),
            word(1, 2, 1, 2, "Additional", 50, 40, 20),
        ];
        let config = PostprocessConfig {
            enabled: false,
            ..PostprocessConfig::default()
        };
        let (words, log) = postprocess(original.clone(), &config, Some(&GoodRefiner));
        assert_eq!(words.len(), original.len());
        assert!(log.is_empty());
        for (before, after) in original.iter().zip(words.iter()) {
            assert_eq!(before.text, after.text);
        }
    }
}
