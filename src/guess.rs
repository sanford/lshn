//! The language of a block of code that doesn't say, as most in comments
//! don't: from what's unmistakable in it, or not at all. A block colored
//! as the wrong language reads worse than one left plain, so every guess
//! here stays quiet on prose, logs and drawings.

/// Each language and what gives it away, the surest first: a diff's
/// headers or valid JSON can't be much else, and a line of `key: value`
/// could be most things, so YAML comes last.
/// What gives a language away.
type Sign = fn(&Code) -> bool;

const GUESSES: &[(&str, Sign)] = &[
    ("diff", diff),
    ("json", json),
    ("html", html),
    ("xml", xml),
    ("toml", toml),
    ("css", css),
    ("haskell", haskell),
    ("clojure", clojure),
    ("lisp", lisp),
    ("go", go),
    ("rust", rust),
    ("c++", cpp),
    ("c", c),
    ("sql", sql),
    ("dockerfile", dockerfile),
    ("python", python),
    ("ruby", ruby),
    ("java", java),
    ("typescript", typescript),
    ("javascript", javascript),
    ("bash", shell),
    ("yaml", yaml),
];

struct Code<'a> {
    text: &'a str,
    /// Its lines, trimmed, without the blank ones.
    lines: Vec<&'a str>,
}

impl Code<'_> {
    /// Whether any line starts with one of `starts`.
    fn starts(&self, starts: &[&str]) -> bool {
        self.lines
            .iter()
            .any(|l| starts.iter().any(|s| l.starts_with(s)))
    }

    fn has(&self, any: &[&str]) -> bool {
        any.iter().any(|s| self.text.contains(s))
    }

    /// How many of `signs` it has.
    fn count(&self, signs: &[&str]) -> usize {
        signs.iter().filter(|s| self.text.contains(*s)).count()
    }

    /// Whether lines end with `;`, as C's family's do.
    fn semicolons(&self) -> bool {
        self.lines.iter().filter(|l| l.ends_with(';')).count() * 3 >= self.lines.len()
    }
}

/// The language `text` is in, as a name syntect knows, or "" if it isn't
/// plain enough to say.
pub fn guess(text: &str) -> &'static str {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return "";
    }
    let code = Code { text, lines };
    if let Some(lang) = shebang(&code) {
        return lang;
    }
    GUESSES
        .iter()
        .find(|(_, is)| is(&code))
        .map_or("", |(lang, _)| lang)
}

fn shebang(code: &Code) -> Option<&'static str> {
    let first = code.lines[0].strip_prefix("#!")?;
    let program = first.split_whitespace().last()?;
    let program = program.rsplit('/').next()?;
    let program = program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    Some(match program {
        "sh" | "bash" | "zsh" | "ksh" | "dash" => "bash",
        "python" => "python",
        "node" | "deno" | "bun" => "javascript",
        "ruby" => "ruby",
        "perl" => "perl",
        _ => return None,
    })
}

fn diff(code: &Code) -> bool {
    let headers = code.starts(&["diff --git ", "--- a/", "+++ b/", "Index: "]);
    let hunk = code.lines.iter().any(|l| {
        l.starts_with("@@ -") && l[4..].contains(" @@") && l.as_bytes()[4].is_ascii_digit()
    });
    hunk || (headers && code.starts(&["+++ ", "--- "]))
}

fn json(code: &Code) -> bool {
    let t = code.text.trim();
    let shaped =
        (t.starts_with('{') && t.ends_with('}')) || (t.starts_with('[') && t.ends_with(']'));
    // `[1, 2]` is JSON, but it's as likely to be anything; objects only, or
    // lists of them.
    shaped && t.contains(':') && serde_json::from_str::<serde_json::Value>(t).is_ok()
}

fn html(code: &Code) -> bool {
    let t = code.text.trim_start().to_ascii_lowercase();
    t.starts_with("<!doctype html")
        || t.starts_with("<html")
        || (t.starts_with('<')
            && code.count(&[
                "<div", "<span", "<p>", "<a href", "<head", "<body", "<script", "<ul", "<li",
            ]) >= 2)
}

fn xml(code: &Code) -> bool {
    let t = code.text.trim_start();
    t.starts_with("<?xml")
        || (t.starts_with('<')
            && t.trim_end().ends_with('>')
            && code.has(&["</", "/>"])
            && code
                .lines
                .iter()
                .all(|l| l.starts_with('<') || l.ends_with('>')))
}

fn toml(code: &Code) -> bool {
    let section = |l: &&str| {
        l.starts_with('[')
            && l.ends_with(']')
            && l.len() > 2
            && !l.contains(' ')
            && !l.contains(',')
    };
    let assignment = |l: &&str| {
        l.split_once(" = ").is_some_and(|(key, _)| {
            !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-.\"".contains(c))
        })
    };
    code.lines.iter().any(section)
        && code.lines.iter().any(assignment)
        && code.lines.iter().all(|l| {
            section(l)
                || assignment(l)
                || l.starts_with('#')
                || l.starts_with(['"', '\'', ']', '}', ' '])
                || l.ends_with(',')
        })
}

fn css(code: &Code) -> bool {
    let declarations = code
        .lines
        .iter()
        .filter(|l| {
            l.split_once(": ").is_some_and(|(prop, _)| {
                !prop.is_empty() && prop.chars().all(|c| c.is_ascii_lowercase() || c == '-')
            }) && l.ends_with(';')
        })
        .count();
    code.lines.iter().any(|l| l.ends_with('{')) && declarations >= 2 && !code.has(&["(", "="])
}

fn includes(code: &Code) -> bool {
    code.starts(&["#include <", "#include \""])
}

fn cpp(code: &Code) -> bool {
    let signs = [
        "std::",
        "cout <<",
        "template <",
        "template<",
        "namespace ",
        "nullptr",
        "#include <iostream>",
        "#include <vector>",
        "auto& ",
        "const auto",
    ];
    (includes(code) && code.count(&signs) >= 1) || code.count(&signs) >= 2
}

fn c(code: &Code) -> bool {
    includes(code)
        || (code.semicolons()
            && !code.has(&["fn ", "let ", "impl ", "#!["])
            && code.count(&[
                "int main(",
                "printf(",
                "malloc(",
                "sizeof(",
                "NULL",
                "->",
                "char *",
                "void *",
                "struct ",
            ]) >= 2)
}

fn sql(code: &Code) -> bool {
    let upper = code.text.to_ascii_uppercase();
    let first = code.lines[0].to_ascii_uppercase();
    let opens = [
        "SELECT ",
        "INSERT INTO ",
        "UPDATE ",
        "DELETE FROM ",
        "CREATE TABLE ",
        "CREATE INDEX ",
        "WITH ",
        "ALTER TABLE ",
        "EXPLAIN ",
    ]
    .iter()
    .any(|k| first.starts_with(k));
    let clauses = [
        " FROM ",
        " WHERE ",
        " VALUES",
        " SET ",
        " JOIN ",
        " GROUP BY ",
        " ORDER BY ",
        "PRIMARY KEY",
        " AS (",
    ];
    let upper = upper.replace('\n', " ");
    opens && clauses.iter().any(|c| upper.contains(c))
}

fn dockerfile(code: &Code) -> bool {
    code.lines[0].starts_with("FROM ")
        && code.starts(&["RUN ", "COPY ", "CMD ", "ENTRYPOINT ", "WORKDIR ", "ENV "])
}

fn go(code: &Code) -> bool {
    code.starts(&["package "]) && code.has(&["func ", "import "])
        || code.count(&[
            "func ",
            ":= ",
            "err != nil",
            "fmt.",
            "chan ",
            "go func",
            "defer ",
        ]) >= 2
}

fn rust(code: &Code) -> bool {
    code.starts(&["use std::", "#[derive(", "impl ", "pub fn ", "fn main()"])
        && code.has(&["fn ", "let ", "::", "impl "])
        || code.count(&[
            "fn ",
            "let mut ",
            "-> ",
            "::",
            "&self",
            "&mut ",
            "Some(",
            "Ok(",
            "unwrap()",
            "impl ",
            "pub struct",
            "match ",
        ]) >= 3
            && code.has(&["fn ", "let "])
}

fn python(code: &Code) -> bool {
    let block = |l: &&&str| {
        [
            "def ", "class ", "if ", "for ", "while ", "with ", "elif ", "try", "except",
        ]
        .iter()
        .any(|k| l.starts_with(k))
            && l.ends_with(':')
    };
    let imports = code.lines.iter().any(|l| {
        (l.starts_with("import ") && !l.contains(['{', ';', '"', '\'']))
            || (l.starts_with("from ") && l.contains(" import "))
    });
    let defs = code
        .lines
        .iter()
        .any(|l| (l.starts_with("def ") || l.starts_with("class ")) && l.ends_with(':'));
    let blocks = code.lines.iter().filter(block).count();
    !code.semicolons()
        && !code.has(&["{\n", "};"])
        && (defs
            || (imports && (blocks > 0 || code.has(&["print(", "self.", "__"])))
            || (blocks >= 2
                && code.has(&["print(", "self.", "None", "True", "False", " in range("])))
}

fn ruby(code: &Code) -> bool {
    let ends = code.lines.iter().filter(|l| **l == "end").count();
    ends >= 1
        && code.starts(&["def ", "class ", "module ", "require ", "puts "])
        && !code.has(&[":\n", "{\n"])
}

fn java(code: &Code) -> bool {
    code.count(&[
        "public class ",
        "public static void ",
        "System.out.",
        "private final ",
        "import java.",
        "@Override",
        "new ArrayList<",
        "public interface ",
    ]) >= 1
        && code.has(&[";"])
}

fn javascript(code: &Code) -> bool {
    let signs = [
        "const ",
        "let ",
        "=> ",
        "function ",
        "console.log(",
        "require(",
        "document.",
        "await ",
        "export ",
        "module.exports",
        "===",
        "async ",
    ];
    let import = code
        .lines
        .iter()
        .any(|l| l.starts_with("import ") && l.contains(" from ") && l.contains(['"', '\'']));
    // Rust's `fn f() -> u8` has a `let` or two as well.
    let rust = code.starts(&["fn "]) || code.has(&[") -> "]);
    !rust && (import || code.count(&signs) >= 2)
}

fn typescript(code: &Code) -> bool {
    javascript(code)
        && code.count(&[
            ": string",
            ": number",
            ": boolean",
            "interface ",
            "type ",
            "<T>",
            ": Promise<",
            " as const",
        ]) >= 1
}

fn haskell(code: &Code) -> bool {
    let signature = code.lines.iter().any(|l| {
        l.split_once(" :: ").is_some_and(|(name, ty)| {
            !name.contains(' ')
                && name.chars().next().is_some_and(char::is_lowercase)
                && (ty.contains("->") || ty.chars().next().is_some_and(char::is_uppercase))
        })
    });
    // `import Data.List`: a module's name, capitalized and dotted.
    let imports = code.lines.iter().any(|l| {
        l.strip_prefix("import ").is_some_and(|m| {
            let m = m.strip_prefix("qualified ").unwrap_or(m);
            m.starts_with(|c: char| c.is_ascii_uppercase())
                && m.split_whitespace().next().is_some_and(|m| m.contains('.'))
        })
    }) && !code.has(&[";", "{"]);
    imports
        || signature && !code.has(&[";", "{"])
        || code.starts(&["module "]) && code.has(&[" where"])
}

/// Whether it's s-expressions: after any `;` comments, it starts with `(`,
/// and its brackets balance.
fn sexps(code: &Code) -> bool {
    let first = code.lines.iter().find(|l| !l.starts_with(';'));
    if !first.is_some_and(|l| l.starts_with('(')) {
        return false;
    }
    let mut depth = 0i32;
    for c in code.text.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return false;
        }
    }
    depth == 0
}

fn clojure(code: &Code) -> bool {
    sexps(code) && code.has(&["(defn ", "(ns ", "(def ", "(let ["])
}

fn lisp(code: &Code) -> bool {
    sexps(code)
        && code.has(&[
            "(defun ",
            "(define ",
            "(lambda ",
            "(let (",
            "(let* (",
            "(setq ",
            "(defmacro ",
        ])
}

fn shell(code: &Code) -> bool {
    const COMMANDS: &[&str] = &[
        "sudo ",
        "apt ",
        "apt-get ",
        "brew ",
        "npm ",
        "npx ",
        "yarn ",
        "pnpm ",
        "pip ",
        "pip3 ",
        "cargo ",
        "git ",
        "cd ",
        "ls ",
        "curl ",
        "wget ",
        "docker ",
        "kubectl ",
        "make ",
        "mkdir ",
        "export ",
        "echo ",
        "chmod ",
        "rm -",
        "cp ",
        "mv ",
        "ssh ",
        "tar ",
        "go install ",
        "go run ",
        "nix ",
        "uv ",
        "python -m ",
        "python3 -m ",
    ];
    let command = |l: &str| {
        COMMANDS
            .iter()
            .any(|c| l.starts_with(c) || l == c.trim_end())
    };
    // A session: commands after their prompts, then what they printed.
    let prompted: Vec<&str> = code
        .lines
        .iter()
        .filter_map(|l| l.strip_prefix("$ "))
        .collect();
    if !prompted.is_empty() {
        return prompted.iter().any(|l| command(l) || l.contains(" | "));
    }
    let commands = code
        .lines
        .iter()
        .filter(|l| command(l) || l.starts_with('#'))
        .count();
    commands == code.lines.len() && code.lines.iter().any(|l| command(l))
        || code.has(&["if [ ", "fi\n", "; then", "; do", "done\n", "esac"])
            && code.count(&["$", "echo ", "fi", "done"]) >= 2
}

fn yaml(code: &Code) -> bool {
    let key = |l: &str| {
        let l = l.strip_prefix("- ").unwrap_or(l);
        l.split_once(':').is_some_and(|(key, value)| {
            key.starts_with(|c: char| c.is_ascii_alphabetic())
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c))
                && (value.is_empty() || value.starts_with(' '))
        })
    };
    let keys = code.lines.iter().filter(|l| key(l)).count();
    // A key with what's under it on the next lines, or a list of items.
    let opens = code
        .lines
        .iter()
        .any(|l| l.ends_with(':') || l.starts_with("- "));
    // Nested, as YAML is: some lines indented under others.
    let nested = code
        .text
        .lines()
        .any(|l| l.starts_with("  ") && !l.trim().is_empty());
    code.lines.len() >= 3
        && keys >= 2
        && nested
        && opens
        && code
            .lines
            .iter()
            .all(|l| key(l) || l.starts_with("- ") || l.starts_with('#') || *l == "---")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_what_is_plain() {
        let cases = [
            ("#!/usr/bin/env python3\nprint('hi')", "python"),
            ("#!/bin/bash\necho hi", "bash"),
            ("@@ -1,3 +1,4 @@\n line\n-old\n+new", "diff"),
            ("{\"a\": 1, \"b\": [true, null]}", "json"),
            ("<!DOCTYPE html>\n<html><body></body></html>", "html"),
            ("[package]\nname = \"lshn\"\nversion = \"0.5.1\"", "toml"),
            (".box {\n  color: red;\n  margin-top: 4px;\n}", "css"),
            (
                "#include <stdio.h>\nint main(void) {\n  printf(\"hi\");\n}",
                "c",
            ),
            ("#include <vector>\nstd::vector<int> v;", "c++"),
            ("SELECT name, count(*)\nFROM users\nGROUP BY name;", "sql"),
            ("FROM rust:1.80\nRUN cargo build --release", "dockerfile"),
            ("if err != nil {\n\treturn err\n}\nx := f()", "go"),
            (
                "fn main() {\n    let mut v = Vec::new();\n    v.push(1);\n}",
                "rust",
            ),
            ("impl Foo {\n    fn bar(&self) -> u32 { 1 }\n}", "rust"),
            ("def f(x):\n    return x + 1", "python"),
            (
                "import os\nfor p in os.listdir('.'):\n    print(p)",
                "python",
            ),
            ("class Foo\n  def bar\n    puts 'hi'\n  end\nend", "ruby"),
            (
                "public class Main {\n  public static void main(String[] a) {\n    System.out.println(1);\n  }\n}",
                "java",
            ),
            ("const f = async (x) => {\n  await g(x);\n};", "javascript"),
            ("import { x } from 'y';\nx();", "javascript"),
            (
                "function f(a: string): number {\n  const n = a.length;\n  return n;\n}",
                "typescript",
            ),
            (
                "map' :: (a -> b) -> [a] -> [b]\nmap' f = foldr ((:) . f) []",
                "haskell",
            ),
            ("(defn square [x]\n  (* x x))", "clojure"),
            ("(defun square (x)\n  (* x x))", "lisp"),
            ("$ cargo install lshn\n    Updating crates.io index", "bash"),
            ("brew install lshn\nlshn best", ""),
            ("git clone https://github.com/a/b\ncd b\nmake", "bash"),
            ("server:\n  port: 8080\n  host: localhost", "yaml"),
        ];
        for (code, lang) in cases {
            assert_eq!(guess(code), lang, "{code}");
        }
    }

    /// What's in comments that isn't code: left plain.
    #[test]
    fn stays_quiet_on_what_isnt_code() {
        for text in [
            "This is just a sentence; it has a semicolon. And another one;",
            "Note: this is important.\nAlso: so is this.",
            "2026-10-09 12:00:01 ERROR connection refused\n2026-10-09 12:00:02 INFO retrying",
            "+-----+     +-----+\n| a   |---->| b   |\n+-----+     +-----+",
            "     1   2   3\n     4   5   6",
            "Q: why?\nA: because.\nQ: and?",
            "(this is a parenthetical aside, not a program)",
            "[1, 2, 3]",
            "Step 1. Do the thing\nStep 2. Do the other thing",
            "let it be",
            "import this",
        ] {
            assert_eq!(guess(text), "", "{text}");
        }
    }

    #[test]
    fn guesses_are_languages_syntect_knows() {
        let set = two_face::syntax::extra_newlines();
        for (lang, _) in GUESSES {
            assert!(set.find_syntax_by_token(lang).is_some(), "{lang}");
        }
        for lang in ["bash", "python", "javascript", "ruby", "perl"] {
            assert!(set.find_syntax_by_token(lang).is_some(), "{lang}");
        }
    }
}
