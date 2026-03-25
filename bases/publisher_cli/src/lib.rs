mod error;

pub use error::CliError;

pub fn run() -> Result<(), CliError> {
    let args: Vec<String> = std::env::args().collect();
    let site_dir = args
        .get(1)
        .ok_or_else(|| CliError::Usage("presemble <site-dir>".to_string()))?;

    println!("Building site: {site_dir}");

    let schemas_dir = std::path::Path::new(site_dir).join("schemas");

    let mut any_errors = false;

    // Discover all .md schema files
    let mut schema_entries: Vec<std::fs::DirEntry> = std::fs::read_dir(&schemas_dir)
        .map_err(CliError::Io)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .map(|ext| ext == "md")
                .unwrap_or(false)
        })
        .collect();

    schema_entries.sort_by_key(|e| e.file_name());

    for schema_entry in schema_entries {
        let schema_path = schema_entry.path();

        // Derive the content directory from the schema file stem
        let schema_stem = schema_path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                CliError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("schema file has no valid stem: {}", schema_path.display()),
                ))
            })?;

        let content_dir = std::path::Path::new(site_dir)
            .join("content")
            .join(schema_stem);

        // Read and parse the schema
        let schema_source = std::fs::read_to_string(&schema_path)?;
        let grammar = match schema::parse_schema(&schema_source) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("schema error in {}: {}", schema_path.display(), e);
                any_errors = true;
                continue;
            }
        };

        // Discover content files for this schema
        let content_entries = match std::fs::read_dir(&content_dir) {
            Ok(entries) => entries,
            Err(e) => {
                eprintln!(
                    "warning: could not read content dir {}: {}",
                    content_dir.display(),
                    e
                );
                continue;
            }
        };

        let mut content_paths: Vec<std::path::PathBuf> = content_entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().map(|ext| ext == "md").unwrap_or(false))
            .collect();

        content_paths.sort();

        for content_path in content_paths {
            let file_name = content_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<unknown>");

            let content_source = std::fs::read_to_string(&content_path)?;

            let doc = match content::parse_document(&content_source) {
                Ok(d) => d,
                Err(e) => {
                    println!("{file_name}: FAIL");
                    println!("  parse error: {e}");
                    any_errors = true;
                    continue;
                }
            };

            let result = content::validate(&doc, &grammar);

            if result.is_valid() {
                println!("{file_name}: PASS");

                // Rendering phase — only for valid documents
                let templates_dir = std::path::Path::new(site_dir).join("templates");
                let template_path = templates_dir.join(format!("{schema_stem}.html"));

                if template_path.exists() {
                    // Build data graph — wrap under schema_stem (e.g., "article")
                    let article_graph = template::build_article_graph(&doc, &grammar);
                    let mut context = template::DataGraph::new();
                    context.insert(schema_stem, template::Value::Record(article_graph));

                    // Load and render the template
                    let tmpl_src = std::fs::read_to_string(&template_path)?;
                    let html = template::render_template(&tmpl_src, &context)
                        .map_err(|e| CliError::Render(e.to_string()))?;

                    // Write output
                    let output_dir = std::path::Path::new(site_dir)
                        .join("output")
                        .join(schema_stem);
                    std::fs::create_dir_all(&output_dir)?;
                    let output_path = output_dir.join(
                        content_path.file_stem()
                            .and_then(|s| s.to_str())
                            .map(|s| format!("{s}.html"))
                            .unwrap_or_else(|| "index.html".to_string())
                    );
                    std::fs::write(&output_path, &html)?;
                    println!("  \u{2192} {}", output_path.display());
                }
            } else {
                println!("{file_name}: FAIL");
                for diagnostic in &result.diagnostics {
                    println!(
                        "  [{}] {}",
                        format_severity(&diagnostic.severity),
                        diagnostic.message
                    );
                }
                any_errors = true;
            }
        }
    }

    if any_errors {
        std::process::exit(1);
    }

    Ok(())
}

fn format_severity(severity: &content::Severity) -> &'static str {
    match severity {
        content::Severity::Error => "ERROR",
        content::Severity::Warning => "WARN",
    }
}
