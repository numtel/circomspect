use wasm_bindgen::prelude::*;
use program_analysis::analysis_runner::AnalysisRunner;

use program_structure::constants::Curve;
use program_structure::report::Report;
use program_structure::report::MessageCategory;
use program_structure::writers::{CachedStdoutWriter};

// Expose this function to JavaScript with wasm-bindgen
#[wasm_bindgen]
pub fn analyze_code(file_content: &str, output_level: String, curve: String, verbose: bool) -> String {
    // Set up message category and curve
    let output_level: MessageCategory = match output_level.as_str() {
        "INFO" => MessageCategory::Info,
        "WARNING" => MessageCategory::Warning,
        "ERROR" => MessageCategory::Error,
        _ => MessageCategory::Info,
    };

    let curve = match curve.as_str() {
        "BN254" => Curve::Bn254,
        "BLS12_381" => Curve::Bls12_381,
        "GOLDILOCKS" => Curve::Goldilocks,
        _ => Curve::Bn254,
    };

    // Set up analysis runner, passing the file contents directly
    let runner = AnalysisRunner::new(curve)
        .with_src(&[file_content]);

    "foo".to_string()
}

/// Filters reports based on message category.
fn filter_by_level(report: &Report, output_level: &MessageCategory) -> bool {
    report.category() >= output_level
}

