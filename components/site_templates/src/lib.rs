use include_dir::{Dir, include_dir};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

static BLOG_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../example-sites/blog");
static PERSONAL_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../example-sites/personal");
static PORTFOLIO_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../example-sites/portfolio");

pub struct SiteTemplate {
    pub name: &'static str,
    pub description: &'static str,
    dir: &'static Dir<'static>,
}

impl SiteTemplate {
    /// Write this template's files to a site directory.
    /// `format` is "hiccup" or "html" — determines which template variant to use.
    pub fn scaffold(&self, site_dir: &std::path::Path, format: &str, style: &StyleConfig) -> Result<(), String> {
        self.write_dir(self.dir, site_dir, format)?;
        let css = generate_css(style);
        let css_dir = site_dir.join("assets");
        std::fs::create_dir_all(&css_dir).map_err(|e| format!("mkdir: {e}"))?;
        std::fs::write(css_dir.join("style.css"), css).map_err(|e| format!("write style.css: {e}"))?;
        Ok(())
    }

    fn write_dir(&self, dir: &Dir, target: &std::path::Path, format: &str) -> Result<(), String> {
        for file in dir.files() {
            let path = file.path();
            let path_str = path.to_string_lossy();

            // Skip template files that don't match the chosen format.
            // templates/hiccup/ or templates/html/ — only write the matching one.
            if path_str.contains("templates/hiccup/") && format != "hiccup" {
                continue;
            }
            if path_str.contains("templates/html/") && format != "html" {
                continue;
            }

            // Map templates/{format}/* to templates/*
            let target_path = if path_str.contains(&format!("templates/{format}/")) {
                let remapped = path_str.replacen(&format!("templates/{format}/"), "templates/", 1);
                target.join(std::path::Path::new(&remapped))
            } else {
                target.join(path)
            };

            if let Some(parent) = target_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
            }
            std::fs::write(&target_path, file.contents())
                .map_err(|e| format!("write {}: {e}", target_path.display()))?;
        }
        for subdir in dir.dirs() {
            self.write_dir(subdir, target, format)?;
        }
        Ok(())
    }
}

/// List all available site templates.
pub fn available_templates() -> Vec<SiteTemplate> {
    vec![
        SiteTemplate {
            name: "blog",
            description: "A blog with posts and author profiles",
            dir: &BLOG_DIR,
        },
        SiteTemplate {
            name: "personal",
            description: "A simple personal homepage with pages",
            dir: &PERSONAL_DIR,
        },
        SiteTemplate {
            name: "portfolio",
            description: "Showcase your work with project pages",
            dir: &PORTFOLIO_DIR,
        },
    ]
}

/// Find a template by name. Returns `None` if no template with that name exists.
pub fn template_by_name(name: &str) -> Option<SiteTemplate> {
    available_templates().into_iter().find(|t| t.name == name)
}

// ─── CSS Style Generator ────────────────────────────────────────────────────

/// Font mood pairing: heading font, body font, and Google Fonts query string.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FontMood {
    #[default]
    Clean,
    Techy,
    Literary,
    Friendly,
    Bold,
    Fancy,
    Rugged,
}

impl FontMood {
    /// Returns `(heading_font, body_font, google_fonts_query)`.
    pub fn fonts(self) -> (&'static str, &'static str, &'static str) {
        match self {
            FontMood::Clean => (
                "Inter",
                "Inter",
                "Inter:wght@400;600;700",
            ),
            FontMood::Techy => (
                "JetBrains Mono",
                "Source Sans 3",
                "JetBrains+Mono:wght@400;700&family=Source+Sans+3:wght@400;600",
            ),
            FontMood::Literary => (
                "Playfair Display",
                "Source Serif 4",
                "Playfair+Display:wght@400;700&family=Source+Serif+4:wght@400;600",
            ),
            FontMood::Friendly => (
                "Nunito",
                "Open Sans",
                "Nunito:wght@400;600;700&family=Open+Sans:wght@400;600",
            ),
            FontMood::Bold => (
                "Oswald",
                "Lato",
                "Oswald:wght@400;600;700&family=Lato:wght@400;700",
            ),
            FontMood::Fancy => (
                "Cormorant Garamond",
                "EB Garamond",
                "Cormorant+Garamond:wght@400;600;700&family=EB+Garamond:wght@400;600",
            ),
            FontMood::Rugged => (
                "Bitter",
                "Merriweather",
                "Bitter:wght@400;700&family=Merriweather:wght@400;700",
            ),
        }
    }

    pub fn all() -> &'static [FontMood] {
        &[
            FontMood::Clean,
            FontMood::Techy,
            FontMood::Literary,
            FontMood::Friendly,
            FontMood::Bold,
            FontMood::Fancy,
            FontMood::Rugged,
        ]
    }
}

impl fmt::Display for FontMood {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FontMood::Clean => write!(f, "Clean"),
            FontMood::Techy => write!(f, "Techy"),
            FontMood::Literary => write!(f, "Literary"),
            FontMood::Friendly => write!(f, "Friendly"),
            FontMood::Bold => write!(f, "Bold"),
            FontMood::Fancy => write!(f, "Fancy"),
            FontMood::Rugged => write!(f, "Rugged"),
        }
    }
}

impl FromStr for FontMood {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "clean" => Ok(FontMood::Clean),
            "techy" => Ok(FontMood::Techy),
            "literary" => Ok(FontMood::Literary),
            "friendly" => Ok(FontMood::Friendly),
            "bold" => Ok(FontMood::Bold),
            "fancy" => Ok(FontMood::Fancy),
            "rugged" => Ok(FontMood::Rugged),
            other => Err(format!("unknown font mood: {other}")),
        }
    }
}

/// Hue rotation offsets defining the color palette structure.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaletteType {
    /// Analogous — offsets [0, 30, -30]
    Warm,
    /// Complementary — offsets [0, 180]
    #[default]
    Cool,
    /// Split-complementary — offsets [0, 150, 210]
    Bold,
}

impl PaletteType {
    pub fn hue_offsets(self) -> &'static [i32] {
        match self {
            PaletteType::Warm => &[0, 30, -30],
            PaletteType::Cool => &[0, 180],
            PaletteType::Bold => &[0, 150, 210],
        }
    }

    pub fn all() -> &'static [PaletteType] {
        &[PaletteType::Warm, PaletteType::Cool, PaletteType::Bold]
    }
}

impl fmt::Display for PaletteType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PaletteType::Warm => write!(f, "Warm"),
            PaletteType::Cool => write!(f, "Cool"),
            PaletteType::Bold => write!(f, "Bold"),
        }
    }
}

impl FromStr for PaletteType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "warm" => Ok(PaletteType::Warm),
            "cool" => Ok(PaletteType::Cool),
            "bold" => Ok(PaletteType::Bold),
            other => Err(format!("unknown palette type: {other}")),
        }
    }
}

/// Controls how many CSS custom properties are emitted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Complexity {
    /// 6 core custom properties.
    #[default]
    Simple,
    /// 14 custom properties including light/dark variants, surface, muted text, border, spacing.
    Involved,
}

impl fmt::Display for Complexity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Complexity::Simple => write!(f, "Simple"),
            Complexity::Involved => write!(f, "Involved"),
        }
    }
}

impl FromStr for Complexity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "simple" => Ok(Complexity::Simple),
            "involved" => Ok(Complexity::Involved),
            other => Err(format!("unknown complexity: {other}")),
        }
    }
}

/// Configuration for `generate_css`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StyleConfig {
    pub font_mood: FontMood,
    /// Seed color as a hex string (with or without leading `#`).
    pub seed_color: String,
    pub palette_type: PaletteType,
    pub complexity: Complexity,
}

impl Default for StyleConfig {
    fn default() -> Self {
        Self {
            font_mood: FontMood::Clean,
            seed_color: "#2563eb".to_owned(),
            palette_type: PaletteType::Cool,
            complexity: Complexity::Simple,
        }
    }
}

// ─── Color helpers ──────────────────────────────────────────────────────────

/// Parse a hex color string (`#rrggbb` or `rrggbb`, case-insensitive) into `(h, s, l)`
/// where h ∈ [0, 360), s ∈ [0, 100], l ∈ [0, 100].
pub fn hex_to_hsl(hex: &str) -> Result<(f64, f64, f64), String> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return Err(format!("expected 6 hex digits, got: {hex}"));
    }
    let r = u8::from_str_radix(&hex[0..2], 16).map_err(|e| e.to_string())? as f64 / 255.0;
    let g = u8::from_str_radix(&hex[2..4], 16).map_err(|e| e.to_string())? as f64 / 255.0;
    let b = u8::from_str_radix(&hex[4..6], 16).map_err(|e| e.to_string())? as f64 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let l = (max + min) / 2.0;

    let s = if delta == 0.0 {
        0.0
    } else {
        delta / (1.0 - (2.0 * l - 1.0).abs())
    };

    let h = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };

    let h = ((h % 360.0) + 360.0) % 360.0;

    Ok((h, s * 100.0, l * 100.0))
}

fn hsl_to_css(h: f64, s: f64, l: f64) -> String {
    format!("hsl({:.1}, {:.1}%, {:.1}%)", h, s, l)
}

fn rotate_hue(h: f64, offset: i32) -> f64 {
    ((h + offset as f64) % 360.0 + 360.0) % 360.0
}

// ─── CSS generation ─────────────────────────────────────────────────────────

/// Generate a complete CSS stylesheet from a `StyleConfig`.
///
/// The output includes:
/// - A `@import` for Google Fonts (move to `<link>` tags for production performance).
/// - A `:root` block with CSS custom properties.
/// - Structural styles for body, headings, links, nav, and breadcrumbs.
pub fn generate_css(config: &StyleConfig) -> String {
    let (h_font, b_font, gf_query) = config.font_mood.fonts();

    let (h, s, l) = hex_to_hsl(&config.seed_color)
        .unwrap_or_else(|_| hex_to_hsl("#2563eb").unwrap());

    let offsets = config.palette_type.hue_offsets();
    let primary_h = rotate_hue(h, offsets[0]);
    let accent_h = if offsets.len() > 1 {
        rotate_hue(h, offsets[1])
    } else {
        rotate_hue(h, 180)
    };

    let color_primary = hsl_to_css(primary_h, s, l);
    let color_accent = hsl_to_css(accent_h, s, l);
    let color_bg = hsl_to_css(primary_h, s * 0.1, 98.0);
    let color_text = hsl_to_css(primary_h, s * 0.15, 15.0);

    let mut vars = format!(
        "  --color-primary: {color_primary};\n\
           --color-accent: {color_accent};\n\
           --color-bg: {color_bg};\n\
           --color-text: {color_text};\n\
           --font-heading: '{h_font}', sans-serif;\n\
           --font-body: '{b_font}', sans-serif;\n"
    );

    if config.complexity == Complexity::Involved {
        let color_primary_light = hsl_to_css(primary_h, s * 0.6, (l + 20.0).min(90.0));
        let color_primary_dark = hsl_to_css(primary_h, s, (l - 15.0).max(5.0));
        let color_accent_light = hsl_to_css(accent_h, s * 0.6, (l + 20.0).min(90.0));
        let color_accent_dark = hsl_to_css(accent_h, s, (l - 15.0).max(5.0));
        let color_surface = hsl_to_css(primary_h, s * 0.05, 96.0);
        let color_text_muted = hsl_to_css(primary_h, s * 0.1, 45.0);
        let color_border = hsl_to_css(primary_h, s * 0.1, 80.0);

        vars.push_str(&format!(
            "  --color-primary-light: {color_primary_light};\n\
             --color-primary-dark: {color_primary_dark};\n\
             --color-accent-light: {color_accent_light};\n\
             --color-accent-dark: {color_accent_dark};\n\
             --color-surface: {color_surface};\n\
             --color-text-muted: {color_text_muted};\n\
             --color-border: {color_border};\n\
             --spacing-sm: 0.5rem;\n\
             --spacing-md: 1rem;\n\
             --spacing-lg: 2rem;\n\
             --spacing-xl: 4rem;\n"
        ));
    }

    format!(
        "/* Generated by Presemble — move @import to <link> tags for production performance */\n\
         @import url('https://fonts.googleapis.com/css2?family={gf_query}&display=swap');\n\
         \n\
         :root {{\n\
         {vars}}}\n\
         \n\
         body {{\n\
           font-family: var(--font-body);\n\
           color: var(--color-text);\n\
           background: var(--color-bg);\n\
           max-width: 72ch;\n\
           margin: 0 auto;\n\
           padding: 1rem 1.5rem;\n\
           line-height: 1.65;\n\
         }}\n\
         \n\
         h1, h2, h3, h4, h5, h6 {{\n\
           font-family: var(--font-heading);\n\
           color: var(--color-primary);\n\
           line-height: 1.2;\n\
           margin-top: 2rem;\n\
           margin-bottom: 0.5rem;\n\
         }}\n\
         \n\
         a {{\n\
           color: var(--color-accent);\n\
           text-decoration: underline;\n\
         }}\n\
         \n\
         a:hover {{\n\
           color: var(--color-primary);\n\
         }}\n\
         \n\
         nav {{\n\
           font-family: var(--font-heading);\n\
           display: flex;\n\
           gap: 1.5rem;\n\
           padding: 0.75rem 0;\n\
           border-bottom: 2px solid var(--color-primary);\n\
           margin-bottom: 2rem;\n\
         }}\n\
         \n\
         nav a {{\n\
           text-decoration: none;\n\
           font-weight: 600;\n\
           color: var(--color-text);\n\
         }}\n\
         \n\
         nav a:hover {{\n\
           color: var(--color-primary);\n\
         }}\n\
         \n\
         .breadcrumb {{\n\
           font-size: 0.875rem;\n\
           color: var(--color-text);\n\
           margin-bottom: 1.5rem;\n\
         }}\n\
         \n\
         .breadcrumb a {{\n\
           color: var(--color-accent);\n\
           text-decoration: none;\n\
         }}\n\
         \n\
         .breadcrumb a:hover {{\n\
           text-decoration: underline;\n\
         }}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── hex_to_hsl ───────────────────────────────────────────────────────────

    #[test]
    fn hex_to_hsl_pure_red() {
        let (h, s, l) = hex_to_hsl("ff0000").unwrap();
        assert!((h - 0.0).abs() < 1.0, "hue should be ~0, got {h}");
        assert!((s - 100.0).abs() < 1.0, "saturation should be ~100, got {s}");
        assert!((l - 50.0).abs() < 1.0, "lightness should be ~50, got {l}");
    }

    #[test]
    fn hex_to_hsl_pure_green() {
        let (h, s, l) = hex_to_hsl("00ff00").unwrap();
        assert!((h - 120.0).abs() < 1.0, "hue should be ~120, got {h}");
        assert!((s - 100.0).abs() < 1.0, "saturation should be ~100, got {s}");
        assert!((l - 50.0).abs() < 1.0, "lightness should be ~50, got {l}");
    }

    #[test]
    fn hex_to_hsl_white() {
        let (h, s, l) = hex_to_hsl("ffffff").unwrap();
        assert!((s - 0.0).abs() < 1.0, "saturation should be 0, got {s}");
        assert!((l - 100.0).abs() < 1.0, "lightness should be 100, got {l}");
        let _ = h; // hue undefined for achromatic
    }

    #[test]
    fn hex_to_hsl_black() {
        let (h, s, l) = hex_to_hsl("000000").unwrap();
        assert!((s - 0.0).abs() < 1.0, "saturation should be 0, got {s}");
        assert!((l - 0.0).abs() < 1.0, "lightness should be 0, got {l}");
        let _ = h;
    }

    #[test]
    fn hex_to_hsl_accepts_hash_prefix() {
        let without = hex_to_hsl("2563eb").unwrap();
        let with_hash = hex_to_hsl("#2563eb").unwrap();
        assert_eq!(without, with_hash);
    }

    #[test]
    fn hex_to_hsl_uppercase() {
        let lower = hex_to_hsl("ff0000").unwrap();
        let upper = hex_to_hsl("FF0000").unwrap();
        assert_eq!(lower, upper);
    }

    #[test]
    fn hex_to_hsl_rejects_short() {
        assert!(hex_to_hsl("fff").is_err());
    }

    // ── generate_css: default config ─────────────────────────────────────────

    #[test]
    fn generate_css_default_contains_core_vars() {
        let config = StyleConfig::default();
        let css = generate_css(&config);
        assert!(css.contains("--color-primary:"), "missing --color-primary");
        assert!(css.contains("--color-accent:"), "missing --color-accent");
        assert!(css.contains("--color-bg:"), "missing --color-bg");
        assert!(css.contains("--color-text:"), "missing --color-text");
        assert!(css.contains("--font-heading:"), "missing --font-heading");
        assert!(css.contains("--font-body:"), "missing --font-body");
    }

    #[test]
    fn generate_css_default_contains_google_fonts_import() {
        let config = StyleConfig::default();
        let css = generate_css(&config);
        assert!(css.contains("@import"), "missing @import");
        assert!(css.contains("fonts.googleapis.com"), "missing google fonts url");
    }

    #[test]
    fn generate_css_default_contains_production_comment() {
        let config = StyleConfig::default();
        let css = generate_css(&config);
        assert!(css.contains("<link>"), "missing production tip comment");
    }

    #[test]
    fn generate_css_simple_does_not_contain_extra_vars() {
        let config = StyleConfig::default(); // Simple complexity
        let css = generate_css(&config);
        assert!(!css.contains("--color-primary-light:"), "should not have -light in Simple mode");
        assert!(!css.contains("--color-surface:"), "should not have --color-surface in Simple mode");
    }

    // ── generate_css: each font mood ─────────────────────────────────────────

    #[test]
    fn generate_css_clean_uses_inter() {
        let config = StyleConfig { font_mood: FontMood::Clean, ..Default::default() };
        let css = generate_css(&config);
        assert!(css.contains("Inter"), "expected Inter font");
    }

    #[test]
    fn generate_css_techy_uses_jetbrains_mono() {
        let config = StyleConfig { font_mood: FontMood::Techy, ..Default::default() };
        let css = generate_css(&config);
        assert!(css.contains("JetBrains Mono"), "expected JetBrains Mono heading font");
        assert!(css.contains("Source Sans 3"), "expected Source Sans 3 body font");
    }

    #[test]
    fn generate_css_literary_uses_playfair() {
        let config = StyleConfig { font_mood: FontMood::Literary, ..Default::default() };
        let css = generate_css(&config);
        assert!(css.contains("Playfair Display"), "expected Playfair Display");
        assert!(css.contains("Source Serif 4"), "expected Source Serif 4");
    }

    #[test]
    fn generate_css_friendly_uses_nunito() {
        let config = StyleConfig { font_mood: FontMood::Friendly, ..Default::default() };
        let css = generate_css(&config);
        assert!(css.contains("Nunito"), "expected Nunito");
        assert!(css.contains("Open Sans"), "expected Open Sans");
    }

    #[test]
    fn generate_css_bold_uses_oswald() {
        let config = StyleConfig { font_mood: FontMood::Bold, ..Default::default() };
        let css = generate_css(&config);
        assert!(css.contains("Oswald"), "expected Oswald");
        assert!(css.contains("Lato"), "expected Lato");
    }

    #[test]
    fn generate_css_fancy_uses_cormorant() {
        let config = StyleConfig { font_mood: FontMood::Fancy, ..Default::default() };
        let css = generate_css(&config);
        assert!(css.contains("Cormorant Garamond"), "expected Cormorant Garamond");
        assert!(css.contains("EB Garamond"), "expected EB Garamond");
    }

    #[test]
    fn generate_css_rugged_uses_bitter() {
        let config = StyleConfig { font_mood: FontMood::Rugged, ..Default::default() };
        let css = generate_css(&config);
        assert!(css.contains("Bitter"), "expected Bitter");
        assert!(css.contains("Merriweather"), "expected Merriweather");
    }

    // ── generate_css: Involved complexity ────────────────────────────────────

    #[test]
    fn generate_css_involved_has_extra_vars() {
        let config = StyleConfig {
            complexity: Complexity::Involved,
            ..Default::default()
        };
        let css = generate_css(&config);
        assert!(css.contains("--color-primary-light:"), "missing --color-primary-light");
        assert!(css.contains("--color-primary-dark:"), "missing --color-primary-dark");
        assert!(css.contains("--color-accent-light:"), "missing --color-accent-light");
        assert!(css.contains("--color-accent-dark:"), "missing --color-accent-dark");
        assert!(css.contains("--color-surface:"), "missing --color-surface");
        assert!(css.contains("--color-text-muted:"), "missing --color-text-muted");
        assert!(css.contains("--color-border:"), "missing --color-border");
        assert!(css.contains("--spacing-sm:"), "missing --spacing-sm");
        assert!(css.contains("--spacing-md:"), "missing --spacing-md");
        assert!(css.contains("--spacing-lg:"), "missing --spacing-lg");
        assert!(css.contains("--spacing-xl:"), "missing --spacing-xl");
    }

    // ── FromStr round-trips ──────────────────────────────────────────────────

    #[test]
    fn font_mood_from_str_round_trips() {
        for mood in FontMood::all() {
            let s = mood.to_string();
            let parsed: FontMood = s.parse().expect("should parse back");
            assert_eq!(parsed, *mood, "round-trip failed for {s}");
        }
    }

    #[test]
    fn font_mood_from_str_case_insensitive() {
        assert_eq!("CLEAN".parse::<FontMood>().unwrap(), FontMood::Clean);
        assert_eq!("literary".parse::<FontMood>().unwrap(), FontMood::Literary);
    }

    #[test]
    fn font_mood_from_str_unknown_errors() {
        assert!("Unknown".parse::<FontMood>().is_err());
    }

    #[test]
    fn palette_type_from_str_round_trips() {
        for pt in PaletteType::all() {
            let s = pt.to_string();
            let parsed: PaletteType = s.parse().expect("should parse back");
            assert_eq!(parsed, *pt, "round-trip failed for {s}");
        }
    }

    #[test]
    fn palette_type_from_str_case_insensitive() {
        assert_eq!("WARM".parse::<PaletteType>().unwrap(), PaletteType::Warm);
        assert_eq!("cool".parse::<PaletteType>().unwrap(), PaletteType::Cool);
    }

    #[test]
    fn complexity_from_str_round_trips() {
        let variants = [Complexity::Simple, Complexity::Involved];
        for &c in &variants {
            let s = c.to_string();
            let parsed: Complexity = s.parse().expect("should parse back");
            assert_eq!(parsed, c, "round-trip failed for {s}");
        }
    }

    #[test]
    fn complexity_from_str_case_insensitive() {
        assert_eq!("SIMPLE".parse::<Complexity>().unwrap(), Complexity::Simple);
        assert_eq!("involved".parse::<Complexity>().unwrap(), Complexity::Involved);
    }

    #[test]
    fn available_templates_returns_three() {
        let templates = available_templates();
        assert_eq!(templates.len(), 3);
    }

    #[test]
    fn template_by_name_finds_blog() {
        let t = template_by_name("blog");
        assert!(t.is_some());
        assert_eq!(t.unwrap().name, "blog");
    }

    #[test]
    fn template_by_name_returns_none_for_unknown() {
        assert!(template_by_name("nonexistent").is_none());
    }

    #[test]
    fn scaffold_blog_hiccup_writes_files() {
        let dir = tempfile::tempdir().unwrap();
        let t = template_by_name("blog").unwrap();
        t.scaffold(dir.path(), "hiccup", &StyleConfig::default()).unwrap();

        assert!(dir.path().join("schemas/post/item.md").exists());
        assert!(dir.path().join("schemas/author/item.md").exists());
        assert!(dir.path().join("templates/post/item.hiccup").exists());
        assert!(dir.path().join("templates/author/item.hiccup").exists());
        // HTML templates should NOT be present
        assert!(!dir.path().join("templates/post/item.html").exists());
    }

    #[test]
    fn scaffold_blog_html_writes_html_templates() {
        let dir = tempfile::tempdir().unwrap();
        let t = template_by_name("blog").unwrap();
        t.scaffold(dir.path(), "html", &StyleConfig::default()).unwrap();

        assert!(dir.path().join("templates/post/item.html").exists());
        // Hiccup templates should NOT be present
        assert!(!dir.path().join("templates/post/item.hiccup").exists());
    }

    #[test]
    fn scaffold_personal_creates_page_schema() {
        let dir = tempfile::tempdir().unwrap();
        let t = template_by_name("personal").unwrap();
        t.scaffold(dir.path(), "hiccup", &StyleConfig::default()).unwrap();
        assert!(dir.path().join("schemas/page/item.md").exists());
    }

    #[test]
    fn scaffold_portfolio_creates_project_schema() {
        let dir = tempfile::tempdir().unwrap();
        let t = template_by_name("portfolio").unwrap();
        t.scaffold(dir.path(), "hiccup", &StyleConfig::default()).unwrap();
        assert!(dir.path().join("schemas/project/item.md").exists());
    }

    #[test]
    fn scaffold_with_custom_style_generates_css() {
        let dir = tempfile::tempdir().unwrap();
        let t = template_by_name("blog").unwrap();
        let style = StyleConfig {
            font_mood: FontMood::Techy,
            ..Default::default()
        };
        t.scaffold(dir.path(), "hiccup", &style).unwrap();
        let css = std::fs::read_to_string(dir.path().join("assets/style.css")).unwrap();
        assert!(css.contains("JetBrains Mono"), "expected techy heading font");
        assert!(css.contains("--color-primary:"), "expected color vars");
    }
}
