#![forbid(unsafe_code)]
//! Validates the source catalogs and generates the compile-time Rust and
//! TypeScript catalogs. `SSR_I18N_BLESS=1` updates committed generated files.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use toml::Value;

const BLESS: &str = "SSR_I18N_BLESS";
const RUST_TABLES: [&str; 10] = [
    "names",
    "common",
    "supervisor",
    "guard",
    "shell",
    "cli",
    "live",
    "replay",
    "setup",
    "testkit",
];

// Strict and reserved keywords across Rust editions, including Rust 2024's gen.
const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "gen", "macro",
    "override", "priv", "try", "typeof", "unsized", "virtual", "yield",
];
// JS/TS keywords and restricted strict-mode bindings cannot be arrow parameters.
const TS_KEYWORDS: &[&str] = &[
    "abstract",
    "any",
    "as",
    "asserts",
    "async",
    "await",
    "bigint",
    "boolean",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "constructor",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "global",
    "if",
    "implements",
    "import",
    "in",
    "infer",
    "instanceof",
    "interface",
    "intrinsic",
    "is",
    "keyof",
    "let",
    "module",
    "namespace",
    "never",
    "new",
    "null",
    "number",
    "object",
    "of",
    "out",
    "override",
    "package",
    "private",
    "protected",
    "public",
    "readonly",
    "require",
    "return",
    "satisfies",
    "set",
    "static",
    "string",
    "super",
    "switch",
    "symbol",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "unique",
    "unknown",
    "using",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "arguments",
    "eval",
];

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Literal(String),
    Placeholder(String),
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_catalog(language: &str) -> Value {
    let path = root().join(format!("i18n/{language}.toml"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    toml::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn check_or_bless_bytes(path: &Path, content: &[u8]) {
    if std::env::var_os(BLESS).is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
        return;
    }
    let existing = std::fs::read(path)
        .unwrap_or_else(|error| panic!("{}: {error}; run with {BLESS}=1", path.display()));
    assert_eq!(
        existing,
        content,
        "{} is out of date; run `SSR_I18N_BLESS=1 cargo test -p ssr-i18n`",
        path.display()
    );
}

fn check_or_bless(path: &Path, content: &str) {
    check_or_bless_bytes(path, content.as_bytes());
}

fn sorted_table(value: &Value) -> BTreeMap<&str, &Value> {
    value
        .as_table()
        .expect("catalog node must be a table")
        .iter()
        .map(|(key, value)| (key.as_str(), value))
        .collect()
}

fn validate_pair(path: &str, ko: &Value, en: &Value) {
    match (ko, en) {
        (Value::String(ko), Value::String(en)) => {
            for (language, text) in [("ko", ko), ("en", en)] {
                assert!(
                    !text.trim().is_empty(),
                    "{path} ({language}): empty or whitespace-only string"
                );
            }
            let ko_tokens = tokenize(ko, path);
            let en_tokens = tokenize(en, path);
            let ko_placeholders = placeholders(&ko_tokens);
            let en_placeholders = placeholders(&en_tokens);
            assert_eq!(
                ko_placeholders.iter().collect::<BTreeSet<_>>(),
                en_placeholders.iter().collect::<BTreeSet<_>>(),
                "placeholder set differs at {path}"
            );
        }
        (Value::Table(_), Value::Table(_)) => {
            let ko = sorted_table(ko);
            let en = sorted_table(en);
            assert_eq!(
                ko.keys().collect::<Vec<_>>(),
                en.keys().collect::<Vec<_>>(),
                "key tree differs at {path}"
            );
            for (key, ko_value) in ko {
                validate_pair(&format!("{path}.{key}"), ko_value, en[key]);
            }
        }
        (Value::Array(ko), Value::Array(en)) => {
            assert_eq!(ko.len(), en.len(), "array length differs at {path}");
            for (index, (ko_value, en_value)) in ko.iter().zip(en).enumerate() {
                validate_pair(&format!("{path}[{index}]"), ko_value, en_value);
            }
        }
        _ => panic!("value type differs or is unsupported at {path}"),
    }
}

fn valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    name != "_"
        && chars
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn tokenize(text: &str, path: &str) -> Vec<Token> {
    let mut chars = text.chars().peekable();
    let mut tokens = Vec::new();
    let mut literal = String::new();
    while let Some(ch) = chars.next() {
        match ch {
            '{' | '}' if chars.peek() == Some(&ch) => {
                chars.next();
                literal.push(ch);
            }
            '{' => {
                if !literal.is_empty() {
                    tokens.push(Token::Literal(std::mem::take(&mut literal)));
                }
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some('{') => panic!("{path}: nested opening brace in placeholder"),
                        Some(ch) => name.push(ch),
                        None => panic!("{path}: unmatched opening brace"),
                    }
                }
                assert!(
                    valid_identifier(&name),
                    "{path}: invalid placeholder identifier {{{name}}}"
                );
                assert!(
                    !RUST_KEYWORDS.contains(&name.as_str())
                        && !TS_KEYWORDS.contains(&name.as_str()),
                    "{path}: reserved Rust/TypeScript placeholder {{{name}}}"
                );
                tokens.push(Token::Placeholder(name));
            }
            '}' => panic!("{path}: unmatched closing brace"),
            ch => literal.push(ch),
        }
    }
    if !literal.is_empty() {
        tokens.push(Token::Literal(literal));
    }
    tokens
}

fn placeholders(tokens: &[Token]) -> Vec<&str> {
    let mut names = Vec::new();
    for token in tokens {
        if let Token::Placeholder(name) = token
            && !names.contains(&name.as_str())
        {
            names.push(name.as_str());
        }
    }
    names
}

fn decoded_literal(tokens: &[Token]) -> String {
    tokens
        .iter()
        .map(|token| match token {
            Token::Literal(text) => text.as_str(),
            Token::Placeholder(_) => panic!("decoded_literal requires no placeholders"),
        })
        .collect()
}

fn rust_format(tokens: &[Token]) -> String {
    let mut format = String::new();
    for token in tokens {
        match token {
            Token::Literal(text) => format.push_str(&text.replace('{', "{{").replace('}', "}}")),
            Token::Placeholder(name) => write!(format, "{{{name}}}").unwrap(),
        }
    }
    format
}

fn validate_rust_key(key: &str, path: &str) {
    assert!(valid_identifier(key), "{path}: invalid Rust identifier");
    assert!(
        !RUST_KEYWORDS.contains(&key),
        "{path}: reserved Rust keyword"
    );
    assert!(
        key.bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
        "{path}: Rust key is not snake_case"
    );
}

fn validate_catalogs(ko: &Value, en: &Value) {
    validate_pair("catalog", ko, en);
    validate_endpoint_names(ko, "ko");
    validate_endpoint_names(en, "en");
    let english = en.to_string();
    assert!(
        !english
            .chars()
            .any(|ch| matches!(ch as u32, 0xAC00..=0xD7A3 | 0x1100..=0x11FF | 0x3130..=0x318F)),
        "English catalog contains Korean characters"
    );
    for table in RUST_TABLES {
        for (key, value) in sorted_table(&ko[table]) {
            validate_rust_key(key, &format!("catalog.{table}.{key}"));
            assert!(value.is_str(), "{table}.{key} must be a flat string");
        }
    }
}

fn validate_endpoint_names(catalog: &Value, language: &str) {
    for key in ["endpoint_main", "endpoint_music", "endpoint_pass"] {
        let text = catalog["names"][key].as_str().unwrap();
        assert!(
            text.encode_utf16().count() <= 31,
            "catalog.names.{key} ({language}): exceeds 31 UTF-16 code units"
        );
    }
}

fn format_rust(source: &str) -> String {
    let mut child = Command::new("rustfmt")
        .args(["--edition", "2024"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("rustfmt must be available to generate catalog.rs");
    use std::io::Write as _;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(source.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "rustfmt failed while generating catalog.rs"
    );
    String::from_utf8(output.stdout).unwrap()
}

fn rust_catalog(ko: &Value, en: &Value) -> String {
    let mut output = String::from(
        "// Generated from i18n/*.toml. Do not edit by hand.\n// Regenerate with `SSR_I18N_BLESS=1 cargo test -p ssr-i18n`.\n\nuse ssr_types::app::Language;\n\n",
    );
    for table_name in RUST_TABLES {
        writeln!(output, "pub mod {table_name} {{").unwrap();
        if !sorted_table(&ko[table_name]).is_empty() {
            output.push_str("    use super::Language;\n\n");
        }
        for (key, ko_value) in sorted_table(&ko[table_name]) {
            let ko_text = ko_value.as_str().unwrap();
            let en_text = en[table_name][key].as_str().unwrap();
            let path = format!("catalog.{table_name}.{key}");
            let ko_tokens = tokenize(ko_text, &path);
            let en_tokens = tokenize(en_text, &path);
            let args = placeholders(&ko_tokens);
            // Keep the language selector distinct from any catalog parameter.
            let mut selector = String::from("language");
            while args.contains(&selector.as_str()) {
                selector.push('_');
            }
            writeln!(output, "    #[doc = {ko_text:?}]").unwrap();
            if args.is_empty() {
                writeln!(
                    output,
                    "    pub fn {key}(language: Language) -> &'static str {{"
                )
                .unwrap();
                writeln!(output, "        match language {{").unwrap();
                writeln!(
                    output,
                    "            Language::Ko => {:?},",
                    decoded_literal(&ko_tokens)
                )
                .unwrap();
                writeln!(
                    output,
                    "            Language::En => {:?},",
                    decoded_literal(&en_tokens)
                )
                .unwrap();
                writeln!(output, "        }}\n    }}\n").unwrap();
            } else {
                if args.len() + 1 > 7 {
                    writeln!(output, "    #[allow(clippy::too_many_arguments)]").unwrap();
                }
                write!(output, "    pub fn {key}({selector}: Language").unwrap();
                for arg in &args {
                    write!(output, ", {arg}: impl core::fmt::Display").unwrap();
                }
                writeln!(output, ") -> String {{").unwrap();
                writeln!(output, "        match {selector} {{").unwrap();
                writeln!(
                    output,
                    "            Language::Ko => format!({:?}),",
                    rust_format(&ko_tokens)
                )
                .unwrap();
                writeln!(
                    output,
                    "            Language::En => format!({:?}),",
                    rust_format(&en_tokens)
                )
                .unwrap();
                writeln!(output, "        }}\n    }}\n").unwrap();
            }
        }
        output.push_str("}\n\n");
    }
    format_rust(&output)
}

fn powershell_catalog(ko: &Value, en: &Value) -> Vec<u8> {
    fn quote(text: &str) -> String {
        format!("'{}'", text.replace('\'', "''"))
    }

    let mut output = String::from("@{\n");
    for (language, catalog) in [("ko", ko), ("en", en)] {
        writeln!(output, "  {language} = @{{").unwrap();
        for (key, value) in sorted_table(&catalog["testkit"]) {
            writeln!(output, "    {key} = {}", quote(value.as_str().unwrap())).unwrap();
        }
        output.push_str("  }\n");
    }
    output.push_str("}\n");
    let mut bytes = vec![0xef, 0xbb, 0xbf];
    bytes.extend_from_slice(output.as_bytes());
    bytes
}

fn ts_catalog(ko: &Value, en: &Value) -> String {
    let mut output = String::from(
        "// Generated from i18n/*.toml. Do not edit by hand.\n// Regenerate with `SSR_I18N_BLESS=1 cargo test -p ssr-i18n`.\n\n",
    );
    output.push_str("export const ko = ");
    write_ts_value(&mut output, ko, ko, 0, "catalog");
    output.push_str(";\n\nexport type Catalog = typeof ko;\n\nexport const en: Catalog = ");
    write_ts_value(&mut output, en, ko, 0, "catalog");
    output.push_str(";\n");
    output
}

fn write_ts_value(output: &mut String, value: &Value, ko_value: &Value, indent: usize, path: &str) {
    match value {
        Value::String(text) => {
            let ko_text = ko_value.as_str().expect("translation shape was validated");
            let ko_tokens = tokenize(ko_text, path);
            let tokens = tokenize(text, path);
            let args = placeholders(&ko_tokens);
            if args.is_empty() {
                write!(output, "{}", ts_string(&decoded_literal(&tokens))).unwrap();
            } else {
                output.push('(');
                for (index, arg) in args.iter().enumerate() {
                    if index != 0 {
                        output.push_str(", ");
                    }
                    write!(output, "{arg}: string | number").unwrap();
                }
                output.push_str(") => `");
                write_ts_template(output, &tokens);
                output.push('`');
            }
        }
        Value::Table(_) => {
            output.push_str("{\n");
            let ko_table = sorted_table(ko_value);
            for (key, child) in sorted_table(value) {
                output.push_str(&"  ".repeat(indent + 1));
                write!(output, "{}: ", ts_string(key)).unwrap();
                write_ts_value(
                    output,
                    child,
                    ko_table[key],
                    indent + 1,
                    &format!("{path}.{key}"),
                );
                output.push_str(",\n");
            }
            output.push_str(&"  ".repeat(indent));
            output.push('}');
        }
        Value::Array(values) => {
            if values.is_empty() {
                output.push_str("[]");
            } else {
                output.push_str("[\n");
                let ko_values = ko_value.as_array().unwrap();
                for (index, child) in values.iter().enumerate() {
                    output.push_str(&"  ".repeat(indent + 1));
                    write_ts_value(
                        output,
                        child,
                        &ko_values[index],
                        indent + 1,
                        &format!("{path}[{index}]"),
                    );
                    output.push_str(",\n");
                }
                output.push_str(&"  ".repeat(indent));
                output.push(']');
            }
        }
        _ => panic!("unsupported catalog value at {path}"),
    }
}

fn ts_string(text: &str) -> String {
    let mut escaped = String::from("\"");
    for ch in text.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => write!(escaped, "\\u{{{:x}}}", ch as u32).unwrap(),
            ch => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

fn write_ts_template(output: &mut String, tokens: &[Token]) {
    for token in tokens {
        match token {
            Token::Literal(text) => write_ts_template_literal(output, text),
            Token::Placeholder(name) => write!(output, "${{{name}}}").unwrap(),
        }
    }
}

fn write_ts_template_literal(output: &mut String, literal: &str) {
    for ch in literal.chars() {
        match ch {
            '\\' => output.push_str("\\\\"),
            '`' => output.push_str("\\`"),
            '$' => output.push_str("\\$"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            ch => output.push(ch),
        }
    }
}

fn assert_rejected(action: impl FnOnce() + std::panic::UnwindSafe, reason: &str) {
    let error = std::panic::catch_unwind(action).expect_err("sample should be rejected");
    let message = error
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| error.downcast_ref::<&str>().copied())
        .unwrap();
    assert!(message.contains("sample"), "missing key path: {message}");
    assert!(
        message.contains(reason),
        "missing reason {reason}: {message}"
    );
}

#[test]
fn repeated_placeholders_share_one_argument() {
    let tokens = tokenize("{name}/{name}/{count}", "sample");
    assert_eq!(placeholders(&tokens), ["name", "count"]);
    assert_eq!(rust_format(&tokens), "{name}/{name}/{count}");
    let mut ts = String::new();
    write_ts_template(&mut ts, &tokens);
    assert_eq!(ts, "${name}/${name}/${count}");
}

#[test]
fn literal_braces_are_decoded_by_both_generators() {
    let tokens = tokenize("{{name}} {{}}", "sample");
    assert!(placeholders(&tokens).is_empty());
    assert_eq!(decoded_literal(&tokens), "{name} {}");
    assert_eq!(rust_format(&tokens), "{{name}} {{}}");
    let tokens = tokenize("{{{name}}}", "sample");
    assert_eq!(rust_format(&tokens), "{{{name}}}");
    let mut ts = String::new();
    write_ts_template(&mut ts, &tokens);
    assert_eq!(ts, "{${name}}");
    let value = Value::String("{{name}} {{}}".into());
    let mut ts = String::new();
    write_ts_value(&mut ts, &value, &value, 0, "sample");
    assert_eq!(ts, "\"{name} {}\"");
}

#[test]
fn dollars_and_template_literals_are_escaped() {
    let tokens = tokenize("${{name}} ${name} ` \\", "sample");
    assert_eq!(rust_format(&tokens), "${{name}} ${name} ` \\");
    let mut ts = String::new();
    write_ts_template(&mut ts, &tokens);
    assert_eq!(ts, "\\${name} \\$${name} \\` \\\\");
}

#[test]
fn unmatched_braces_are_rejected() {
    for text in ["{", "}", "a{name", "a}", "{{}", "{name}}", "{na{me}"] {
        assert_rejected(
            || {
                tokenize(text, "sample");
            },
            "brace",
        );
    }
}

#[test]
fn identifiers_and_keywords_are_validated() {
    for name in ["1key", "_", "a-b", ""] {
        assert_rejected(|| validate_rust_key(name, "sample"), "identifier");
        assert_rejected(
            || {
                tokenize(&format!("{{{name}}}"), "sample");
            },
            "identifier",
        );
    }
    for name in RUST_KEYWORDS {
        assert_rejected(
            || validate_rust_key(name, "sample"),
            "reserved Rust keyword",
        );
    }
    for name in RUST_KEYWORDS.iter().chain(TS_KEYWORDS) {
        assert_rejected(
            || {
                tokenize(&format!("{{{name}}}"), "sample");
            },
            "reserved",
        );
    }
    validate_rust_key("valid_2", "sample");
    assert_eq!(placeholders(&tokenize("{valid_2}", "sample")), ["valid_2"]);
}

#[test]
fn empty_strings_are_rejected_at_every_leaf() {
    for text in ["", " \t\n", "\u{2003}"] {
        let value = Value::Array(vec![Value::String(text.into())]);
        assert_rejected(|| validate_pair("sample", &value, &value), "empty");
    }
    let value = Value::String(" leading and trailing ".into());
    validate_pair("sample", &value, &value);
}

#[test]
fn endpoint_names_count_utf16_units() {
    let mut catalog: Value = toml::from_str(
        "[names]\nendpoint_main = 'main'\nendpoint_music = 'music'\nendpoint_pass = 'pass'",
    )
    .unwrap();
    for key in ["endpoint_main", "endpoint_music", "endpoint_pass"] {
        catalog["names"][key] = Value::String(format!("{}a", "😀".repeat(15)));
        validate_endpoint_names(&catalog, "sample");
        catalog["names"][key] = Value::String("😀".repeat(16));
        assert_rejected(|| validate_endpoint_names(&catalog, "sample"), "31 UTF-16");
        catalog["names"][key] = Value::String("a".repeat(31));
        validate_endpoint_names(&catalog, "sample");
        catalog["names"][key] = Value::String("a".repeat(32));
        assert_rejected(|| validate_endpoint_names(&catalog, "sample"), "31 UTF-16");
        catalog["names"][key] = Value::String("name".into());
    }
}

#[test]
fn multiline_docs_and_selector_collisions_generate_safe_rust() {
    let mut catalog = Value::Table(
        RUST_TABLES
            .into_iter()
            .map(|table| (table.to_owned(), Value::Table(toml::map::Map::new())))
            .collect(),
    );
    let common = catalog["common"].as_table_mut().unwrap();
    common.insert(
        "sample".into(),
        Value::String("first\n\"quoted\" \\ second {language} {language_}".into()),
    );
    common.insert("literal".into(), Value::String("{{name}}".into()));
    let generated = rust_catalog(&catalog, &catalog);
    assert!(
        generated
            .contains("#[doc = \"first\\n\\\"quoted\\\" \\\\ second {language} {language_}\"]")
    );
    assert!(generated.contains("language__: Language"));
    assert!(generated.contains("match language__"));
    assert!(generated.contains("Language::Ko => \"{name}\""));
}

#[test]
fn powershell_catalog_has_a_bom_equal_keys_and_escaped_apostrophes() {
    let ko: Value = toml::from_str("[testkit]\nsample = \"사용자의 {name}\"").unwrap();
    let en: Value = toml::from_str("[testkit]\nsample = \"user's {name}\"").unwrap();
    validate_pair("catalog", &ko, &en);
    let generated = powershell_catalog(&ko, &en);
    assert!(generated.starts_with(&[0xef, 0xbb, 0xbf]));
    let text = std::str::from_utf8(&generated[3..]).unwrap();
    assert!(text.contains("  ko = @{\n    sample = '사용자의 {name}'\n  }"));
    assert!(text.contains("  en = @{\n    sample = 'user''s {name}'\n  }"));
}

#[test]
fn typescript_object_keys_are_always_quoted() {
    let value: Value =
        toml::from_str("[screen]\n'a-b' = 'text'\n'1key' = 'other'\n'class' = 'label'").unwrap();
    let generated = ts_catalog(&value, &value);
    for key in ["screen", "a-b", "1key", "class"] {
        assert!(generated.contains(&format!("{}: ", ts_string(key))));
    }
}

#[test]
fn catalog_files_match_sources() {
    let ko = read_catalog("ko");
    let en = read_catalog("en");
    validate_catalogs(&ko, &en);
    check_or_bless(
        &root().join("crates/ssr-i18n/src/catalog.rs"),
        &rust_catalog(&ko, &en),
    );
    check_or_bless(
        &root().join("apps/spatial-softreceiver/ui/src/catalog.ts"),
        &ts_catalog(&ko, &en),
    );
    check_or_bless_bytes(
        &root().join("tools/testkit/strings.psd1"),
        &powershell_catalog(&ko, &en),
    );
}

fn inf_strings(text: &str, section: &str) -> BTreeMap<String, String> {
    let mut current = "";
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            current = &line[1..line.len() - 1];
        } else if current.eq_ignore_ascii_case(section)
            && !line.starts_with(';')
            && let Some((key, value)) = line.split_once('=')
        {
            let value = value.trim();
            if let Some(value) = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
            {
                values.insert(key.trim().to_owned(), value.to_owned());
            }
        }
    }
    values
}

#[test]
fn inf_names_match_catalog() {
    let ko = read_catalog("ko");
    let en = read_catalog("en");
    let path = root().join("driver/spatialsoftreceiver/spatialsoftreceiver.inx");
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        bytes.starts_with(&[0xff, 0xfe]),
        "the INF must remain UTF-16LE with a BOM"
    );
    assert_eq!(
        bytes.len() % 2,
        0,
        "the INF must contain complete UTF-16 code units"
    );
    let (pairs, remainder) = bytes[2..].as_chunks::<2>();
    assert!(remainder.is_empty());
    let units = pairs
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect::<Vec<_>>();
    let text = String::from_utf16(&units).expect("the INF must contain valid UTF-16LE");
    let mappings = [
        ("DiskName", "driver_disk"),
        ("SpatialSoftReceiver.DeviceDesc", "driver_device"),
        ("SpatialSoftReceiver.ServiceDesc", "driver_service"),
        ("Endpoint0Name", "endpoint_main"),
        ("Endpoint1Name", "endpoint_music"),
        ("Endpoint2Name", "endpoint_pass"),
    ];
    for (section, catalog) in [("Strings", &en), ("Strings.0412", &ko)] {
        let inf = inf_strings(&text, section);
        for (inf_key, catalog_key) in mappings {
            assert_eq!(
                inf[inf_key],
                catalog["names"][catalog_key].as_str().unwrap(),
                "{section}.{inf_key}"
            );
        }
    }
}

#[test]
fn typescript_screen_shapes_are_generated() {
    let ko: Value = toml::from_str(
        r#"
[screen]
title = "제목"
[screen.panel]
items = ["하나", "둘"]
rows = [{ name = "이름", purpose = "용도 {count}" }]
"#,
    )
    .unwrap();
    let en: Value = toml::from_str(
        r#"
[screen]
title = "Title"
[screen.panel]
items = ["One", "Two"]
rows = [{ name = "Name", purpose = "Purpose {count}" }]
"#,
    )
    .unwrap();
    validate_pair("catalog", &ko, &en);
    let generated = ts_catalog(&ko, &en);
    assert!(generated.contains("\"items\": ["));
    assert!(generated.contains("\"하나\""));
    assert!(generated.contains("\"둘\""));
    assert!(generated.contains("\"purpose\": (count: string | number) => `Purpose ${count}`"));
    assert!(generated.contains("export const en: Catalog"));
}
