#![forbid(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

//! Workspace policy gates. These tests inspect the repository tree directly so
//! build flags and conditional compilation cannot hide policy violations.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Clone)]
struct FirstPartyCrate {
    name: String,
    dir: PathBuf,
    manifest: PathBuf,
    is_app: bool,
}

#[derive(Deserialize)]
struct Budget {
    budget: BTreeMap<String, usize>,
}

#[derive(Deserialize)]
struct Feature {
    id: String,
    kind: String,
    milestone: String,
    status: String,
    tests: Vec<String>,
}

#[derive(Deserialize)]
struct Registry {
    feature: Vec<Feature>,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root must be accessible")
}

fn read_toml(path: &Path) -> toml::Value {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{}: failed to read TOML: {error}", path.display()));
    toml::from_str(&text)
        .unwrap_or_else(|error| panic!("{}: failed to parse TOML: {error}", path.display()))
}

fn relative_path(path: &Path) -> String {
    path.strip_prefix(repo_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn first_party_crates() -> Vec<FirstPartyCrate> {
    let root = repo_root();
    let mut crates = Vec::new();

    for parent in ["crates", "apps"] {
        let parent_dir = root.join(parent);
        let entries = fs::read_dir(&parent_dir)
            .unwrap_or_else(|error| panic!("{}: {error}", parent_dir.display()));
        for entry in entries {
            let entry = entry.unwrap_or_else(|error| {
                panic!(
                    "{}: failed to read directory entry: {error}",
                    parent_dir.display()
                )
            });
            let dir = entry.path();
            let manifest = dir.join("Cargo.toml");
            if !manifest.is_file() {
                continue;
            }
            let parsed = read_toml(&manifest);
            let name = parsed
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
                .unwrap_or_else(|| panic!("{}: missing [package] name", manifest.display()))
                .to_owned();
            crates.push(FirstPartyCrate {
                name,
                dir,
                manifest,
                is_app: parent == "apps",
            });
        }
    }

    crates.sort_by(|left, right| left.name.cmp(&right.name));
    assert!(
        !crates.is_empty(),
        "no first-party crates found under crates/ or apps/"
    );
    crates
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        let entries =
            fs::read_dir(&current).unwrap_or_else(|error| panic!("{}: {error}", current.display()));
        for entry in entries {
            let entry = entry.unwrap_or_else(|error| {
                panic!(
                    "{}: failed to read directory entry: {error}",
                    current.display()
                )
            });
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "target") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }

    files.sort();
    files
}

fn direct_rust_files(dir: &Path, roots: &mut Vec<PathBuf>) {
    if !dir.is_dir() {
        return;
    }
    let entries = fs::read_dir(dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| {
            panic!("{}: failed to read directory entry: {error}", dir.display())
        });
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|extension| extension == "rs") {
            roots.push(path);
        }
    }
}

fn crate_roots(dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for candidate in ["src/lib.rs", "src/main.rs", "build.rs"] {
        let path = dir.join(candidate);
        if path.is_file() {
            roots.push(path);
        }
    }
    for direct_dir in ["src/bin", "tests", "examples", "benches"] {
        direct_rust_files(&dir.join(direct_dir), &mut roots);
    }
    roots.sort();
    roots
}

fn is_test_example_or_bench(crate_dir: &Path, root: &Path) -> bool {
    root.strip_prefix(crate_dir)
        .ok()
        .and_then(|relative| relative.components().next())
        .is_some_and(|first| {
            matches!(
                first.as_os_str().to_str(),
                Some("tests" | "examples" | "benches")
            )
        })
}

fn load_budget() -> Budget {
    let path = repo_root().join("unsafe-budget.toml");
    let text =
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    toml::from_str(&text)
        .unwrap_or_else(|error| panic!("{}: failed to parse: {error}", path.display()))
}

fn load_registry() -> Registry {
    let path = repo_root().join("features.toml");
    let text =
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    toml::from_str(&text)
        .unwrap_or_else(|error| panic!("{}: failed to parse: {error}", path.display()))
}

fn push_blanks(out: &mut String, chars: &[char], start: usize, end: usize) {
    for &character in &chars[start..end] {
        if character == '\n' {
            out.push('\n');
        } else {
            out.push(' ');
        }
    }
}

fn raw_literal_end(chars: &[char], start: usize) -> Option<usize> {
    let mut cursor = if chars.get(start) == Some(&'r') {
        start + 1
    } else if matches!(chars.get(start), Some('b' | 'c')) && chars.get(start + 1) == Some(&'r') {
        start + 2
    } else {
        return None;
    };

    let hash_start = cursor;
    while chars.get(cursor) == Some(&'#') {
        cursor += 1;
    }
    let hashes = cursor - hash_start;
    if chars.get(cursor) != Some(&'"') {
        return None;
    }
    cursor += 1;

    while cursor < chars.len() {
        if chars[cursor] == '"'
            && (0..hashes).all(|offset| chars.get(cursor + 1 + offset) == Some(&'#'))
        {
            return Some(cursor + 1 + hashes);
        }
        cursor += 1;
    }
    Some(chars.len())
}

fn quoted_literal_end(chars: &[char], quote: usize) -> usize {
    let mut cursor = quote + 1;
    while cursor < chars.len() {
        match chars[cursor] {
            '\\' => cursor = (cursor + 2).min(chars.len()),
            '"' => return cursor + 1,
            _ => cursor += 1,
        }
    }
    chars.len()
}

fn char_literal_end(chars: &[char], quote: usize) -> Option<usize> {
    let mut cursor = quote + 1;
    let character = *chars.get(cursor)?;
    if character == '\n' || character == '\r' || character == '\'' {
        return None;
    }

    if character == '\\' {
        cursor += 1;
        match chars.get(cursor).copied()? {
            'u' if chars.get(cursor + 1) == Some(&'{') => {
                cursor += 2;
                while cursor < chars.len() && chars[cursor] != '}' {
                    cursor += 1;
                }
                if chars.get(cursor) != Some(&'}') {
                    return None;
                }
                cursor += 1;
            }
            'x' => {
                cursor += 3;
                if cursor > chars.len() {
                    return None;
                }
            }
            _ => cursor += 1,
        }
    } else {
        cursor += 1;
    }

    (chars.get(cursor) == Some(&'\'')).then_some(cursor + 1)
}

/// Replaces comments and Rust string, byte-string, C-string, and character
/// literals with whitespace while preserving line breaks and all code tokens.
fn strip_comments_and_literals(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut cursor = 0;

    while cursor < chars.len() {
        if chars[cursor] == '/' && chars.get(cursor + 1) == Some(&'/') {
            let start = cursor;
            cursor += 2;
            while cursor < chars.len() && chars[cursor] != '\n' {
                cursor += 1;
            }
            push_blanks(&mut out, &chars, start, cursor);
            continue;
        }

        if chars[cursor] == '/' && chars.get(cursor + 1) == Some(&'*') {
            let start = cursor;
            cursor += 2;
            let mut depth = 1_usize;
            while cursor < chars.len() && depth > 0 {
                if chars[cursor] == '/' && chars.get(cursor + 1) == Some(&'*') {
                    depth += 1;
                    cursor += 2;
                } else if chars[cursor] == '*' && chars.get(cursor + 1) == Some(&'/') {
                    depth -= 1;
                    cursor += 2;
                } else {
                    cursor += 1;
                }
            }
            push_blanks(&mut out, &chars, start, cursor);
            continue;
        }

        if let Some(end) = raw_literal_end(&chars, cursor) {
            push_blanks(&mut out, &chars, cursor, end);
            cursor = end;
            continue;
        }

        if matches!(chars[cursor], 'b' | 'c') && chars.get(cursor + 1) == Some(&'"') {
            let end = quoted_literal_end(&chars, cursor + 1);
            push_blanks(&mut out, &chars, cursor, end);
            cursor = end;
            continue;
        }

        if chars[cursor] == '"' {
            let end = quoted_literal_end(&chars, cursor);
            push_blanks(&mut out, &chars, cursor, end);
            cursor = end;
            continue;
        }

        if chars[cursor] == 'b'
            && chars.get(cursor + 1) == Some(&'\'')
            && let Some(end) = char_literal_end(&chars, cursor + 1)
        {
            push_blanks(&mut out, &chars, cursor, end);
            cursor = end;
            continue;
        }

        if chars[cursor] == '\''
            && let Some(end) = char_literal_end(&chars, cursor)
        {
            push_blanks(&mut out, &chars, cursor, end);
            cursor = end;
            continue;
        }

        out.push(chars[cursor]);
        cursor += 1;
    }

    out
}

fn is_identifier_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

fn count_unsafe_tokens(source: &str) -> usize {
    let code = strip_comments_and_literals(source);
    code.match_indices("unsafe")
        .filter(|(index, _)| {
            let before = code[..*index].chars().next_back();
            let after = code[*index + "unsafe".len()..].chars().next();
            before.is_none_or(|character| !is_identifier_character(character))
                && after.is_none_or(|character| !is_identifier_character(character))
        })
        .count()
}

#[test]
fn every_crate_root_forbids_unsafe() {
    let budget = load_budget();
    let mut violations = Vec::new();

    for krate in first_party_crates() {
        let budget_for_crate = budget.budget.get(&krate.name).copied().unwrap_or(0);
        for root in crate_roots(&krate.dir) {
            let source = fs::read_to_string(&root)
                .unwrap_or_else(|error| panic!("{}: {error}", root.display()));
            let code = strip_comments_and_literals(&source);
            let has_forbid = code.contains("#![forbid(unsafe_code)]");
            let has_deny = code.contains("#![deny(unsafe_code)]");
            // Integration tests, examples and benches are crates of their own
            // that never need the budget, so they forbid unsafe even in a
            // budgeted crate; only the library, binaries and build script may
            // carry `deny` instead.
            let allowed = if is_test_example_or_bench(&krate.dir, &root) {
                0
            } else {
                budget_for_crate
            };

            if allowed == 0 && !has_forbid {
                violations.push(format!(
                    "{} (crate {}): expected #![forbid(unsafe_code)]",
                    relative_path(&root),
                    krate.name
                ));
            } else if allowed > 0 {
                if !has_deny {
                    violations.push(format!(
                        "{} (crate {}): unsafe budget is {allowed}; expected #![deny(unsafe_code)]",
                        relative_path(&root),
                        krate.name
                    ));
                }
                if has_forbid {
                    violations.push(format!(
                        "{} (crate {}): unsafe budget is {allowed}; must not carry #![forbid(unsafe_code)]",
                        relative_path(&root),
                        krate.name
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "crate-root unsafe lint violations:\n{}",
        violations.join("\n")
    );
}

fn lint_value<'a>(manifest: &'a toml::Value, group: &str, lint: &str) -> Option<&'a str> {
    manifest.get("lints")?.get(group)?.get(lint)?.as_str()
}

#[test]
fn budgeted_crates_carry_the_unsafe_lint_table() {
    let budget = load_budget();
    let mut violations = Vec::new();

    for krate in first_party_crates() {
        let manifest = read_toml(&krate.manifest);
        let allowed = budget.budget.get(&krate.name).copied().unwrap_or(0);
        let inherits_workspace = manifest
            .get("lints")
            .and_then(|lints| lints.get("workspace"))
            .and_then(toml::Value::as_bool)
            == Some(true);

        if allowed == 0 {
            if !inherits_workspace {
                violations.push(format!(
                    "{} (crate {}): budget is 0; expected [lints] workspace = true",
                    relative_path(&krate.manifest),
                    krate.name
                ));
            }
            continue;
        }

        if inherits_workspace {
            violations.push(format!(
                "{} (crate {}): budget is {allowed}; must not inherit workspace lints",
                relative_path(&krate.manifest),
                krate.name
            ));
        }

        for (group, lint) in [
            ("rust", "unsafe_code"),
            ("rust", "unsafe_op_in_unsafe_fn"),
            ("clippy", "undocumented_unsafe_blocks"),
            ("clippy", "multiple_unsafe_ops_per_block"),
            ("clippy", "missing_safety_doc"),
        ] {
            if lint_value(&manifest, group, lint) != Some("deny") {
                violations.push(format!(
                    "{} (crate {}): budget is {allowed}; expected [lints.{group}] {lint} = \"deny\"",
                    relative_path(&krate.manifest),
                    krate.name
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "unsafe lint-table violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn no_unsafe_outside_budget() {
    let budget = load_budget();
    let mut violations = Vec::new();

    for krate in first_party_crates() {
        let allowed = budget.budget.get(&krate.name).copied().unwrap_or(0);
        let mut total = 0;
        let mut files_with_unsafe = Vec::new();

        for file in rust_files(&krate.dir) {
            let source = fs::read_to_string(&file)
                .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
            let count = count_unsafe_tokens(&source);
            if count > 0 {
                total += count;
                files_with_unsafe
                    .push(format!("{}: {count} unsafe token(s)", relative_path(&file)));
            }
        }

        if total > allowed {
            violations.push(format!(
                "crate {}: found {total} unsafe token(s), budget is {allowed}",
                krate.name
            ));
            violations.extend(files_with_unsafe);
        }
    }

    assert!(
        violations.is_empty(),
        "unsafe budget violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn budget_names_only_existing_crates() {
    let budget = load_budget();
    let crate_names: BTreeSet<String> = first_party_crates()
        .into_iter()
        .map(|krate| krate.name)
        .collect();
    let stale: Vec<String> = budget
        .budget
        .keys()
        .filter(|name| !crate_names.contains(*name))
        .cloned()
        .collect();

    assert!(
        stale.is_empty(),
        "unsafe-budget.toml names crates that do not exist under crates/ or apps/:\n{}",
        stale.join("\n")
    );
}

#[test]
fn unsafe_scanner_ignores_comments_strings_and_chars() {
    let cases = [
        ("line comment", "// unsafe\nfn safe() {}", 0),
        (
            "nested block comments",
            "/* unsafe /* unsafe */ unsafe */ fn safe() {}",
            0,
        ),
        (
            "plain string",
            r#"let value = "unsafe \" still unsafe";"#,
            0,
        ),
        (
            "raw string without hashes",
            r##"let value = r"unsafe";"##,
            0,
        ),
        (
            "raw string with one hash",
            r###"let value = r#"a " and unsafe"#;"###,
            0,
        ),
        (
            "raw string with two hashes",
            r####"let value = r##"a "# and unsafe"##;"####,
            0,
        ),
        ("byte string", r#"let value = b"unsafe";"#, 0),
        (
            "raw byte string",
            r###"let value = br#"a " and unsafe"#;"###,
            0,
        ),
        ("C string", r#"let value = c"unsafe";"#, 0),
        ("double-quote char", r#"let value = '"';"#, 0),
        ("escaped quote char", r"let value = '\'';", 0),
        ("escaped slash char", r"let value = '\\';", 0),
        ("Unicode char", r"let value = '\u{1F600}';", 0),
        (
            "lifetimes and labels",
            "fn f<'a>(value: &'static str) { 'outer: loop { unsafe{} break 'outer; } }",
            1,
        ),
        ("identifier boundaries", "unsafe_code my_unsafe unsafely", 0),
        (
            "unsafe token forms",
            "unsafe{} unsafe fn f() {} (unsafe)",
            3,
        ),
        ("unsafe lint attribute", "#![forbid(unsafe_code)]", 0),
    ];

    for (description, source, expected) in cases {
        assert_eq!(
            count_unsafe_tokens(source),
            expected,
            "scanner failed for {description}: {source}"
        );
    }
}

fn split_test_reference(reference: &str) -> Option<(&str, &str)> {
    let mut parts = reference.split("::");
    let crate_name = parts.next()?;
    let test_name = parts.next()?;
    (!crate_name.is_empty() && !test_name.is_empty() && parts.next().is_none())
        .then_some((crate_name, test_name))
}

fn is_rust_identifier(name: &str) -> bool {
    let (identifier, raw) = name
        .strip_prefix("r#")
        .map_or((name, false), |identifier| (identifier, true));
    let mut characters = identifier.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if identifier == "_" || first != '_' && !first.is_alphabetic() {
        return false;
    }
    if !characters.all(is_identifier_character) {
        return false;
    }

    const KEYWORDS: &[&str] = &[
        "Self", "abstract", "as", "async", "await", "become", "box", "break", "const", "continue",
        "crate", "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if",
        "impl", "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv",
        "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "try",
        "type", "typeof", "union", "unsafe", "unsized", "use", "virtual", "where", "while",
        "yield",
    ];
    if raw {
        !matches!(identifier, "crate" | "self" | "super" | "Self")
    } else {
        !KEYWORDS.contains(&identifier)
    }
}

#[test]
fn features_registry_is_well_formed() {
    let registry = load_registry();
    let crate_names: BTreeSet<String> = first_party_crates()
        .into_iter()
        .map(|krate| krate.name)
        .collect();
    let mut seen_ids = BTreeSet::new();
    let mut violations = Vec::new();

    for feature in &registry.feature {
        if feature.id.trim().is_empty() {
            violations.push("features.toml: feature id must not be empty".to_owned());
        } else if !seen_ids.insert(feature.id.as_str()) {
            violations.push(format!(
                "features.toml: duplicate feature id '{}'",
                feature.id
            ));
        }
        if feature.kind.trim().is_empty() {
            violations.push(format!(
                "features.toml: feature '{}': kind must not be empty",
                feature.id
            ));
        }
        if !matches!(
            feature.status.as_str(),
            "planned" | "partial" | "implemented"
        ) {
            violations.push(format!(
                "features.toml: feature '{}': status '{}' must be planned, partial, or implemented",
                feature.id, feature.status
            ));
        }
        if !matches!(
            feature.milestone.as_str(),
            "M0" | "M1" | "M2" | "M3" | "M4" | "M5"
        ) {
            violations.push(format!(
                "features.toml: feature '{}': milestone '{}' must be M0 through M5",
                feature.id, feature.milestone
            ));
        }

        for reference in &feature.tests {
            let Some((crate_name, test_name)) = split_test_reference(reference) else {
                violations.push(format!(
                    "features.toml: feature '{}': test '{}' must have the form crate-name::test_fn",
                    feature.id, reference
                ));
                continue;
            };
            if !crate_names.contains(crate_name) {
                violations.push(format!(
                    "features.toml: feature '{}': test '{}' names unknown crate '{}'",
                    feature.id, reference, crate_name
                ));
            }
            if !is_rust_identifier(test_name) {
                violations.push(format!(
                    "features.toml: feature '{}': test '{}' has invalid Rust identifier '{}'",
                    feature.id, reference, test_name
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "feature registry format violations:\n{}",
        violations.join("\n")
    );
}

fn skip_ascii_whitespace(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes
        .get(cursor)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        cursor += 1;
    }
    cursor
}

fn has_identifier_boundaries(code: &str, start: usize, length: usize) -> bool {
    let bytes = code.as_bytes();
    let before_ok =
        start == 0 || !bytes[start - 1].is_ascii_alphanumeric() && bytes[start - 1] != b'_';
    let end = start + length;
    let after_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric() && bytes[end] != b'_';
    before_ok && after_ok
}

fn function_positions(code: &str, name: &str) -> Vec<usize> {
    let bytes = code.as_bytes();
    let mut positions = Vec::new();

    for (start, _) in code.match_indices("fn") {
        if !has_identifier_boundaries(code, start, 2) {
            continue;
        }
        let mut cursor = skip_ascii_whitespace(bytes, start + 2);
        if !bytes[cursor..].starts_with(name.as_bytes())
            || !has_identifier_boundaries(code, cursor, name.len())
        {
            continue;
        }
        cursor = skip_ascii_whitespace(bytes, cursor + name.len());

        if bytes.get(cursor) == Some(&b'<') {
            let mut depth = 0_usize;
            while cursor < bytes.len() {
                match bytes[cursor] {
                    b'<' => depth += 1,
                    b'>' => {
                        depth -= 1;
                        if depth == 0 {
                            cursor += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                cursor += 1;
            }
            if depth != 0 {
                continue;
            }
            cursor = skip_ascii_whitespace(bytes, cursor);
        }

        if bytes.get(cursor) == Some(&b'(') {
            positions.push(start);
        }
    }
    positions
}

fn prefix_has_test_attribute(code: &str, function_start: usize) -> bool {
    let item_start = code[..function_start]
        .rfind([';', '{', '}'])
        .map_or(0, |index| index + 1);
    let prefix = &code[item_start..function_start];
    let bytes = prefix.as_bytes();
    let mut cursor = 0;

    while cursor < bytes.len() {
        if bytes[cursor] != b'#' {
            cursor += 1;
            continue;
        }
        let bracket = skip_ascii_whitespace(bytes, cursor + 1);
        if bytes.get(bracket) != Some(&b'[') {
            cursor += 1;
            continue;
        }

        let mut end = bracket + 1;
        let mut depth = 1_usize;
        while end < bytes.len() && depth > 0 {
            match bytes[end] {
                b'[' => depth += 1,
                b']' => depth -= 1,
                _ => {}
            }
            end += 1;
        }
        if depth == 0 && prefix[bracket + 1..end - 1].trim() == "test" {
            return true;
        }
        cursor = end;
    }
    false
}

fn proptest_block_ranges(code: &str) -> Vec<(usize, usize)> {
    let bytes = code.as_bytes();
    let mut ranges = Vec::new();

    for (start, _) in code.match_indices("proptest") {
        if !has_identifier_boundaries(code, start, "proptest".len()) {
            continue;
        }
        let bang = skip_ascii_whitespace(bytes, start + "proptest".len());
        if bytes.get(bang) != Some(&b'!') {
            continue;
        }
        let open = skip_ascii_whitespace(bytes, bang + 1);
        if bytes.get(open) != Some(&b'{') {
            continue;
        }

        let mut cursor = open + 1;
        let mut depth = 1_usize;
        while cursor < bytes.len() && depth > 0 {
            match bytes[cursor] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
            cursor += 1;
        }
        if depth == 0 {
            ranges.push((open + 1, cursor - 1));
        }
    }
    ranges
}

fn source_has_test_fn(source: &str, name: &str) -> bool {
    let code = strip_comments_and_literals(source);
    let proptest_ranges = proptest_block_ranges(&code);
    function_positions(&code, name).into_iter().any(|position| {
        prefix_has_test_attribute(&code, position)
            || proptest_ranges
                .iter()
                .any(|(start, end)| *start <= position && position < *end)
    })
}

fn test_fn_exists(crate_dir: &Path, test_name: &str) -> bool {
    rust_files(crate_dir).into_iter().any(|file| {
        let source =
            fs::read_to_string(&file).unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        source_has_test_fn(&source, test_name)
    })
}

#[test]
fn implemented_features_have_existing_tests() {
    let registry = load_registry();
    let crates: BTreeMap<String, PathBuf> = first_party_crates()
        .into_iter()
        .map(|krate| (krate.name, krate.dir))
        .collect();
    let mut violations = Vec::new();

    for feature in &registry.feature {
        if feature.status == "implemented" && feature.tests.is_empty() {
            violations.push(format!(
                "features.toml: feature '{}': status is implemented but tests is empty",
                feature.id
            ));
        }

        for reference in &feature.tests {
            let Some((crate_name, test_name)) = split_test_reference(reference) else {
                violations.push(format!(
                    "features.toml: feature '{}': test '{}' must have the form crate-name::test_fn",
                    feature.id, reference
                ));
                continue;
            };
            let Some(crate_dir) = crates.get(crate_name) else {
                violations.push(format!(
                    "features.toml: feature '{}': test '{}' names unknown crate '{}'",
                    feature.id, reference, crate_name
                ));
                continue;
            };
            if !test_fn_exists(crate_dir, test_name) {
                violations.push(format!(
                    "features.toml: feature '{}': test function '{}' was not found as #[test] or inside proptest! {{}} in crate '{}'",
                    feature.id, test_name, crate_name
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "feature registry test violations:\n{}",
        violations.join("\n")
    );
}

fn workspace_dependency_packages() -> BTreeMap<String, String> {
    let manifest = read_toml(&repo_root().join("Cargo.toml"));
    manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(toml::Value::as_table)
        .into_iter()
        .flatten()
        .map(|(alias, specification)| {
            let package = specification
                .as_table()
                .and_then(|table| table.get("package"))
                .and_then(toml::Value::as_str)
                .unwrap_or(alias)
                .to_owned();
            (alias.clone(), package)
        })
        .collect()
}

fn dependency_package(
    alias: &str,
    specification: &toml::Value,
    workspace_packages: &BTreeMap<String, String>,
) -> String {
    let Some(table) = specification.as_table() else {
        return alias.to_owned();
    };
    if let Some(package) = table.get("package").and_then(toml::Value::as_str) {
        return package.to_owned();
    }
    if table.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
        return workspace_packages
            .get(alias)
            .cloned()
            .unwrap_or_else(|| alias.to_owned());
    }
    alias.to_owned()
}

fn check_dependency_table(
    krate: &FirstPartyCrate,
    section: &str,
    dependencies: &toml::Table,
    workspace_packages: &BTreeMap<String, String>,
    violations: &mut Vec<String>,
) {
    const WINDOWS_PACKAGES: [&str; 4] = ["windows", "windows-sys", "windows-core", "wasapi"];

    for (alias, specification) in dependencies {
        let package = dependency_package(alias, specification, workspace_packages);
        if WINDOWS_PACKAGES.contains(&package.as_str()) && krate.name != "bio-sys" {
            violations.push(format!(
                "{} (crate {}, [{section}] dependency '{}'): package '{}' may only be used by bio-sys",
                relative_path(&krate.manifest),
                krate.name,
                alias,
                package
            ));
        }
        if package == "bio-sys" && (!krate.is_app || krate.name == "bio-replay") {
            let expectation = if krate.name == "bio-replay" {
                "bio-replay must run in Linux CI and may not depend on bio-sys"
            } else {
                "only crates under apps/ may depend on bio-sys"
            };
            violations.push(format!(
                "{} (crate {}, [{section}] dependency '{}'): {expectation}",
                relative_path(&krate.manifest),
                krate.name,
                alias
            ));
        }
    }
}

#[test]
fn windows_code_stays_in_bio_sys() {
    const DEPENDENCY_SECTIONS: [&str; 3] =
        ["dependencies", "dev-dependencies", "build-dependencies"];

    let workspace_packages = workspace_dependency_packages();
    let mut violations = Vec::new();

    for krate in first_party_crates() {
        let manifest = read_toml(&krate.manifest);
        for section in DEPENDENCY_SECTIONS {
            if let Some(dependencies) = manifest.get(section).and_then(toml::Value::as_table) {
                check_dependency_table(
                    &krate,
                    section,
                    dependencies,
                    &workspace_packages,
                    &mut violations,
                );
            }
        }

        if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
            for (target, target_manifest) in targets {
                for section in DEPENDENCY_SECTIONS {
                    if let Some(dependencies) =
                        target_manifest.get(section).and_then(toml::Value::as_table)
                    {
                        check_dependency_table(
                            &krate,
                            &format!("target.'{target}'.{section}"),
                            dependencies,
                            &workspace_packages,
                            &mut violations,
                        );
                    }
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Windows dependency policy violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn test_detector_finds_test_attribute() {
    let source = r#"
        #[test]
        #[should_panic(expected = "demonstration")]
        #[cfg(any(test, windows))]
        fn detected<T>() {}
    "#;
    assert!(source_has_test_fn(source, "detected"));
}

#[test]
fn test_detector_rejects_plain_function() {
    assert!(!source_has_test_fn("fn ordinary() {}", "ordinary"));
}

#[test]
fn test_detector_finds_proptest_function() {
    let source = r#"
        proptest! {
            fn generated(value in any::<u8>()) {
                prop_assert!(value <= u8::MAX);
            }
        }
    "#;
    assert!(source_has_test_fn(source, "generated"));
}

#[test]
fn test_detector_ignores_comments_and_strings() {
    let source = r##"
        // #[test] fn imaginary() {}
        /* #[test] fn imaginary() {} */
        const EXAMPLE: &str = "#[test] fn imaginary() {}";
        const RAW: &str = r#"#[test] fn imaginary() {}"#;
    "##;
    assert!(!source_has_test_fn(source, "imaginary"));
}
