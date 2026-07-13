use std::path::Path;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style, Theme, ThemeSet};
use syntect::parsing::{SyntaxDefinition, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;
use texti_model::{SyntaxMode, SyntaxSpan};

const PLAIN_TEXT: &str = "Plain Text";
const SLINT_SYNTAX: &str = include_str!("syntaxes/Slint.sublime-syntax");

/// The editor color scheme used when producing render-ready syntax colors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HighlightTheme {
    /// Colors with sufficient contrast on Texti's dark editor background.
    #[default]
    Dark,
    /// Colors with sufficient contrast on Texti's light editor background.
    Light,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedLanguage {
    label: String,
    plain_text: bool,
}

impl DetectedLanguage {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn is_plain_text(&self) -> bool {
        self.plain_text
    }
}

#[derive(Default)]
pub struct SyntaxService;

impl SyntaxService {
    pub fn detect(path: Option<&Path>, text: &str) -> DetectedLanguage {
        let syntax = detect_syntax(path, text);
        detected_language(path, text, syntax)
    }

    pub fn detect_with_mode(
        _mode: SyntaxMode,
        path: Option<&Path>,
        text: &str,
    ) -> DetectedLanguage {
        Self::detect(path, text)
    }

    pub fn highlight(
        path: Option<&Path>,
        text: &str,
        max_bytes: usize,
        theme: HighlightTheme,
    ) -> Vec<SyntaxSpan> {
        let syntax = detect_syntax(path, text);
        if is_plain_text(syntax) {
            return Vec::new();
        }
        highlight_with_syntax(syntax, text, max_bytes, theme)
    }

    pub fn highlight_with_mode(
        _mode: SyntaxMode,
        path: Option<&Path>,
        text: &str,
        max_bytes: usize,
        theme: HighlightTheme,
    ) -> Vec<SyntaxSpan> {
        Self::highlight(path, text, max_bytes, theme)
    }
}

fn syntax_set() -> &'static SyntaxSet {
    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAX_SET.get_or_init(|| {
        let mut builder = two_face::syntax::extra_newlines().into_builder();
        let slint = SyntaxDefinition::load_from_str(SLINT_SYNTAX, true, Some("Slint"))
            .expect("the bundled Slint syntax definition must be valid");
        builder.add(slint);
        builder.build()
    })
}

fn theme(theme: HighlightTheme) -> &'static Theme {
    static THEMES: OnceLock<ThemeSet> = OnceLock::new();
    let themes = THEMES.get_or_init(ThemeSet::load_defaults);
    let name = match theme {
        HighlightTheme::Dark => "base16-ocean.dark",
        HighlightTheme::Light => "base16-ocean.light",
    };
    themes
        .themes
        .get(name)
        .expect("Syntect's embedded themes must contain both base16 Ocean variants")
}

fn detect_syntax<'a>(path: Option<&Path>, text: &str) -> &'a SyntaxReference {
    let ps = syntax_set();
    if let Some(path) = path {
        if let Ok(Some(syntax)) = ps.find_syntax_for_file(path) {
            return syntax;
        }
        if let Some(file_name) = path.file_name().and_then(|name| name.to_str())
            && let Some(syntax) = detect_by_file_name(ps, file_name)
        {
            return syntax;
        }
    }
    let first_line = text.lines().next().unwrap_or_default();
    if let Some(syntax) = ps.find_syntax_by_first_line(first_line) {
        return syntax;
    }
    if let Some(syntax) = detect_by_content(ps, text) {
        return syntax;
    }
    ps.find_syntax_plain_text()
}

fn detect_by_file_name<'a>(ps: &'a SyntaxSet, file_name: &str) -> Option<&'a SyntaxReference> {
    match file_name {
        "Dockerfile" | "Containerfile" => ps.find_syntax_by_name("Dockerfile"),
        "CMakeLists.txt" => ps.find_syntax_by_name("CMake"),
        "Makefile" | "makefile" | "GNUmakefile" => ps.find_syntax_by_name("Makefile"),
        ".env" | ".env.local" | ".env.example" => ps.find_syntax_by_name("DotENV"),
        ".gitignore" | ".gitexclude" => ps
            .find_syntax_by_name("Git Ignore")
            .or_else(|| ps.find_syntax_by_extension("gitignore")),
        ".gitattributes" => ps
            .find_syntax_by_name("Git Attributes")
            .or_else(|| ps.find_syntax_by_extension("gitattributes")),
        ".gitconfig" => ps
            .find_syntax_by_name("Git Config")
            .or_else(|| ps.find_syntax_by_extension("gitconfig")),
        ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" => {
            ps.find_syntax_by_name("Bourne Again Shell (bash)")
        }
        _ => None,
    }
}

fn detect_by_content<'a>(ps: &'a SyntaxSet, text: &str) -> Option<&'a SyntaxReference> {
    let trimmed = text.trim_start();
    let first_line = trimmed.lines().next().unwrap_or_default();
    if first_line.starts_with("fn ")
        || first_line.starts_with("pub fn ")
        || trimmed.contains("fn main(")
        || trimmed.contains("use std::")
    {
        return ps.find_syntax_by_name("Rust");
    }
    if first_line == "package main" || trimmed.contains("\nfunc main(") {
        return ps.find_syntax_by_name("Go");
    }
    if first_line.starts_with("#include ") {
        if text.contains("std::") || text.contains("#include <iostream>") {
            return ps.find_syntax_by_name("C++");
        }
        return ps.find_syntax_by_name("C");
    }
    if first_line.starts_with("class ") || trimmed.contains("public static void main") {
        return ps.find_syntax_by_name("Java");
    }
    if first_line.starts_with("fun main(") || trimmed.contains(": String") {
        return ps.find_syntax_by_name("Kotlin");
    }
    if first_line.starts_with("let ") || first_line.starts_with("import ") {
        return ps
            .find_syntax_by_name("TypeScript")
            .or_else(|| ps.find_syntax_by_name("JavaScript"));
    }
    None
}

fn detected_language(
    path: Option<&Path>,
    text: &str,
    syntax: &SyntaxReference,
) -> DetectedLanguage {
    let label = label_override(path, text).unwrap_or_else(|| normalized_label(&syntax.name));
    DetectedLanguage {
        plain_text: is_plain_text(syntax) && label == PLAIN_TEXT,
        label,
    }
}

fn label_override(path: Option<&Path>, text: &str) -> Option<String> {
    if let Some(file_name) = path
        .and_then(Path::file_name)
        .and_then(|file_name| file_name.to_str())
    {
        let label = match file_name {
            "Dockerfile" | "Containerfile" => "Dockerfile",
            "CMakeLists.txt" => "CMake",
            "Makefile" | "makefile" | "GNUmakefile" => "Makefile",
            ".env" | ".env.local" | ".env.example" => "DotENV",
            ".gitignore" => "Git Ignore",
            ".gitattributes" => "Git Attributes",
            ".gitconfig" => "Git Config",
            _ => "",
        };
        if !label.is_empty() {
            return Some(label.to_string());
        }
    }

    if let Some(extension) = path
        .and_then(Path::extension)
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
    {
        let label = match extension.as_str() {
            "ts" | "tsx" => "TypeScript",
            "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
            "vue" => "Vue",
            "svelte" => "Svelte",
            "kt" | "kts" => "Kotlin",
            "swift" => "Swift",
            "slint" => "Slint",
            "toml" => "TOML",
            "json" => "JSON",
            "jsonl" => "JSON Lines",
            "md" | "markdown" => "Markdown",
            "html" | "htm" => "HTML",
            "css" => "CSS",
            "scss" => "SCSS",
            "sass" => "Sass",
            "less" => "LESS",
            "sql" => "SQL",
            "go" => "Go",
            "java" => "Java",
            "cs" => "C#",
            "rb" => "Ruby",
            "php" => "PHP",
            "py" => "Python",
            "rs" => "Rust",
            "yaml" | "yml" => "YAML",
            "sh" | "bash" | "zsh" | "fish" => "Shell",
            "ps1" | "psm1" | "psd1" => "PowerShell",
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "C++",
            "c" | "h" => "C",
            "cmake" => "CMake",
            "env" => "DotENV",
            "tf" | "tfvars" => "Terraform",
            "nix" => "Nix",
            "zig" | "zon" => "Zig",
            "rspec" => "RSpec",
            "lua" => "Lua",
            "dart" => "Dart",
            "ex" | "exs" => "Elixir",
            "erl" | "hrl" => "Erlang",
            "fs" | "fsi" | "fsx" => "F#",
            "hs" | "lhs" => "Haskell",
            "jl" => "Julia",
            "nim" => "Nim",
            "r" => "R",
            "scala" | "sc" => "Scala",
            "sol" => "Solidity",
            "proto" => "Protocol Buffer",
            "graphql" | "gql" => "GraphQL",
            "wgsl" => "WGSL",
            "xml" | "xsd" | "xsl" => "XML",
            "ini" | "conf" | "cfg" => "Config",
            _ => return None,
        };
        return Some(label.to_string());
    }

    let trimmed = text.trim_start();
    if trimmed.starts_with("#!/") && trimmed.lines().next().unwrap_or_default().contains("sh") {
        return Some("Shell".to_string());
    }
    None
}

fn normalized_label(name: &str) -> String {
    match name {
        "JavaScript (Babel)" => "JavaScript".to_string(),
        "TypescriptReact" => "TypeScript".to_string(),
        "TypeScript" => "TypeScript".to_string(),
        "Bourne Again Shell (bash)" => "Shell".to_string(),
        "C++" => "C++".to_string(),
        "C" => "C".to_string(),
        "CMake" => "CMake".to_string(),
        "HTML" => "HTML".to_string(),
        "CSS" => "CSS".to_string(),
        "SCSS" => "SCSS".to_string(),
        "Sass" => "Sass".to_string(),
        "LESS" => "LESS".to_string(),
        "YAML" => "YAML".to_string(),
        "TOML" => "TOML".to_string(),
        "JSON" => "JSON".to_string(),
        "DotENV" => "DotENV".to_string(),
        "Dockerfile" => "Dockerfile".to_string(),
        "PHP" => "PHP".to_string(),
        "SQL" => "SQL".to_string(),
        "Markdown" => "Markdown".to_string(),
        "Plain Text" => PLAIN_TEXT.to_string(),
        other => other.to_string(),
    }
}

fn is_plain_text(syntax: &SyntaxReference) -> bool {
    syntax.name == PLAIN_TEXT
}

fn highlight_with_syntax(
    syntax: &SyntaxReference,
    text: &str,
    max_bytes: usize,
    theme_mode: HighlightTheme,
) -> Vec<SyntaxSpan> {
    let max = clamp_to_char_boundary(text, max_bytes.min(text.len()));
    let mut highlighter = HighlightLines::new(syntax, theme(theme_mode));
    let mut spans = Vec::new();
    let mut absolute = 0usize;

    for line in LinesWithEndings::from(&text[..max]) {
        let Ok(regions) = highlighter.highlight_line(line, syntax_set()) else {
            break;
        };
        let mut line_offset = 0usize;
        for (style, slice) in regions {
            let start = absolute + line_offset;
            let end = start + slice.len();
            line_offset += slice.len();
            if start == end || slice.trim().is_empty() {
                continue;
            }
            spans.push(SyntaxSpan {
                start_byte: start,
                end_byte: end,
                class_name: class_name(&style).to_string(),
                foreground: Some(color_hex(style.foreground)),
                bold: style.font_style.contains(FontStyle::BOLD),
                italic: style.font_style.contains(FontStyle::ITALIC),
            });
        }
        absolute += line.len();
        if absolute >= max {
            break;
        }
    }

    coalesce_spans(spans)
}

fn class_name(style: &Style) -> &'static str {
    if style.font_style.contains(FontStyle::ITALIC) {
        "comment"
    } else {
        "token"
    }
}

fn color_hex(color: syntect::highlighting::Color) -> String {
    format!("#{:02X}{:02X}{:02X}", color.r, color.g, color.b)
}

fn clamp_to_char_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn coalesce_spans(spans: Vec<SyntaxSpan>) -> Vec<SyntaxSpan> {
    let mut output: Vec<SyntaxSpan> = Vec::new();
    for span in spans {
        if let Some(previous) = output.last_mut()
            && previous.end_byte == span.start_byte
            && previous.class_name == span.class_name
            && previous.foreground == span.foreground
            && previous.bold == span.bold
            && previous.italic == span.italic
        {
            previous.end_byte = span.end_byte;
            continue;
        }
        output.push(span);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_languages_from_paths() {
        let cases = [
            ("main.rs", "fn main() {}", "Rust"),
            ("tool.py", "print('hi')", "Python"),
            ("app.js", "const x = 1;", "JavaScript"),
            ("app.ts", "let x: number = 1;", "TypeScript"),
            ("index.html", "<main></main>", "HTML"),
            ("style.css", "body { color: red; }", "CSS"),
            ("data.json", "{\"a\":1}", "JSON"),
            ("config.yaml", "a: 1", "YAML"),
            ("Cargo.toml", "[package]", "TOML"),
            ("README.md", "# Hi", "Markdown"),
            ("script.sh", "#!/usr/bin/env bash\necho hi", "Shell"),
            ("query.sql", "select * from users", "SQL"),
            ("main.go", "package main", "Go"),
            ("main.c", "int main() { return 0; }", "C"),
            ("main.cpp", "int main() { return 0; }", "C++"),
            ("Main.java", "class Main {}", "Java"),
            ("app.rb", "puts 'hi'", "Ruby"),
            ("index.php", "<?php echo 'hi';", "PHP"),
            ("App.swift", "let name = \"Texti\"", "Swift"),
            ("Main.kt", "fun main() {}", "Kotlin"),
            (
                "App.tsx",
                "export const App = () => <main />;",
                "TypeScript",
            ),
            ("App.vue", "<template><main /></template>", "Vue"),
            (
                "App.svelte",
                "<script>let name = 'Texti';</script>",
                "Svelte",
            ),
            ("main.tf", "resource \"x\" \"y\" {}", "Terraform"),
            ("flake.nix", "{ pkgs }: pkgs.hello", "Nix"),
            ("main.zig", "pub fn main() void {}", "Zig"),
            ("Dockerfile", "FROM scratch", "Dockerfile"),
            (".env.local", "TEXTI=1", "DotENV"),
            (
                "CMakeLists.txt",
                "cmake_minimum_required(VERSION 3.20)",
                "CMake",
            ),
        ];

        for (path, text, expected) in cases {
            let detected = SyntaxService::detect(Some(Path::new(path)), text);
            assert_eq!(detected.label(), expected, "{path}");
        }
    }

    #[test]
    fn unknown_files_fall_back_to_plain_text() {
        let detected = SyntaxService::detect(Some(Path::new("notes.unknown-ext")), "hello");
        assert_eq!(detected.label(), PLAIN_TEXT);
        assert!(detected.is_plain_text());
        assert!(
            SyntaxService::highlight(
                Some(Path::new("notes.unknown-ext")),
                "hello",
                1024,
                HighlightTheme::Dark,
            )
            .is_empty()
        );
    }

    #[test]
    fn shebang_can_detect_shell_without_extension() {
        let detected = SyntaxService::detect(None, "#!/usr/bin/env bash\necho hi");
        assert_eq!(detected.label(), "Shell");
    }

    #[test]
    fn produces_render_ready_highlight_spans() {
        let spans = SyntaxService::highlight(
            Some(Path::new("main.rs")),
            "fn main() {\n    let x = 1;\n}",
            4096,
            HighlightTheme::Dark,
        );
        assert!(spans.iter().any(|span| span.foreground.is_some()));
        assert!(spans.iter().all(|span| span.start_byte < span.end_byte));
    }

    #[test]
    fn highlights_extra_syntax_pack_languages() {
        let cases = [
            ("App.tsx", "export const App = () => <main />;"),
            ("App.vue", "<template><main /></template>"),
            ("App.svelte", "<script>let name = 'Texti';</script>"),
            ("main.tf", "resource \"texti_file\" \"demo\" {}"),
            ("flake.nix", "{ pkgs }: pkgs.hello"),
            ("main.zig", "pub fn main() void {}"),
            ("Dockerfile", "FROM scratch\nCOPY . /app"),
            (".env.local", "TEXTI_MODE=preview"),
            ("CMakeLists.txt", "cmake_minimum_required(VERSION 3.20)"),
        ];

        for (path, text) in cases {
            let spans =
                SyntaxService::highlight(Some(Path::new(path)), text, 4096, HighlightTheme::Dark);
            assert!(!spans.is_empty(), "{path}");
        }
    }

    #[test]
    fn slint_files_use_the_bundled_grammar_and_produce_spans() {
        let text = "export component MainWindow inherits Window {\n    property <string> title: \"Texti\";\n    // A real Slint comment\n}";
        let detected = SyntaxService::detect(Some(Path::new("main.slint")), text);
        let spans = SyntaxService::highlight(
            Some(Path::new("main.slint")),
            text,
            text.len(),
            HighlightTheme::Dark,
        );

        assert_eq!(detected.label(), "Slint");
        assert!(!detected.is_plain_text());
        assert!(!spans.is_empty());
        assert!(spans.iter().any(|span| {
            &text[span.start_byte..span.end_byte] == "export"
                && span
                    .foreground
                    .as_deref()
                    .is_some_and(|color| color != "#C0C5CE")
        }));
    }

    #[test]
    fn light_and_dark_themes_produce_distinct_contrasting_colors() {
        let text = "fn main() { let value = \"Texti\"; }";
        let dark = SyntaxService::highlight(
            Some(Path::new("main.rs")),
            text,
            text.len(),
            HighlightTheme::Dark,
        );
        let light = SyntaxService::highlight(
            Some(Path::new("main.rs")),
            text,
            text.len(),
            HighlightTheme::Light,
        );

        assert_eq!(span_ranges(&dark), span_ranges(&light));
        assert_ne!(span_colors(&dark), span_colors(&light));

        let dark_default =
            color_luminance(theme(HighlightTheme::Dark).settings.foreground.unwrap());
        let light_default =
            color_luminance(theme(HighlightTheme::Light).settings.foreground.unwrap());
        assert!(dark_default > 0.5, "dark-theme text should be light");
        assert!(light_default < 0.5, "light-theme text should be dark");
    }

    #[test]
    fn legacy_syntax_modes_do_not_override_auto_detection() {
        let detected = SyntaxService::detect_with_mode(
            SyntaxMode::PlainText,
            Some(Path::new("main.rs")),
            "fn main() {}",
        );
        assert_eq!(detected.label(), "Rust");
        assert!(
            !SyntaxService::highlight_with_mode(
                SyntaxMode::PlainText,
                Some(Path::new("main.rs")),
                "fn main() {}",
                4096,
                HighlightTheme::Dark,
            )
            .is_empty()
        );
    }

    fn span_ranges(spans: &[SyntaxSpan]) -> Vec<(usize, usize)> {
        spans
            .iter()
            .map(|span| (span.start_byte, span.end_byte))
            .collect()
    }

    fn span_colors(spans: &[SyntaxSpan]) -> Vec<Option<&str>> {
        spans
            .iter()
            .map(|span| span.foreground.as_deref())
            .collect()
    }

    fn color_luminance(color: syntect::highlighting::Color) -> f32 {
        (0.2126 * f32::from(color.r) + 0.7152 * f32::from(color.g) + 0.0722 * f32::from(color.b))
            / 255.0
    }
}
