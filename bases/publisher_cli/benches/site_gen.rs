use std::fs;
use std::path::{Path, PathBuf};

pub struct BenchSiteConfig {
    pub pages: usize,
    pub schemas: usize,
    pub links_per_page: usize,
    pub body_bytes: usize,
}

pub fn generate_site(config: &BenchSiteConfig) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "presemble-bench-{}-{}-{}-{}",
        config.pages, config.schemas, config.links_per_page, config.body_bytes
    ));

    // Clean up from previous runs
    let _ = fs::remove_dir_all(&dir);

    let items_per_schema = config.pages / config.schemas.max(1);
    let remainder = config.pages % config.schemas.max(1);

    // Generate schemas
    for s in 0..config.schemas {
        generate_schema(&dir, s, config);
    }

    // Generate content
    for s in 0..config.schemas {
        let count = items_per_schema + if s < remainder { 1 } else { 0 };
        for i in 0..count {
            generate_content(&dir, s, i, items_per_schema, config);
        }
    }

    // Generate templates
    for s in 0..config.schemas {
        generate_template(&dir, s, config);
    }
    generate_index_template(&dir, config);

    // Minimal assets
    let assets_dir = dir.join("assets");
    fs::create_dir_all(&assets_dir).unwrap();
    fs::write(assets_dir.join("style.css"), "body { font-family: sans-serif; }\n").unwrap();

    dir
}

fn stem_name(s: usize) -> String {
    format!("type{s}")
}

fn generate_schema(dir: &Path, schema_idx: usize, config: &BenchSiteConfig) {
    let stem = stem_name(schema_idx);
    let schema_dir = dir.join("schemas").join(&stem);
    fs::create_dir_all(&schema_dir).unwrap();

    let mut content = String::new();
    content.push_str(&format!("# {stem} title {{#title}}\noccurs\n: exactly once\n\n"));
    content.push_str(&format!("Summary for {stem}. {{#summary}}\noccurs\n: 1..3\n\n"));

    // Link slots pointing to other types (round-robin)
    for l in 0..config.links_per_page {
        let target_schema = (schema_idx + l + 1) % config.schemas;
        let target_stem = stem_name(target_schema);
        content.push_str(&format!(
            "[<name>](/{target_stem}/<name>) {{#link{l}}}\noccurs\n: exactly once\n\n"
        ));
    }

    content.push_str("----\n\nBody content.\nheadings\n: h2..h6\n");

    fs::write(schema_dir.join("item.md"), content).unwrap();
}

fn generate_content(
    dir: &Path,
    schema_idx: usize,
    item_idx: usize,
    items_per_schema: usize,
    config: &BenchSiteConfig,
) {
    let stem = stem_name(schema_idx);
    let content_dir = dir.join("content").join(&stem);
    fs::create_dir_all(&content_dir).unwrap();

    let slug = format!("item-{item_idx:04}");
    let mut content = String::new();

    // Title (capitalized)
    content.push_str(&format!("# Item {item_idx} of {stem}\n\n"));

    // Summary
    content.push_str(&format!(
        "This is the summary for item {item_idx} in the {stem} collection.\n\n"
    ));

    // Cross-links (deterministic, spread across types)
    for l in 0..config.links_per_page {
        let target_schema = (schema_idx + l + 1) % config.schemas;
        let target_stem = stem_name(target_schema);
        // Use a prime multiplier for spread
        let target_idx = (item_idx * 7 + l * 13 + 3) % items_per_schema.max(1);
        let target_slug = format!("item-{target_idx:04}");
        content.push_str(&format!("[Link to {target_stem}](/{target_stem}/{target_slug})\n\n"));
    }

    // Body separator
    content.push_str("----\n\n");

    // Body content of approximately body_bytes size
    let mut body = String::new();
    let mut bytes_written = 0;
    let mut para = 0;
    while bytes_written < config.body_bytes {
        if para % 5 == 0 {
            let line = format!("## Section {para} of item {item_idx}\n\n");
            bytes_written += line.len();
            body.push_str(&line);
        }
        let line = format!(
            "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Paragraph {para} of item {item_idx} in {stem}.\n\n"
        );
        bytes_written += line.len();
        body.push_str(&line);
        para += 1;
    }

    content.push_str(&body);

    fs::write(content_dir.join(format!("{slug}.md")), content).unwrap();
}

fn generate_template(dir: &Path, schema_idx: usize, config: &BenchSiteConfig) {
    let stem = stem_name(schema_idx);
    let template_dir = dir.join("templates").join(&stem);
    fs::create_dir_all(&template_dir).unwrap();

    let mut content = String::from(
        "<!DOCTYPE html>\n<html><head><title>Bench</title>\
         <link rel=\"stylesheet\" href=\"/assets/style.css\" />\
         </head><body><main>\n",
    );
    content.push_str("  <presemble:insert data=\"input.title\" as=\"h1\" />\n");
    content.push_str("  <presemble:insert data=\"input.summary\" as=\"p\" />\n");

    for l in 0..config.links_per_page {
        content.push_str(&format!("  <presemble:insert data=\"input.link{l}\" />\n"));
    }

    content.push_str("  <div><presemble:insert data=\"input.body\" /></div>\n");
    content.push_str("</main></body></html>\n");

    fs::write(template_dir.join("item.html"), content).unwrap();
}

fn generate_index_template(dir: &Path, config: &BenchSiteConfig) {
    let template_dir = dir.join("templates");
    fs::create_dir_all(&template_dir).unwrap();

    let first_stem = stem_name(0);
    let content = format!(
        "<!DOCTYPE html>\n<html><head><title>Bench Index</title>\
         <link rel=\"stylesheet\" href=\"/assets/style.css\" />\
         </head><body><main>\n\
         <h1>Bench Site</h1>\n\
         <template data-each=\"input.{first_stem}\">\n\
         <presemble:insert data=\"item.title\" as=\"h2\" />\n\
         <presemble:insert data=\"item.link\" />\n\
         </template>\n\
         </main></body></html>\n"
    );

    fs::write(template_dir.join("index.html"), content).unwrap();
}
