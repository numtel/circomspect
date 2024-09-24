use log::{debug, trace};
use std::path::PathBuf;
use std::collections::HashMap;

use parser::ParseResult;
use parser::syntax_sugar_remover;

use program_structure::{
    writers::{LogWriter, ReportWriter},
    template_data::TemplateInfo,
    function_data::FunctionInfo,
    file_definition::{FileLibrary, FileLocation, FileID},
    cfg::{Cfg, IntoCfg},
    constants::Curve,
    report::{ReportCollection, Report},
};

use program_structure::template_library::TemplateLibrary;

use crate::{
    analysis_context::{AnalysisContext, AnalysisError},
    get_analysis_passes, config,
};

type CfgCache = HashMap<String, Cfg>;
type ReportCache = HashMap<String, ReportCollection>;

/// A type responsible for caching CFGs and running analysis passes over all
/// functions and templates.
#[derive(Default)]
pub struct AnalysisRunner {
    curve: Curve,
    libraries: Vec<PathBuf>,
    /// The corresponding file library including file includes.
    file_library: FileLibrary,
    /// Template ASTs generated by the parser.
    template_asts: TemplateInfo,
    /// Function ASTs generated by the parser.
    function_asts: FunctionInfo,
    /// Cached template CFGs generated on demand.
    template_cfgs: CfgCache,
    /// Cached function CFGs generated on demand.
    function_cfgs: CfgCache,
    /// Reports created during CFG generation.
    template_reports: ReportCache,
    /// Reports created during CFG generation.
    function_reports: ReportCache,
}

impl AnalysisRunner {
    pub fn new(curve: Curve) -> Self {
        AnalysisRunner { curve, ..Default::default() }
    }

    pub fn with_libraries(mut self, libraries: &[PathBuf]) -> Self {
        self.libraries.extend_from_slice(libraries);
        self
    }

    pub fn with_files(mut self, input_files: &[PathBuf]) -> (Self, ReportCollection) {
        let reports =
            match parser::parse_files(input_files, &self.libraries, &config::COMPILER_VERSION) {
                ParseResult::Program(program, warnings) => {
                    self.template_asts = program.templates;
                    self.function_asts = program.functions;
                    self.file_library = program.file_library;
                    warnings
                }
                ParseResult::Library(library, warnings) => {
                    self.template_asts = library.templates;
                    self.function_asts = library.functions;
                    self.file_library = library.file_library;
                    warnings
                }
            };
        (self, reports)
    }

    pub fn with_src_wasm(mut self, file_contents: &[&str]) -> (Self, ReportCollection) {
        use parser::parse_definition;

        let mut library_contents = HashMap::new();
        let mut file_library = FileLibrary::default();
        for (file_index, file_source) in file_contents.iter().enumerate() {
            let file_name = format!("file-{file_index}.circom");
            let file_id = file_library.add_file(file_name, file_source.to_string(), true);
            library_contents.insert(file_id, vec![parse_definition(file_source).unwrap()]);
        }
        let template_library = TemplateLibrary::new(library_contents, file_library.clone());
        let mut reports = ReportCollection::new();
        let (new_templates, new_functions) = syntax_sugar_remover::remove_syntactic_sugar(
            &template_library.templates,
            &template_library.functions,
            &template_library.file_library,
            &mut reports,
        );
        self.template_asts = new_templates;
        self.function_asts = new_functions;
        self.file_library = template_library.file_library;

        (self, reports)
    }

    /// Convenience method used to generate a runner for testing purposes.
    #[cfg(test)]
    pub fn with_src(mut self, file_contents: &[&str]) -> Self {
        use parser::parse_definition;

        let mut library_contents = HashMap::new();
        let mut file_library = FileLibrary::default();
        for (file_index, file_source) in file_contents.iter().enumerate() {
            let file_name = format!("file-{file_index}.circom");
            let file_id = file_library.add_file(file_name, file_source.to_string(), true);
            library_contents.insert(file_id, vec![parse_definition(file_source).unwrap()]);
        }
        let template_library = TemplateLibrary::new(library_contents, file_library.clone());
        self.template_asts = template_library.templates;
        self.function_asts = template_library.functions;
        self.file_library = template_library.file_library;

        self
    }

    pub fn file_library(&self) -> &FileLibrary {
        &self.file_library
    }

    pub fn template_names(&self, user_input_only: bool) -> Vec<String> {
        // Clone template names to avoid holding multiple references to `self`.
        self.template_asts
            .iter()
            .filter_map(|(name, ast)| {
                if !user_input_only || self.file_library.is_user_input(ast.get_file_id()) {
                    Some(name)
                } else {
                    None
                }
            })
            .cloned()
            .collect()
    }

    pub fn function_names(&self, user_input_only: bool) -> Vec<String> {
        // Clone function names to avoid holding multiple references to `self`.
        self.function_asts
            .iter()
            .filter_map(|(name, ast)| {
                if !user_input_only || self.file_library.is_user_input(ast.get_file_id()) {
                    Some(name)
                } else {
                    None
                }
            })
            .cloned()
            .collect()
    }

    fn analyze_template<W: LogWriter + ReportWriter>(&mut self, name: &str, writer: &mut W) {
        writer.write_message(&format!("analyzing template '{name}'"));

        // We take ownership of the CFG and any previously generated reports
        // here to avoid holding multiple mutable and immutable references to
        // `self`. This may lead to the CFG being regenerated during analysis if
        // the template is invoked recursively. If it is then ¯\_(ツ)_/¯.
        let mut reports = self.take_template_reports(name);
        if let Ok(cfg) = self.take_template(name) {
            for analysis_pass in get_analysis_passes() {
                reports.append(&mut analysis_pass(self, &cfg));
            }
            // Re-insert the CFG into the hash map.
            if self.replace_template(name, cfg) {
                debug!("template `{name}` CFG was regenerated during analysis");
            }
        }
        writer.write_reports(&reports, &self.file_library);
    }

    pub fn analyze_templates<W: LogWriter + ReportWriter>(
        &mut self,
        writer: &mut W,
        user_input_only: bool,
    ) {
        for name in self.template_names(user_input_only) {
            self.analyze_template(&name, writer);
        }
    }

    fn analyze_function<W: LogWriter + ReportWriter>(&mut self, name: &str, writer: &mut W) {
        writer.write_message(&format!("analyzing function '{name}'"));

        // We take ownership of the CFG and any previously generated reports
        // here to avoid holding multiple mutable and immutable references to
        // `self`. This may lead to the CFG being regenerated during analysis if
        // the function is invoked recursively. If it is then ¯\_(ツ)_/¯.
        let mut reports = self.take_function_reports(name);
        if let Ok(cfg) = self.take_function(name) {
            for analysis_pass in get_analysis_passes() {
                reports.append(&mut analysis_pass(self, &cfg));
            }
            // Re-insert the CFG into the hash map.
            if self.replace_function(name, cfg) {
                debug!("function `{name}` CFG was regenerated during analysis");
            }
        }
        writer.write_reports(&reports, &self.file_library);
    }

    pub fn analyze_functions<W: LogWriter + ReportWriter>(
        &mut self,
        writer: &mut W,
        user_input_only: bool,
    ) {
        for name in self.function_names(user_input_only) {
            self.analyze_function(&name, writer);
        }
    }

    /// Report cache from CFG generation. These will be emitted when the
    /// template is analyzed.
    fn append_template_reports(&mut self, name: &str, reports: &mut ReportCollection) {
        self.template_reports.entry(name.to_string()).or_default().append(reports);
    }

    /// Report cache from CFG generation. These will be emitted when the
    /// template is analyzed.
    fn take_template_reports(&mut self, name: &str) -> ReportCollection {
        self.template_reports.remove(name).unwrap_or_default()
    }

    /// Report cache from CFG generation. These will be emitted when the
    /// function is analyzed.
    fn append_function_reports(&mut self, name: &str, reports: &mut ReportCollection) {
        self.function_reports.entry(name.to_string()).or_default().append(reports);
    }

    /// Report cache from CFG generation. These will be emitted when the
    /// function is analyzed.
    fn take_function_reports(&mut self, name: &str) -> ReportCollection {
        self.function_reports.remove(name).unwrap_or_default()
    }

    fn cache_template(&mut self, name: &str) -> Result<&Cfg, AnalysisError> {
        if !self.template_cfgs.contains_key(name) {
            // The template CFG needs to be generated from the AST.
            if self.template_reports.contains_key(name) {
                // We have already failed to generate the CFG.
                return Err(AnalysisError::FailedToLiftTemplate { name: name.to_string() });
            }
            // Get the AST corresponding to the template.
            let Some(ast) = self.template_asts.get(name) else {
                trace!("failed to lift unknown template `{name}`");
                return Err(AnalysisError::UnknownTemplate { name: name.to_string() });
            };
            // Generate the template CFG from the AST. Cache any reports.
            let mut reports = ReportCollection::new();
            let cfg = generate_cfg(ast, &self.curve, &mut reports).map_err(|report| {
                reports.push(*report);
                trace!("failed to lift template `{name}`");
                AnalysisError::FailedToLiftTemplate { name: name.to_string() }
            })?;
            self.append_template_reports(name, &mut reports);
            self.template_cfgs.insert(name.to_string(), cfg);
            trace!("successfully lifted template `{name}`");
        }
        Ok(self.template_cfgs.get(name).unwrap())
    }

    fn cache_function(&mut self, name: &str) -> Result<&Cfg, AnalysisError> {
        if !self.function_cfgs.contains_key(name) {
            // The function CFG needs to be generated from the AST.
            if self.function_reports.contains_key(name) {
                // We have already failed to generate the CFG.
                return Err(AnalysisError::FailedToLiftFunction { name: name.to_string() });
            }
            // Get the AST corresponding to the function.
            let Some(ast) = self.function_asts.get(name) else {
                trace!("failed to lift unknown function `{name}`");
                return Err(AnalysisError::UnknownFunction { name: name.to_string() });
            };
            // Generate the function CFG from the AST. Cache any reports.
            let mut reports = ReportCollection::new();
            let cfg = generate_cfg(ast, &self.curve, &mut reports).map_err(|report| {
                reports.push(*report);
                trace!("failed to lift function `{name}`");
                AnalysisError::FailedToLiftFunction { name: name.to_string() }
            })?;
            self.append_function_reports(name, &mut reports);
            self.function_cfgs.insert(name.to_string(), cfg);
            trace!("successfully lifted function `{name}`");
        }
        Ok(self.function_cfgs.get(name).unwrap())
    }

    pub fn take_template(&mut self, name: &str) -> Result<Cfg, AnalysisError> {
        self.cache_template(name)?;
        // The CFG must be available since caching was successful.
        Ok(self.template_cfgs.remove(name).unwrap())
    }

    pub fn take_function(&mut self, name: &str) -> Result<Cfg, AnalysisError> {
        self.cache_function(name)?;
        // The CFG must be available since caching was successful.
        Ok(self.function_cfgs.remove(name).unwrap())
    }

    pub fn replace_template(&mut self, name: &str, cfg: Cfg) -> bool {
        self.template_cfgs.insert(name.to_string(), cfg).is_some()
    }

    pub fn replace_function(&mut self, name: &str, cfg: Cfg) -> bool {
        self.function_cfgs.insert(name.to_string(), cfg).is_some()
    }
}

impl AnalysisContext for AnalysisRunner {
    fn is_template(&self, name: &str) -> bool {
        self.template_asts.contains_key(name)
    }

    fn is_function(&self, name: &str) -> bool {
        self.function_asts.contains_key(name)
    }

    fn template(&mut self, name: &str) -> Result<&Cfg, AnalysisError> {
        self.cache_template(name)
    }

    fn function(&mut self, name: &str) -> Result<&Cfg, AnalysisError> {
        self.cache_function(name)
    }

    fn underlying_str(
        &self,
        file_id: &FileID,
        file_location: &FileLocation,
    ) -> Result<String, AnalysisError> {
        let Ok(file) = self.file_library.to_storage().get(*file_id) else {
            return Err(AnalysisError::UnknownFile { file_id: *file_id });
        };
        if file_location.end <= file.source().len() {
            Ok(file.source()[file_location.start..file_location.end].to_string())
        } else {
            Err(AnalysisError::InvalidLocation {
                file_id: *file_id,
                file_location: file_location.clone(),
            })
        }
    }
}

fn generate_cfg<Ast: IntoCfg>(
    ast: Ast,
    curve: &Curve,
    reports: &mut ReportCollection,
) -> Result<Cfg, Box<Report>> {
    ast.into_cfg(curve, reports)
        .map_err(|error| Box::new(error.into()))?
        .into_ssa()
        .map_err(|error| Box::new(error.into()))
}

#[cfg(test)]
mod tests {
    use program_structure::ir::Statement;

    use super::*;

    #[test]
    fn test_function() {
        let mut runner = AnalysisRunner::new(Curve::Goldilocks).with_src(&[r#"
            function foo(a) {
                return a[0] + a[1];
            }
        "#]);

        // Check that `foo` is a known function, that we can access the CFG
        // for `foo`, and that the CFG is properly cached.
        assert!(runner.is_function("foo"));
        assert!(!runner.function_cfgs.contains_key("foo"));
        assert!(runner.function("foo").is_ok());
        assert!(runner.function_cfgs.contains_key("foo"));

        // Check that the `take_function` and `replace_function` APIs work as expected.
        let cfg = runner.take_function("foo").unwrap();
        assert!(!runner.function_cfgs.contains_key("foo"));
        assert!(!runner.replace_function("foo", cfg));
        assert!(runner.function_cfgs.contains_key("foo"));

        // Check that `baz` is not a known function, that attempting to access
        // `baz` produces an error, and that nothing is cached.
        assert!(!runner.is_function("baz"));
        assert!(!runner.function_cfgs.contains_key("baz"));
        assert!(matches!(runner.function("baz"), Err(AnalysisError::UnknownFunction { .. })));
        assert!(!runner.function_cfgs.contains_key("baz"));
    }

    #[test]
    fn test_template() {
        let mut runner = AnalysisRunner::new(Curve::Goldilocks).with_src(&[r#"
            template Foo(n) {
                signal input a[2];

                a[0] === a[1];
            }
        "#]);

        // Check that `Foo` is a known template, that we can access the CFG
        // for `Foo`, and that the CFG is properly cached.
        assert!(runner.is_template("Foo"));
        assert!(!runner.template_cfgs.contains_key("Foo"));
        assert!(runner.template("Foo").is_ok());
        assert!(runner.template_cfgs.contains_key("Foo"));

        // Check that the `take_template` and `replace_template` APIs work as expected.
        let cfg = runner.take_template("Foo").unwrap();
        assert!(!runner.template_cfgs.contains_key("Foo"));
        assert!(!runner.replace_template("Foo", cfg));
        assert!(runner.template_cfgs.contains_key("Foo"));

        // Check that `Baz` is not a known template, that attempting to access
        // `Baz` produces an error, and that nothing is cached.
        assert!(!runner.is_template("Baz"));
        assert!(!runner.template_cfgs.contains_key("Baz"));
        assert!(matches!(runner.template("Baz"), Err(AnalysisError::UnknownTemplate { .. })));
        assert!(!runner.template_cfgs.contains_key("Baz"));
    }

    #[test]
    fn test_underlying_str() {
        use Statement::*;
        let mut runner = AnalysisRunner::new(Curve::Goldilocks).with_src(&[r#"
            template Foo(n) {
                signal input a[2];

                a[0] === a[1];
            }
        "#]);

        let cfg = runner.take_template("Foo").unwrap();
        for stmt in cfg.entry_block().iter() {
            let file_id = stmt.meta().file_id().unwrap();
            let file_location = stmt.meta().file_location();
            let string = runner.underlying_str(&file_id, &file_location).unwrap();
            match stmt {
                // TODO: Why do some statements include the semi-colon and others don't?
                Declaration { .. } => assert_eq!(string, "signal input a[2]"),
                ConstraintEquality { .. } => assert_eq!(string, "a[0] === a[1];"),
                _ => unreachable!(),
            }
        }
    }
}
