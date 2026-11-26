//! Helper metrik untuk test ground truth OCR. Hanya dikompilasi saat `cargo test`.
//!
//! Ground truth tidak diambil dari hasil OCR kita, melainkan dari resource
//! upstream OCRmyPDF (`tests/resources/linn.txt`) yang disalin ke
//! `tests/fixtures/`. Body ground truth adalah subset baris prosa.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

pub struct CrossDocument {
    pub name: &'static str,
    pub pdf: PathBuf,
    pub reference: String,
    pub body_reference: String,
}

/// Sampel lintas dokumen untuk mendeteksi aturan yang terlalu spesifik.
/// Ground truth dibuat dari teks sumber saat fixture di-generate, bukan dari
/// hasil OCR pipeline.
pub fn cross_documents() -> Vec<CrossDocument> {
    let generated =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../documorph-test-docs/generated");
    [
        ("image-only-no-text", "linn-sequencer"),
        ("id-clean", "id-clean"),
        ("id-noisy", "id-noisy"),
        ("id-skew", "id-skew"),
        ("id-table", "id-table"),
        ("id-lowres", "id-lowres"),
        ("id-blur", "id-blur"),
        ("id-rot90", "id-rot90"),
        ("id-table-skew", "id-table-skew"),
        ("id-two-column", "id-two-column"),
        ("id-linn-noise", "linn-sequencer"),
    ]
    .into_iter()
    .map(|(name, truth)| {
        let reference = fixture(&format!("{truth}.ground-truth.txt"));
        let body_reference = if truth == "linn-sequencer" {
            body_reference()
        } else {
            reference.clone()
        };
        CrossDocument {
            name,
            pdf: generated.join(format!("{name}.pdf")),
            reference,
            body_reference,
        }
    })
    .collect()
}

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures")
        .join(name);
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("fixture {name}: {error}"))
}

pub fn full_reference() -> String {
    fixture("linn-sequencer.ground-truth.txt")
}

pub fn body_reference() -> String {
    fixture("linn-sequencer.body-ground-truth.txt")
}

/// Samakan hal-hal yang memang ekuivalen secara tipografi supaya metrik
/// mengukur akurasi kata, bukan gaya tanda baca.
pub fn normalize_for_metrics(text: &str) -> String {
    let normalized = text
        .replace(['—', '–', '−', '‐'], "-")
        .replace(['’', '‘'], "'")
        .replace(['“', '”'], "\"")
        .replace('½', " 1/2 ")
        .replace(['″', '“', '”'], "\"")
        .replace(['ÿ', '¿'], "?")
        .replace('•', "0");
    normalized
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || matches!(ch, ' ' | '/' | '\'' | '\n') {
                ch.to_lowercase().next().unwrap_or(ch)
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn levenshtein_chars(a: &[char], b: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![0usize; b.len() + 1];
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + usize::from(ca != cb));
        }
        previous = current;
    }
    previous[b.len()]
}

fn levenshtein_words(a: &[&str], b: &[&str]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![0usize; b.len() + 1];
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + usize::from(ca != cb));
        }
        previous = current;
    }
    previous[b.len()]
}

pub fn cer(reference: &str, candidate: &str) -> f64 {
    let reference = normalize_for_metrics(reference);
    let candidate = normalize_for_metrics(candidate);
    let a: Vec<char> = reference.chars().collect();
    let b: Vec<char> = candidate.chars().collect();
    levenshtein_chars(&a, &b) as f64 / b.len().max(1) as f64
}

pub fn wer(reference: &str, candidate: &str) -> f64 {
    let reference = normalize_for_metrics(reference);
    let candidate = normalize_for_metrics(candidate);
    let a: Vec<&str> = reference.split_whitespace().collect();
    let b: Vec<&str> = candidate.split_whitespace().collect();
    levenshtein_words(&a, &b) as f64 / b.len().max(1) as f64
}

/// Ambil hanya baris prosa dari hasil OCR. Baris yang awalnya bukan body
/// (judul, heading, bullet, alamat) tidak ikut dihitung sebagai regresi body.
pub fn body_only(text: &str) -> String {
    body_only_with_reference(&body_reference(), text)
}

pub fn body_only_with_reference(reference: &str, text: &str) -> String {
    let starts: HashSet<String> = reference
        .lines()
        .filter_map(|line| {
            normalize_for_metrics(line)
                .split_whitespace()
                .next()
                .map(str::to_string)
        })
        .collect();
    text.lines()
        .filter(|line| {
            let normalized = normalize_for_metrics(line);
            let first = normalized.split_whitespace().next().unwrap_or("");
            starts.contains(first)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn print_metrics(label: &str, reference: &str, candidate: &str, body_reference: &str) {
    // Untuk fixture yang seluruh barisnya memang body, tidak perlu filter
    // berdasarkan first-word (filter itu hanya dipakai ground truth linn).
    let body_candidate = if body_reference == reference {
        candidate.to_string()
    } else {
        body_only_with_reference(body_reference, candidate)
    };
    println!(
        "{label:<16} CER={:.4} WER={:.4} | body CER={:.4} WER={:.4} (body words {})",
        cer(reference, candidate),
        wer(reference, candidate),
        cer(body_reference, &body_candidate),
        wer(body_reference, &body_candidate),
        normalize_for_metrics(&body_candidate)
            .split_whitespace()
            .count(),
    );
}
