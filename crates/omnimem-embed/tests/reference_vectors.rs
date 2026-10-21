//! The Rust engine must produce the vectors the Python 6.7 engine does.
//!
//! `fixtures/reference_vectors.json` is written by `generate_reference.py`
//! from the Python ONNX engine. Matching it means a store built by 6.7 needs
//! no re-embedding after import, and the recall floor, dedup threshold and
//! skill clustering threshold all carry over unchanged.
//!
//! The model is read from the Hugging Face cache at the pinned revision. When
//! it isn't there the tests say so and pass, rather than fail on a machine
//! that has never run OmniMem.

use std::path::PathBuf;

use omnimem_embed::{EmbedConfig, Embedder, Pooling};
use serde_json::Value;

/// Two float32 pipelines on different ONNX Runtime builds agree to about
/// 1e-6; anything that changes tokenisation or pooling moves components by
/// orders of magnitude more.
const COMPONENT_TOLERANCE: f32 = 1e-4;

fn fixture() -> Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference_vectors.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture present"))
        .expect("fixture is JSON")
}

fn engine() -> Option<Embedder> {
    let config = EmbedConfig::default();
    match Embedder::load(&config) {
        Ok(engine) => Some(engine),
        Err(err) if err.is_model_unavailable() => {
            eprintln!("skipping: {err}");
            None
        }
        Err(err) => panic!("model present but failed to load: {err}"),
    }
}

fn as_vector(value: &Value) -> Vec<f32> {
    value
        .as_array()
        .expect("vector array")
        .iter()
        .map(|x| x.as_f64().expect("number") as f32)
        .collect()
}

fn assert_close(actual: &[f32], expected: &[f32], label: &str) {
    assert_eq!(actual.len(), expected.len(), "{label}: dimension");
    let worst = actual
        .iter()
        .zip(expected)
        .map(|(a, e)| (a - e).abs())
        .fold(0.0f32, f32::max);
    assert!(
        worst <= COMPONENT_TOLERANCE,
        "{label}: worst component diff {worst}"
    );
    let cosine: f32 = actual.iter().zip(expected).map(|(a, e)| a * e).sum();
    assert!(cosine > 0.99999, "{label}: cosine {cosine}");
}

#[test]
fn engine_reports_the_models_configuration() {
    let Some(engine) = engine() else { return };
    let fx = fixture();
    assert_eq!(
        engine.dimension(),
        fx["dimension"].as_u64().unwrap() as usize
    );
    assert_eq!(
        engine.max_seq_length(),
        fx["max_seq_length"].as_u64().unwrap() as usize
    );
    assert_eq!(engine.pooling(), Pooling::Mean);
}

#[test]
fn single_texts_match_the_python_engine() {
    let Some(engine) = engine() else { return };
    for item in fixture()["singles"].as_array().unwrap() {
        let text = item["text"].as_str().unwrap();
        let actual = engine.embed(text).unwrap();
        let norm: f32 = actual.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "{text:?}: norm {norm}");
        assert_close(&actual, &as_vector(&item["vector"]), &format!("{text:?}"));
    }
}

#[test]
fn a_mixed_length_batch_matches_python_and_its_own_singles() {
    let Some(engine) = engine() else { return };
    let fx = fixture();
    let texts: Vec<&str> = fx["batch"]["texts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    let expected = fx["batch"]["vectors"].as_array().unwrap();
    let batch = engine.embed_batch(&texts).unwrap();
    assert_eq!(batch.len(), texts.len());
    for ((text, actual), want) in texts.iter().zip(&batch).zip(expected) {
        assert_close(actual, &as_vector(want), &format!("batch {text:?}"));
        // Padding to the longest member must not leak into mean pooling.
        assert_close(
            actual,
            &engine.embed(text).unwrap(),
            &format!("batch vs single {text:?}"),
        );
    }
}

#[test]
fn an_empty_batch_is_empty() {
    let Some(engine) = engine() else { return };
    assert!(engine.embed_batch(&[]).unwrap().is_empty());
}
