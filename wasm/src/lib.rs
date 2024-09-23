use wasm_bindgen::prelude::*;
use program_analysis::analysis_runner::AnalysisRunner;
extern crate console_error_panic_hook;

use program_structure::constants::Curve;
use program_structure::writers::{CachedStdoutWriter};

#[wasm_bindgen]
pub fn analyze_code(file_content: &str, curve: String) -> String {
    console_error_panic_hook::set_once();

    let curve = match curve.as_str() {
        "BN254" => Curve::Bn254,
        "BLS12_381" => Curve::Bls12_381,
        "GOLDILOCKS" => Curve::Goldilocks,
        _ => Curve::Bn254,
    };

    // Set up analysis runner, passing the file contents directly
    let mut runner = AnalysisRunner::new(curve)
        .with_src(&[file_content]);
    let mut stdout_writer = CachedStdoutWriter::new(true);

    // Analyze functions and templates in user provided input files.
    runner.analyze_functions(&mut stdout_writer, true);
    runner.analyze_templates(&mut stdout_writer, true);

    let json_string = serde_json::to_string(&stdout_writer.reports()).unwrap();

    json_string
}

