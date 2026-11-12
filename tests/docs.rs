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

/// The layers DSH composes a profile from at every start, in DSH's order:
/// the bundles in the profile's `package.json`, the profile's own patch, the
/// home layer, which applies to every profile of the DSH home, and the
/// `--patch` overlays of the run.
const COMPOSITION: [&str; 4] = [
    "package.json",
    "cordis.patch.yml",
    "$DSH_HOME/cordis.patch.yml",
    "--patch",
];

// catches: a README whose composition order leaves out a layer DSH applies, or names it out of
// DSH's order, so a reader checking an instance against its profile folder misses what overrides it
#[test]
fn readme_states_the_composition_order() -> TestResult {
    let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))?;
    let prose = text.replace('`', "");
    let prose = prose.split_whitespace().collect::<Vec<_>>().join(" ");
    let start = prose
        .find("DSH composes the profile at every start")
        .ok_or("README.md: no sentence states the order DSH composes a profile in")?;
    let sentence = prose[start..].split(". ").next().unwrap_or_default();
    let mut rest = sentence;
    for layer in COMPOSITION {
        let Some(at) = rest.find(layer) else {
            return Err(format!(
                "README.md: the composition order does not name {layer} after the layers before it: {sentence}"
            )
            .into());
        };
        rest = &rest[at + layer.len()..];
    }
    Ok(())
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
