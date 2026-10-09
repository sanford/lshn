//! TeX's math, as Unicode text: `\frac{a}{b}` as a/b, `x^2` as x², `\alpha`
//! as α. Terminals can't typeset, but most of what articles write reads
//! fine as a line of symbols.

/// `tex`, the inside of a formula, as text.
pub fn to_unicode(tex: &str) -> String {
    let mut scanner = Scanner {
        src: tex.chars().collect(),
        pos: 0,
    };
    let mut out = String::new();
    loop {
        out.push_str(&scanner.sequence());
        if scanner.pos >= scanner.src.len() {
            break;
        }
        // A closing brace with no opening one.
        scanner.pos += 1;
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    out.replace("( ", "(").replace(" )", ")").replace(" ,", ",")
}

/// `text` with the formulas in it, between `$…$`, `$$…$$`, `\(…\)` or
/// `\[…\]`, as text. A `$` that isn't a formula's (a price) is left be.
pub fn in_text(text: &str) -> String {
    if !text.contains(['$', '\\']) {
        return text.to_string();
    }
    let mut out = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let Some(start) = rest.find(['$', '\\']) else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let (open, close) = if rest.starts_with("$$") {
            ("$$", "$$")
        } else if rest.starts_with('$') {
            ("$", "$")
        } else if rest.starts_with("\\(") {
            ("\\(", "\\)")
        } else if rest.starts_with("\\[") {
            ("\\[", "\\]")
        } else {
            out.push('\\');
            rest = &rest[1..];
            continue;
        };
        let inner = &rest[open.len()..];
        let Some(end) = inner.find(close) else {
            out.push_str(open);
            rest = inner;
            continue;
        };
        let formula = &inner[..end];
        let math = match open {
            // "$5 and $10" is money: a formula hugs its dollars.
            "$" => {
                !formula.is_empty()
                    && !formula.starts_with(' ')
                    && !formula.ends_with(' ')
                    && !formula.contains('\n')
                    && looks_like_math(formula)
            }
            _ => !formula.trim().is_empty(),
        };
        if math {
            out.push_str(&to_unicode(formula));
            rest = &inner[end + close.len()..];
        } else {
            out.push_str(open);
            rest = inner;
        }
    }
    out
}

/// Whether what's between two `$` is a formula rather than prose between
/// two prices: it has TeX in it, or it's a lone symbol or number.
fn looks_like_math(s: &str) -> bool {
    s.contains(['\\', '^', '_', '=', '{'])
        || s.chars().count() == 1
        || s.chars()
            .all(|c| c.is_ascii_digit() || c == '/' || c == '.')
            && !s.starts_with(|c: char| c.is_ascii_digit())
}

struct Scanner {
    src: Vec<char>,
    pos: usize,
}

impl Scanner {
    fn peek(&self) -> Option<char> {
        self.src.get(self.pos).copied()
    }

    fn skip(&mut self, c: char) {
        if self.peek() == Some(c) {
            self.pos += 1;
        }
    }

    /// Up to the brace that closes the group it's in, or the end.
    fn sequence(&mut self) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            match c {
                '}' => break,
                '{' => {
                    self.pos += 1;
                    out.push_str(&self.sequence());
                    self.skip('}');
                }
                '\\' => out.push_str(&self.command()),
                '^' => {
                    self.pos += 1;
                    let arg = self.arg();
                    out.push_str(&script(&arg, true));
                }
                '_' => {
                    self.pos += 1;
                    let arg = self.arg();
                    out.push_str(&script(&arg, false));
                }
                '~' | '&' => {
                    self.pos += 1;
                    out.push(' ');
                }
                '\'' => {
                    self.pos += 1;
                    out.push('′');
                }
                c => {
                    self.pos += 1;
                    out.push(c);
                }
            }
        }
        out
    }

    /// What a script or command takes: a group, a command, or a character.
    fn arg(&mut self) -> String {
        while self.peek() == Some(' ') {
            self.pos += 1;
        }
        match self.peek() {
            None => String::new(),
            Some('{') => {
                self.pos += 1;
                let inner = self.sequence();
                self.skip('}');
                inner
            }
            Some('\\') => self.command(),
            Some(c) => {
                self.pos += 1;
                c.to_string()
            }
        }
    }

    /// An optional argument, `[3]` in `\sqrt[3]{x}`.
    fn optional(&mut self) -> Option<String> {
        if self.peek() != Some('[') {
            return None;
        }
        let end = self.src[self.pos..].iter().position(|&c| c == ']')?;
        let inner: String = self.src[self.pos + 1..self.pos + end].iter().collect();
        self.pos += end + 1;
        Some(to_unicode(&inner))
    }

    fn command(&mut self) -> String {
        self.pos += 1;
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        if self.pos == start {
            let Some(c) = self.peek() else {
                return String::new();
            };
            self.pos += 1;
            return match c {
                // A row's end, in cases and matrices.
                '\\' => "; ".into(),
                ',' | ';' | ':' | ' ' => " ".into(),
                '!' => String::new(),
                '{' => "{".into(),
                '}' => "}".into(),
                '|' => "‖".into(),
                c => c.to_string(),
            };
        }
        let name: String = self.src[start..self.pos].iter().collect();
        self.expand(&name)
    }

    fn expand(&mut self, name: &str) -> String {
        match name {
            "frac" | "tfrac" | "dfrac" | "cfrac" => {
                let (num, den) = (self.arg(), self.arg());
                format!("{}/{}", term(&num), term(&den))
            }
            "binom" | "tbinom" | "dbinom" => {
                let (n, k) = (self.arg(), self.arg());
                format!("C({n}, {k})")
            }
            "sqrt" => {
                let root = match self.optional().as_deref() {
                    Some("3") => "∛",
                    Some("4") => "∜",
                    _ => "√",
                };
                let arg = self.arg();
                if arg.chars().count() == 1 {
                    format!("{root}{arg}")
                } else {
                    format!("{root}({arg})")
                }
            }
            "mathbb" => self.arg().chars().map(double_struck).collect(),
            "mathcal" | "mathscr" => self.arg().chars().map(script_letter).collect(),
            "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr" | "Bigl"
            | "Bigr" | "biggl" | "biggr" | "Biggl" | "Biggr" | "middle" => {
                self.skip('.');
                String::new()
            }
            "begin" => {
                let env = self.arg();
                // A matrix's or an alignment's column spec.
                if env == "array" {
                    self.arg();
                }
                if env == "cases" {
                    "{".into()
                } else {
                    String::new()
                }
            }
            "end" => {
                self.arg();
                String::new()
            }
            "phantom" | "hphantom" | "vphantom" | "label" | "tag" => {
                self.arg();
                String::new()
            }
            "not" => {
                let arg = self.arg();
                negated(&arg)
            }
            "operatorname" => self.arg(),
            _ if WRAPPERS.contains(&name) => self.arg(),
            _ => {
                if let Some(&(_, accent)) = ACCENTS.iter().find(|(n, _)| *n == name) {
                    let arg = self.arg();
                    return if arg.chars().count() == 1 {
                        format!("{arg}{accent}")
                    } else {
                        arg
                    };
                }
                match SYMBOLS.iter().find(|(n, _)| *n == name) {
                    Some((_, symbol)) => spaced(symbol),
                    // `\displaystyle` and the like, and what this doesn't
                    // know: its name, or nothing.
                    None if IGNORED.contains(&name) => String::new(),
                    None => name.to_string(),
                }
            }
        }
    }
}

/// A fraction's top or bottom, in brackets if it's more than one term:
/// if it has an operator in it, outside any brackets of its own.
fn term(s: &str) -> String {
    let s = s.trim();
    let mut depth = 0;
    let mut terms = 1;
    for c in s.chars() {
        match c {
            '(' | '[' | '{' | '⟨' => depth += 1,
            ')' | ']' | '}' | '⟩' => depth -= 1,
            c if depth == 0 && (c.is_whitespace() || "+-−=/·×,<>±∓".contains(c)) => {
                terms += 1
            }
            _ => {}
        }
    }
    if terms == 1 {
        s.to_string()
    } else {
        format!("({s})")
    }
}

/// `arg` raised or lowered: in Unicode's super- or subscript characters if
/// it has all of them, else after `^` or `_`.
fn script(arg: &str, up: bool) -> String {
    let arg = arg.trim();
    let mapped: Option<String> = arg
        .chars()
        .map(|c| if up { superscript(c) } else { subscript(c) })
        .collect();
    match mapped {
        Some(s) if !s.is_empty() => s,
        _ => {
            let mark = if up { '^' } else { '_' };
            if arg.chars().count() == 1 {
                format!("{mark}{arg}")
            } else {
                format!("{mark}({arg})")
            }
        }
    }
}

fn superscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        '+' => '⁺',
        '-' | '−' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        'n' => 'ⁿ',
        'i' => 'ⁱ',
        'a' => 'ᵃ',
        'b' => 'ᵇ',
        'c' => 'ᶜ',
        'd' => 'ᵈ',
        'e' => 'ᵉ',
        'f' => 'ᶠ',
        'g' => 'ᵍ',
        'h' => 'ʰ',
        'j' => 'ʲ',
        'k' => 'ᵏ',
        'l' => 'ˡ',
        'm' => 'ᵐ',
        'o' => 'ᵒ',
        'p' => 'ᵖ',
        'r' => 'ʳ',
        's' => 'ˢ',
        't' => 'ᵗ',
        'u' => 'ᵘ',
        'v' => 'ᵛ',
        'w' => 'ʷ',
        'x' => 'ˣ',
        'y' => 'ʸ',
        'z' => 'ᶻ',
        'T' => 'ᵀ',
        '′' => '′',
        '∗' | '*' => '*',
        _ => return None,
    })
}

fn subscript(c: char) -> Option<char> {
    Some(match c {
        '0' => '₀',
        '1' => '₁',
        '2' => '₂',
        '3' => '₃',
        '4' => '₄',
        '5' => '₅',
        '6' => '₆',
        '7' => '₇',
        '8' => '₈',
        '9' => '₉',
        '+' => '₊',
        '-' | '−' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'h' => 'ₕ',
        'i' => 'ᵢ',
        'j' => 'ⱼ',
        'k' => 'ₖ',
        'l' => 'ₗ',
        'm' => 'ₘ',
        'n' => 'ₙ',
        'o' => 'ₒ',
        'p' => 'ₚ',
        'r' => 'ᵣ',
        's' => 'ₛ',
        't' => 'ₜ',
        'u' => 'ᵤ',
        'v' => 'ᵥ',
        'x' => 'ₓ',
        _ => return None,
    })
}

fn double_struck(c: char) -> char {
    match c {
        'C' => 'ℂ',
        'H' => 'ℍ',
        'N' => 'ℕ',
        'P' => 'ℙ',
        'Q' => 'ℚ',
        'R' => 'ℝ',
        'Z' => 'ℤ',
        'A'..='Z' => char::from_u32(0x1D538 + (c as u32 - 'A' as u32)).unwrap_or(c),
        '1' => '𝟙',
        c => c,
    }
}

fn script_letter(c: char) -> char {
    match c {
        'B' => 'ℬ',
        'E' => 'ℰ',
        'F' => 'ℱ',
        'H' => 'ℋ',
        'I' => 'ℐ',
        'L' => 'ℒ',
        'M' => 'ℳ',
        'R' => 'ℛ',
        'A'..='Z' => char::from_u32(0x1D49C + (c as u32 - 'A' as u32)).unwrap_or(c),
        c => c,
    }
}

/// `\not=` and the like.
fn negated(arg: &str) -> String {
    let negated = match arg.trim() {
        "=" => "≠".into(),
        "∈" => "∉".into(),
        "⊂" => "⊄".into(),
        "⊆" => "⊈".into(),
        "≡" => "≢".into(),
        "<" => "≮".into(),
        ">" => "≯".into(),
        "∼" => "≁".into(),
        a => format!("{a}\u{338}"),
    };
    spaced(&negated)
}

/// Relations, arrows and operators with room either side, as TeX sets them:
/// `a\leq b` is "a ≤ b".
fn spaced(symbol: &str) -> String {
    const SPACED: &str = "≤≥⩽⩾≠≈∼≃≅≡∝≪≫≺≻⪯⪰∈∉∋⊂⊃⊆⊇⊊⊑∣∤∥⊢⊣⊨≐≔≜≍→←↔⇒⇐⇔⟹⟸⟺↦⟶⟵⟼↪⇀⇝×÷±∓∪∩∧∨⊕⊖⊗⊙∖≢≮≯≁⊄⊈";
    let mut chars = symbol.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if SPACED.contains(c) => format!(" {c} "),
        _ => symbol.to_string(),
    }
}

/// Commands whose argument is text, shown as it is.
const WRAPPERS: &[&str] = &[
    "text",
    "textrm",
    "textit",
    "textbf",
    "texttt",
    "textsf",
    "mathrm",
    "mathit",
    "mathbf",
    "mathsf",
    "mathtt",
    "mathnormal",
    "boldsymbol",
    "bm",
    "mbox",
    "hbox",
    "emph",
    "pmb",
    "mathop",
    "underline",
    "overline",
    "boxed",
    "mathfrak",
];

/// Commands that change how a formula's set, not what it says.
const IGNORED: &[&str] = &[
    "displaystyle",
    "textstyle",
    "scriptstyle",
    "scriptscriptstyle",
    "limits",
    "nolimits",
    "nonumber",
    "notag",
    "quad",
    "qquad",
    "relax",
    "allowbreak",
    "noindent",
    "centering",
    "rm",
    "it",
    "bf",
    "sf",
    "tt",
    "cal",
    "small",
    "large",
    "Large",
    "normalsize",
    "hfill",
];

/// Accents, as combining characters after a single letter.
const ACCENTS: &[(&str, char)] = &[
    ("hat", '\u{302}'),
    ("widehat", '\u{302}'),
    ("bar", '\u{304}'),
    ("vec", '\u{20D7}'),
    ("tilde", '\u{303}'),
    ("widetilde", '\u{303}'),
    ("dot", '\u{307}'),
    ("ddot", '\u{308}'),
    ("acute", '\u{301}'),
    ("grave", '\u{300}'),
    ("check", '\u{30C}'),
    ("breve", '\u{306}'),
];

const SYMBOLS: &[(&str, &str)] = &[
    // Greek.
    ("alpha", "α"),
    ("beta", "β"),
    ("gamma", "γ"),
    ("delta", "δ"),
    ("epsilon", "ϵ"),
    ("varepsilon", "ε"),
    ("zeta", "ζ"),
    ("eta", "η"),
    ("theta", "θ"),
    ("vartheta", "ϑ"),
    ("iota", "ι"),
    ("kappa", "κ"),
    ("lambda", "λ"),
    ("mu", "μ"),
    ("nu", "ν"),
    ("xi", "ξ"),
    ("pi", "π"),
    ("varpi", "ϖ"),
    ("rho", "ρ"),
    ("varrho", "ϱ"),
    ("sigma", "σ"),
    ("varsigma", "ς"),
    ("tau", "τ"),
    ("upsilon", "υ"),
    ("phi", "ϕ"),
    ("varphi", "φ"),
    ("chi", "χ"),
    ("psi", "ψ"),
    ("omega", "ω"),
    ("Gamma", "Γ"),
    ("Delta", "Δ"),
    ("Theta", "Θ"),
    ("Lambda", "Λ"),
    ("Xi", "Ξ"),
    ("Pi", "Π"),
    ("Sigma", "Σ"),
    ("Upsilon", "Υ"),
    ("Phi", "Φ"),
    ("Psi", "Ψ"),
    ("Omega", "Ω"),
    // Big operators.
    ("sum", "∑"),
    ("prod", "∏"),
    ("coprod", "∐"),
    ("int", "∫"),
    ("iint", "∬"),
    ("iiint", "∭"),
    ("oint", "∮"),
    ("bigcup", "⋃"),
    ("bigcap", "⋂"),
    ("bigoplus", "⨁"),
    ("bigotimes", "⨂"),
    ("bigvee", "⋁"),
    ("bigwedge", "⋀"),
    // Operators.
    ("times", "×"),
    ("div", "÷"),
    ("cdot", "·"),
    ("cdots", "⋯"),
    ("ldots", "…"),
    ("dots", "…"),
    ("dotsc", "…"),
    ("dotsb", "⋯"),
    ("vdots", "⋮"),
    ("ddots", "⋱"),
    ("pm", "±"),
    ("mp", "∓"),
    ("ast", "∗"),
    ("star", "⋆"),
    ("circ", "∘"),
    ("bullet", "•"),
    ("oplus", "⊕"),
    ("ominus", "⊖"),
    ("otimes", "⊗"),
    ("odot", "⊙"),
    ("cup", "∪"),
    ("cap", "∩"),
    ("setminus", "∖"),
    ("wedge", "∧"),
    ("land", "∧"),
    ("vee", "∨"),
    ("lor", "∨"),
    ("neg", "¬"),
    ("lnot", "¬"),
    ("nabla", "∇"),
    ("partial", "∂"),
    ("prime", "′"),
    ("dagger", "†"),
    ("ddagger", "‡"),
    ("oslash", "⊘"),
    ("wr", "≀"),
    ("amalg", "⨿"),
    // Relations.
    ("leq", "≤"),
    ("le", "≤"),
    ("leqslant", "⩽"),
    ("geq", "≥"),
    ("ge", "≥"),
    ("geqslant", "⩾"),
    ("neq", "≠"),
    ("ne", "≠"),
    ("approx", "≈"),
    ("sim", "∼"),
    ("simeq", "≃"),
    ("cong", "≅"),
    ("equiv", "≡"),
    ("propto", "∝"),
    ("ll", "≪"),
    ("gg", "≫"),
    ("prec", "≺"),
    ("succ", "≻"),
    ("preceq", "⪯"),
    ("succeq", "⪰"),
    ("in", "∈"),
    ("notin", "∉"),
    ("ni", "∋"),
    ("subset", "⊂"),
    ("supset", "⊃"),
    ("subseteq", "⊆"),
    ("supseteq", "⊇"),
    ("subsetneq", "⊊"),
    ("sqsubseteq", "⊑"),
    ("mid", "∣"),
    ("nmid", "∤"),
    ("parallel", "∥"),
    ("perp", "⊥"),
    ("vdash", "⊢"),
    ("dashv", "⊣"),
    ("models", "⊨"),
    ("doteq", "≐"),
    ("coloneqq", "≔"),
    ("triangleq", "≜"),
    ("asymp", "≍"),
    // Arrows.
    ("to", "→"),
    ("rightarrow", "→"),
    ("leftarrow", "←"),
    ("gets", "←"),
    ("leftrightarrow", "↔"),
    ("Rightarrow", "⇒"),
    ("Leftarrow", "⇐"),
    ("Leftrightarrow", "⇔"),
    ("implies", "⟹"),
    ("impliedby", "⟸"),
    ("iff", "⟺"),
    ("mapsto", "↦"),
    ("longrightarrow", "⟶"),
    ("longleftarrow", "⟵"),
    ("longmapsto", "⟼"),
    ("Longrightarrow", "⟹"),
    ("uparrow", "↑"),
    ("downarrow", "↓"),
    ("updownarrow", "↕"),
    ("hookrightarrow", "↪"),
    ("rightharpoonup", "⇀"),
    ("nearrow", "↗"),
    ("searrow", "↘"),
    ("leadsto", "⇝"),
    // Brackets and delimiters.
    ("langle", "⟨"),
    ("rangle", "⟩"),
    ("lfloor", "⌊"),
    ("rfloor", "⌋"),
    ("lceil", "⌈"),
    ("rceil", "⌉"),
    ("lbrace", "{"),
    ("rbrace", "}"),
    ("lvert", "|"),
    ("rvert", "|"),
    ("vert", "|"),
    ("lVert", "‖"),
    ("rVert", "‖"),
    ("Vert", "‖"),
    ("backslash", "\\"),
    // Letters and the rest.
    ("infty", "∞"),
    ("emptyset", "∅"),
    ("varnothing", "∅"),
    ("forall", "∀"),
    ("exists", "∃"),
    ("nexists", "∄"),
    ("aleph", "ℵ"),
    ("hbar", "ℏ"),
    ("ell", "ℓ"),
    ("Re", "ℜ"),
    ("Im", "ℑ"),
    ("wp", "℘"),
    ("angle", "∠"),
    ("triangle", "△"),
    ("square", "□"),
    ("Box", "□"),
    ("checkmark", "✓"),
    ("top", "⊤"),
    ("bot", "⊥"),
    ("therefore", "∴"),
    ("because", "∵"),
    ("degree", "°"),
    ("S", "§"),
    ("P", "¶"),
    ("%", "%"),
    // Spacing.
    ("quad", " "),
    ("qquad", " "),
    ("enspace", " "),
    ("thinspace", " "),
    // Functions, upright as they are.
    ("sin", " sin "),
    ("cos", " cos "),
    ("tan", " tan "),
    ("cot", " cot "),
    ("sec", " sec "),
    ("csc", " csc "),
    ("arcsin", " arcsin "),
    ("arccos", " arccos "),
    ("arctan", " arctan "),
    ("sinh", " sinh "),
    ("cosh", " cosh "),
    ("tanh", " tanh "),
    ("log", " log "),
    ("ln", " ln "),
    ("lg", " lg "),
    ("exp", " exp "),
    ("lim", "lim"),
    ("liminf", "lim inf"),
    ("limsup", "lim sup"),
    ("max", "max"),
    ("min", "min"),
    ("sup", "sup"),
    ("inf", "inf"),
    ("arg", "arg"),
    ("argmax", "argmax"),
    ("argmin", "argmin"),
    ("det", "det"),
    ("dim", "dim"),
    ("ker", "ker"),
    ("deg", "deg"),
    ("gcd", "gcd"),
    ("Pr", "Pr"),
    ("mod", " mod "),
    ("bmod", " mod "),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_formulas_as_text() {
        for (tex, text) in [
            (r"x^2 + y^2 = z^2", "x² + y² = z²"),
            (r"\frac{a+b}{2}", "(a+b)/2"),
            (r"\frac{1}{n}", "1/n"),
            (r"\frac{QK^T}{\sqrt{d_k}}", "QKᵀ/√(dₖ)"),
            (r"e^{ix} = \cos x + i\sin x", "eⁱˣ = cos x + i sin x"),
            (r"\sum_{i=1}^{n} i", "∑ᵢ₌₁ⁿ i"),
            (r"\alpha \leq \beta", "α ≤ β"),
            (r"\sqrt{x^2+1}", "√(x²+1)"),
            (r"\sqrt[3]{x}", "∛x"),
            (r"\mathbb{R}^n", "ℝⁿ"),
            (r"O(n \log n)", "O(n log n)"),
            (r"\hat{x}", "x\u{302}"),
            (r"e^{i\pi} + 1 = 0", "e^(iπ) + 1 = 0"),
            (r"\left( \frac{x}{y} \right)", "(x/y)"),
            (r"{\displaystyle a\not= b}", "a ≠ b"),
            (r"f'(x)", "f′(x)"),
            (r"\text{if } x > 0", "if x > 0"),
            (r"a_{ij}", "aᵢⱼ"),
            (r"x_{\max}", "xₘₐₓ"),
            (r"a\to b", "a → b"),
            (r"\unknown{x}", "unknownx"),
        ] {
            assert_eq!(to_unicode(tex), text, "{tex}");
        }
    }

    #[test]
    fn finds_formulas_in_text() {
        assert_eq!(
            in_text(r"Let $x^2$ be \(\alpha\), and $$\sum_i a_i$$."),
            "Let x² be α, and ∑ᵢ aᵢ."
        );
        // Money stays money.
        assert_eq!(in_text("It costs $5 and $10."), "It costs $5 and $10.");
        assert_eq!(in_text("From $5 to $x$."), "From $5 to x.");
        assert_eq!(in_text(r"C:\Users\me"), r"C:\Users\me");
    }
}
