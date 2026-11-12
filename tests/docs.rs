//! CLI examples in the shipped documentation must parse through the real parser.

use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::path::Path;

type TestResult = Result<(), Box<dyn Error>>;
const DOCS: [&str; 2] = ["README.md", "src/cli/skill.md"];

// catches: a README or agent-guide example that no longer parses: a renamed verb or flag, a NAME given
// where none is taken, a flag on the wrong verb, or dsh arguments without -- (P4.1)
#[test]
fn doc_examples_parse() -> TestResult {
    let mut failures = Vec::new();
    for doc in DOCS {
        let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(doc))?;
        let mut checked = 0;
        for (example, prose) in examples(&text) {
            let Some(words) = shell_words(&example) else {
                failures.push(format!("{doc}: unbalanced quotes in {example}"));
                continue;
            };
            let args = words.get(1..).unwrap_or_default();
            if (prose && args.len() < 2) || args.iter().any(|word| elided(word)) {
                continue;
            }
            checked += 1;
            if let Err(error) = dshd::cli::parse(args.iter().map(OsString::from).collect()) {
                failures.push(format!("{doc}: {example}: {error}"));
            }
        }
        if checked == 0 {
            failures.push(format!("{doc}: no dshd example to check"));
        }
    }
    assert!(
        failures.is_empty(),
        "examples that do not parse:\n{}",
        failures.join("\n")
    );
    Ok(())
}

// catches: a flag in a verb's usage line (src/cli/help.rs) that the README's verb table, which the
// README calls the reference for each verb, does not list, as `run --patch` once was
#[test]
fn verb_table_lists_every_flag() -> TestResult {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let help = fs::read_to_string(root.join("src/cli/help.rs"))?;
    let readme = fs::read_to_string(root.join("README.md"))?;
    let rows: Vec<(&str, &str)> = readme
        .lines()
        .skip_while(|line| !line.starts_with("| Verb | Flags |"))
        .skip(2)
        .take_while(|line| line.starts_with("| `"))
        .filter_map(|line| {
            let (verb, rest) = line.strip_prefix("| `")?.split_once('`')?;
            Some((verb.split(' ').next()?, rest.split(" | ").nth(1)?))
        })
        .collect();
    let mut failures = Vec::new();
    for usage in help
        .lines()
        .filter_map(|line| line.split_once("Usage: dshd "))
    {
        // The general page's `VERB`, and code that builds a usage line, name no verb.
        let verb = usage.1.split(' ').next().unwrap_or_default();
        if verb.is_empty() || !verb.chars().all(|c| c.is_ascii_lowercase()) {
            continue;
        }
        let Some((_, listed)) = rows.iter().find(|(row, _)| *row == verb) else {
            failures.push(format!("{verb}: no row"));
            continue;
        };
        for flag in flags(usage.1).filter(|flag| !["--json", "--text"].contains(flag)) {
            if !flags(listed).any(|row_flag| row_flag == flag) {
                failures.push(format!("{verb}: {flag}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "flags missing from the README verb table:\n{}",
        failures.join("\n")
    );
    Ok(())
}

/// The flags named in a usage line or a table cell: `-x` and `--word`.
fn flags(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace() || "[]|()`,\\".contains(c))
        .filter(|word| {
            let name = word.trim_start_matches('-');
            word.starts_with('-') && name.starts_with(|c: char| c.is_ascii_alphabetic())
        })
}

/// The `dshd` command lines of a Markdown text: `$ dshd ...` lines, and
/// inline code spans that start with `dshd`, which are prose (`true`).
fn examples(text: &str) -> Vec<(String, bool)> {
    let mut found = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("$ dshd")
            && (rest.is_empty() || rest.starts_with(' '))
        {
            found.push((format!("dshd{rest}"), false));
        }
        for span in line.split('`').skip(1).step_by(2) {
            if span == "dshd" || span.starts_with("dshd ") {
                found.push((span.to_owned(), true));
            }
        }
    }
    found
}

/// A word that stands for others: a metavariable such as `NAME` or
/// `KEY=VALUE`, an elision, an optional part, a pipe, or a variable.
fn elided(word: &str) -> bool {
    let metavariable = !word.starts_with('-')
        && word.chars().any(|c| c.is_ascii_uppercase())
        && word
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || "_=.-".contains(c));
    metavariable || word.contains("...") || word.contains(['[', '|', '<', '$'])
}

/// Splits a command line as a POSIX shell would, for quotes and comments;
/// `None` when a quote is not closed.
fn shell_words(line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match quote {
            Some(open) if c == open => quote = None,
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                word.get_or_insert_with(String::new);
            }
            None if c == '#' && word.is_none() => break,
            None if c.is_whitespace() => words.extend(word.take()),
            Some(_) | None => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    quote.is_none().then_some(words)
}
