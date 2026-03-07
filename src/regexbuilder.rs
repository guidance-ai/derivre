use std::fmt::Debug;
use std::hash::Hash;

use crate::HashMap;
use anyhow::{ensure, Result};
use regex_syntax::ParserBuilder;

use crate::{
    ast::{
        byteset_256, byteset_clear, byteset_contains, byteset_from_range, byteset_set, Expr,
        ExprSet,
    },
    mapper::map_ast,
    pp::{byte_to_string, byteset_to_string},
    simplify::ConcatElement,
    ExprRef, Regex,
};

#[derive(Clone)]
pub struct RegexBuilder {
    parser_builder: ParserBuilder,
    exprset: ExprSet,
    string_escape_caches: HashMap<StringEscapeOptions, HashMap<ExprRef, ExprRef>>,
    json_quote_options_cache: HashMap<JsonQuoteOptions, StringEscapeOptions>,
}

/// Fallback escape format for bytes in [`StringEscapeOptions::must_escape`]
/// that have no entry in [`StringEscapeOptions::single_char_escapes`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FallbackEscapeFormat {
    /// `\uXXXX` — JSON-style 4-digit Unicode escape.
    /// Only valid for bytes 0x00–0x7F; returns an error for higher bytes
    /// unless they have a single-char escape mapping.
    UnicodeXXXX,
    /// `\xHH` — 2-digit hex escape (Python, C, YAML double-quoted).
    /// Works for any byte 0x00–0xFF.
    HexHH,
    /// Bare 2-digit hex — just the escape prefix followed by `HH`.
    /// For example, with `escape_prefix: b'%'`, byte 0x20 becomes `%20`.
    BareHH,
    /// No fallback — must-escape bytes without a single-char mapping will
    /// be excluded from the output regex.
    None,
}

/// How the quote character itself is escaped within the string.
///
/// Also affects whether the escape prefix is active;
/// see [`StringEscapeOptions`] for the full rules.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum QuoteEscapeMethod {
    /// Backslash-escape: `'` → `\'`, `"` → `\"`.
    Backslash,
    /// Doubling: `'` → `''` (e.g., YAML single-quoted strings).
    Doubling,
    /// No special handling for the quote character. It is escaped like
    /// any other must-escape byte via single-char escapes or fallback.
    Normal,
}

/// Declarative description of a string literal escape grammar.
///
/// This struct describes how bytes are escaped within a string literal for a
/// given language/format. The [`RegexBuilder::string_escape`] method uses these
/// options to transform a regex R into R' such that strings matching R', when
/// unescaped according to this grammar, produce strings matching R.
///
/// # Escape prefix handling
///
/// The [`escape_prefix`](Self::escape_prefix) byte is treated as an escape
/// prefix whenever any of the following are true:
/// - [`single_char_escapes`](Self::single_char_escapes) is non-empty
/// - [`fallback_escape`](Self::fallback_escape) is not
///   [`None`](FallbackEscapeFormat::None)
/// - [`quote_escape`](Self::quote_escape) is
///   [`Backslash`](QuoteEscapeMethod::Backslash)
///
/// When the escape prefix is active, it is implicitly added to the set of
/// bytes that must be escaped. When none of the above conditions hold
/// (e.g., pure [`Doubling`](QuoteEscapeMethod::Doubling) with no other
/// escapes), the escape prefix byte passes through as a literal character.
///
/// # Normalization
///
/// The [`single_char_escapes`](Self::single_char_escapes) and
/// [`must_escape`](Self::must_escape) fields are sorted and deduplicated
/// before use, so insertion order does not affect behavior or caching.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StringEscapeOptions {
    /// Mappings from byte value to the character placed after the escape
    /// prefix. For example, `(0x0A, b'n')` means byte 0x0A is escaped as
    /// `\n` (when `escape_prefix` is `b'\\'`).
    /// The escape prefix byte should be listed here if it should be escaped
    /// by doubling (e.g., `(b'\\', b'\\')` for `\\` → `\\\\`). If not
    /// listed, the escape prefix is escaped via the fallback format.
    /// The quote character need not be listed for `Backslash` or `Doubling`
    /// quote escape modes (handled implicitly).
    pub single_char_escapes: Vec<(u8, u8)>,

    /// How to escape must-escape bytes that have no single-char mapping.
    pub fallback_escape: FallbackEscapeFormat,

    /// The byte used as escape prefix for single-char escapes and
    /// fallback formats. The escape prefix is implicitly added to
    /// must_escape when active.
    pub escape_prefix: u8,

    /// ASCII character used to delimit the string (e.g., `'"'`, `'\''`).
    pub quote_char: char,

    /// How the quote character itself is escaped inside the string body.
    pub quote_escape: QuoteEscapeMethod,

    /// Bytes that MUST be escaped in the string body. `quote_char` and
    /// (when applicable) the escape prefix are added implicitly.
    /// Bytes with neither a single-char mapping nor a fallback format
    /// will be silently excluded from the output regex.
    pub must_escape: Vec<u8>,

    /// When true, `quote_char` delimiters are not added around the result.
    /// The quote character is still escaped if it appears in the regex.
    pub raw_mode: bool,
}

impl StringEscapeOptions {
    /// Sort and deduplicate `single_char_escapes` and `must_escape`.
    /// Called automatically by [`RegexBuilder::string_escape`].
    pub fn normalize(&mut self) {
        self.single_char_escapes.sort();
        self.single_char_escapes.dedup();
        self.must_escape.sort();
        self.must_escape.dedup();
    }
    /// Build options equivalent to JSON string escaping (RFC 8259).
    ///
    /// Uses `\uXXXX` fallback, double-quote delimiter, backslash escaping,
    /// and single-char escapes for `\b`, `\f`, `\n`, `\r`, `\t`.
    /// Control characters 0x00–0x1F and 0x7F are in `must_escape`.
    pub fn json() -> Self {
        Self {
            single_char_escapes: vec![
                (0x08, b'b'),
                (0x0C, b'f'),
                (b'\n', b'n'),
                (b'\r', b'r'),
                (b'\t', b't'),
                (b'\\', b'\\'),
            ],
            fallback_escape: FallbackEscapeFormat::UnicodeXXXX,
            escape_prefix: b'\\',
            quote_char: '"',
            quote_escape: QuoteEscapeMethod::Backslash,
            must_escape: (0x00..=0x1Fu8).chain(std::iter::once(0x7Fu8)).collect(),
            raw_mode: false,
        }
    }

    /// Build options for JSON without `\uXXXX` fallback, in raw mode.
    ///
    /// Same as [`json()`](Self::json) but with
    /// [`FallbackEscapeFormat::None`] and `raw_mode: true`. Bytes that
    /// lack a single-char escape mapping (e.g., 0x01) cannot be represented
    /// and will be excluded from the output regex.
    pub fn json_raw() -> Self {
        Self {
            single_char_escapes: vec![
                (0x08, b'b'),
                (0x0C, b'f'),
                (b'\n', b'n'),
                (b'\r', b'r'),
                (b'\t', b't'),
                (b'\\', b'\\'),
            ],
            fallback_escape: FallbackEscapeFormat::None,
            escape_prefix: b'\\',
            quote_char: '"',
            quote_escape: QuoteEscapeMethod::Backslash,
            must_escape: (0x00..=0x1Fu8).chain(std::iter::once(0x7Fu8)).collect(),
            raw_mode: true,
        }
    }

    /// Build options for URL percent-encoding (RFC 3986).
    ///
    /// Uses `%HH` for all bytes outside the unreserved set
    /// (`A-Z`, `a-z`, `0-9`, `-`, `.`, `_`, `~`). No string delimiters.
    pub fn url_percent_encoding() -> Self {
        let unreserved: Vec<u8> = (b'A'..=b'Z')
            .chain(b'a'..=b'z')
            .chain(b'0'..=b'9')
            .chain([b'-', b'.', b'_', b'~'].into_iter())
            .collect();
        let must_escape: Vec<u8> = (0x00..=0xFFu8)
            .filter(|b| !unreserved.contains(b))
            .collect();
        Self {
            single_char_escapes: vec![],
            fallback_escape: FallbackEscapeFormat::BareHH,
            escape_prefix: b'%',
            quote_char: '"',
            quote_escape: QuoteEscapeMethod::Normal,
            must_escape,
            raw_mode: true,
        }
    }
}

/// Options for JSON string quoting (legacy API).
///
/// Internally converted to [`StringEscapeOptions`] and delegated to
/// [`RegexBuilder::string_escape`]. Backslash and double-quote escaping
/// are always enabled regardless of `allowed_escapes`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct JsonQuoteOptions {
    /// Which escape forms to allow (each char enables one):
    /// `n`, `r`, `b`, `t`, `f` — single-char control escapes;
    /// `u` — `\uXXXX` for 0x00–0x1F and 0x7F;
    /// `\`, `"` — accepted but have no effect (always enabled).
    pub allowed_escapes: String,

    /// When true, the wrapping `"..."` delimiters are omitted.
    pub raw_mode: bool,
}

impl JsonQuoteOptions {
    pub fn no_unicode_raw() -> Self {
        Self {
            // \uXXXX not allowed
            allowed_escapes: "nrbtf\\\"".to_string(),
            raw_mode: true,
        }
    }

    pub fn with_unicode_raw() -> Self {
        Self {
            // allow \uXXXX
            allowed_escapes: "nrbtf\\\"u".to_string(),
            raw_mode: true,
        }
    }

    pub fn regular() -> Self {
        Self {
            // allow \uXXXX
            allowed_escapes: "nrbtf\\\"u".to_string(),
            raw_mode: false,
        }
    }

    pub fn is_allowed(&self, b: u8) -> bool {
        self.allowed_escapes.as_bytes().contains(&b)
    }

    pub fn set_if_allowed(&self, bs: &mut [u32], b: u8) {
        if self.is_allowed(b) {
            byteset_set(bs, b as usize);
        }
    }

    /// Convert to [`StringEscapeOptions`].
    ///
    /// The `\\` and `"` entries in `allowed_escapes` are ignored; backslash
    /// and double-quote escaping are always enabled in the result.
    pub fn to_string_escape_options(&self) -> StringEscapeOptions {
        let escape_map: &[(u8, u8, u8)] = &[
            (b'b', 0x08, b'b'),
            (b'f', 0x0C, b'f'),
            (b'n', b'\n', b'n'),
            (b'r', b'\r', b'r'),
            (b't', b'\t', b't'),
            (b'\\', b'\\', b'\\'),
        ];

        let single_char_escapes: Vec<(u8, u8)> = escape_map
            .iter()
            .filter(|(key, _, _)| self.is_allowed(*key))
            .map(|(_, byte, esc)| (*byte, *esc))
            .collect();

        let fallback_escape = if self.is_allowed(b'u') {
            FallbackEscapeFormat::UnicodeXXXX
        } else {
            FallbackEscapeFormat::None
        };

        StringEscapeOptions {
            single_char_escapes,
            fallback_escape,
            escape_prefix: b'\\',
            quote_char: '"',
            quote_escape: QuoteEscapeMethod::Backslash,
            must_escape: (0x00..=0x1Fu8).chain(std::iter::once(0x7Fu8)).collect(),
            raw_mode: self.raw_mode,
        }
    }
}

#[derive(Clone)]
pub enum RegexAst {
    /// Intersection of the regexes
    And(Vec<RegexAst>),
    /// Union of the regexes
    Or(Vec<RegexAst>),
    /// Concatenation of the regexes
    Concat(Vec<RegexAst>),
    /// Matches the regex; should be at the end of the main regex.
    /// The length of the lookahead can be recovered from the engine.
    LookAhead(Box<RegexAst>),
    /// Matches everything the regex doesn't match.
    /// Can lead to invalid utf8.
    Not(Box<RegexAst>),
    /// Repeat the regex at least min times, at most max times
    /// u32::MAX means infinity
    Repeat(Box<RegexAst>, u32, u32),
    /// MultipleOf(d, s) matches if the input, interpreted as decimal ASCII number, is a multiple of d*10^-s.
    /// EmptyString is not included.
    MultipleOf(u32, u32),
    /// Matches the empty string. Same as Concat([]).
    EmptyString,
    /// Matches nothing. Same as Or([]).
    NoMatch,
    /// Compile the regex using the regex_syntax crate.
    /// This assumes the regex is implicitly anchored.
    /// It allows ^$ only at the beginning and end of the regex.
    Regex(String),
    /// Compile the regex using the regex_syntax crate, but do not assume it's anchored.
    /// This will add (.*) to the beginning and end of the regex if it doesn't already have
    /// anchors.
    SearchRegex(String),
    /// Matches this string only
    Literal(String),
    /// Matches this string of bytes only. Can lead to invalid utf8.
    ByteLiteral(Vec<u8>),
    /// Matches this byte only. If byte is not in 0..127, it may lead to invalid utf8.
    Byte(u8),
    /// Matches any byte in the set, expressed as bitset.
    /// Can lead to invalid utf8 if the set is not a subset of 0..127
    ByteSet(Vec<u32>),
    /// Quote the regex as a JSON string.
    /// For example, `[A-Z\n]+` becomes `([A-Z]|\\n)+`.
    ///
    /// Delegates to [`StringEscape`](RegexAst::StringEscape) via
    /// [`RegexBuilder::string_escape`]. See [`StringEscapeOptions`] for
    /// the full escape model.
    JsonQuote(Box<RegexAst>, JsonQuoteOptions),
    /// Escape the regex as a string literal using configurable escape options.
    ///
    /// Transforms a regex R into R' such that strings matching R', when
    /// unescaped according to the given [`StringEscapeOptions`], produce
    /// strings matching R. See [`StringEscapeOptions`] for the full
    /// escape model and configuration.
    StringEscape(Box<RegexAst>, StringEscapeOptions),
    /// Reference previously built regex
    ExprRef(ExprRef),
}

impl RegexAst {
    /// Regex is empty iff self ⊆ big
    pub fn contained_in(&self, big: &RegexAst) -> RegexAst {
        let small = self;
        RegexAst::And(vec![small.clone(), RegexAst::Not(Box::new(big.clone()))])
    }

    pub fn get_args(&self) -> &[RegexAst] {
        match self {
            RegexAst::And(asts) | RegexAst::Or(asts) | RegexAst::Concat(asts) => asts,
            RegexAst::LookAhead(ast)
            | RegexAst::Not(ast)
            | RegexAst::Repeat(ast, _, _)
            | RegexAst::JsonQuote(ast, _)
            | RegexAst::StringEscape(ast, _) => std::slice::from_ref(ast),
            RegexAst::EmptyString
            | RegexAst::MultipleOf(_, _)
            | RegexAst::NoMatch
            | RegexAst::Regex(_)
            | RegexAst::SearchRegex(_)
            | RegexAst::Literal(_)
            | RegexAst::ByteLiteral(_)
            | RegexAst::ExprRef(_)
            | RegexAst::Byte(_)
            | RegexAst::ByteSet(_) => &[],
        }
    }

    pub fn tag(&self) -> &'static str {
        match self {
            RegexAst::And(_) => "And",
            RegexAst::Or(_) => "Or",
            RegexAst::Concat(_) => "Concat",
            RegexAst::LookAhead(_) => "LookAhead",
            RegexAst::Not(_) => "Not",
            RegexAst::EmptyString => "EmptyString",
            RegexAst::NoMatch => "NoMatch",
            RegexAst::Regex(_) => "Regex",
            RegexAst::SearchRegex(_) => "SearchRegex",
            RegexAst::Literal(_) => "Literal",
            RegexAst::ByteLiteral(_) => "ByteLiteral",
            RegexAst::ExprRef(_) => "ExprRef",
            RegexAst::Repeat(_, _, _) => "Repeat",
            RegexAst::Byte(_) => "Byte",
            RegexAst::ByteSet(_) => "ByteSet",
            RegexAst::MultipleOf(_, _) => "MultipleOf",
            RegexAst::JsonQuote(_, _) => "JsonQuote",
            RegexAst::StringEscape(_, _) => "StringEscape",
        }
    }

    pub fn write_to_str(&self, dst: &mut String, max_len: usize, exprset: Option<&ExprSet>) {
        let mut todo = vec![Some(self)];
        while let Some(ast) = todo.pop() {
            if dst.len() >= max_len {
                dst.push_str("...");
                break;
            }
            if ast.is_none() {
                dst.push(')');
                continue;
            }
            let ast = ast.unwrap();
            dst.push_str(" (");
            dst.push_str(ast.tag());
            todo.push(None);
            match ast {
                RegexAst::And(_)
                | RegexAst::Or(_)
                | RegexAst::Concat(_)
                | RegexAst::LookAhead(_)
                | RegexAst::Not(_) => {}
                RegexAst::Byte(b) => {
                    dst.push(' ');
                    dst.push_str(&byte_to_string(*b));
                }
                RegexAst::ByteSet(bs) => {
                    dst.push(' ');
                    if bs.len() == 256 / 32 {
                        dst.push_str(&byteset_to_string(bs));
                    } else {
                        dst.push_str(&format!("invalid byteset len: {}", bs.len()))
                    }
                }
                RegexAst::SearchRegex(s) | RegexAst::Regex(s) => {
                    dst.push(' ');
                    write_regex(dst, s);
                }
                RegexAst::Literal(s) => {
                    dst.push_str(&format!(" {:?}", s));
                }
                RegexAst::ByteLiteral(s) => {
                    dst.push_str(&format!(" {:?}", String::from_utf8_lossy(s)));
                }
                RegexAst::ExprRef(r) => {
                    if let Some(es) = exprset {
                        let e_len = max_len.saturating_sub(dst.len());
                        dst.push_str(&format!(" {}", es.expr_to_string_max_len(*r, e_len)));
                    } else {
                        dst.push_str(&format!(" {}", r.as_usize()));
                    }
                }
                RegexAst::Repeat(_, min, max) => {
                    dst.push_str(&format!("{{{},{}}} ", min, max));
                }
                RegexAst::MultipleOf(d, s) => {
                    if *s == 0 {
                        dst.push_str(&format!(" % {} == 0 ", d));
                    } else {
                        dst.push_str(&format!(" % {}x10^-{} == 0", d, s));
                    }
                }
                RegexAst::JsonQuote(_, opts) => {
                    dst.push_str(&format!(" {:?}", opts));
                }
                RegexAst::StringEscape(_, opts) => {
                    dst.push_str(&format!(" {:?}", opts));
                }
                RegexAst::EmptyString | RegexAst::NoMatch => {}
            }
            for c in ast.get_args().iter().rev() {
                todo.push(Some(c));
            }
        }
    }
}

pub(crate) fn write_regex(dst: &mut String, regex: &str) {
    dst.push('/');
    let mut escaped = false;
    for c in regex.chars() {
        match c {
            '\\' if !escaped => {
                escaped = true;
                continue;
            }
            '/' => {
                dst.push('\\');
                dst.push(c);
            }
            '\n' => {
                dst.push_str("\\n");
            }
            '\r' => {
                dst.push_str("\\r");
            }
            '\t' => {
                dst.push_str("\\t");
            }
            _ => {
                if c < ' ' {
                    dst.push_str(&format!("\\x{:02X}", c as u32));
                } else {
                    if escaped {
                        dst.push('\\');
                    }
                    dst.push(c);
                }
            }
        }
        escaped = false;
    }
    if escaped {
        dst.push_str("\\\\");
    }
    dst.push('/');
}

impl Debug for RegexAst {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = String::new();
        self.write_to_str(&mut s, 512, None);
        write!(f, "{}", s)
    }
}

impl Default for RegexBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl RegexBuilder {
    pub fn new() -> Self {
        Self {
            parser_builder: ParserBuilder::new(),
            exprset: ExprSet::new(256),
            string_escape_caches: HashMap::default(),
            json_quote_options_cache: HashMap::default(),
        }
    }

    pub fn to_regex_limited(&self, r: ExprRef, max_fuel: u64) -> Result<Regex> {
        Regex::new_with_exprset(self.exprset.clone(), r, max_fuel)
    }

    pub fn to_regex(&self, r: ExprRef) -> Regex {
        Regex::new_with_exprset(self.exprset.clone(), r, u64::MAX).unwrap()
    }

    pub fn into_regex_limited(self, r: ExprRef, max_fuel: u64) -> Result<Regex> {
        Regex::new_with_exprset(self.exprset, r, max_fuel)
    }

    pub fn into_regex(self, r: ExprRef) -> Regex {
        Regex::new_with_exprset(self.exprset, r, u64::MAX).unwrap()
    }

    pub fn exprset(&self) -> &ExprSet {
        &self.exprset
    }

    pub fn into_exprset(self) -> ExprSet {
        self.exprset
    }

    pub fn reserve(&mut self, size: usize) {
        self.exprset.reserve(size);
    }

    /// Transform a regex for JSON string quoting.
    ///
    /// Converts `options` to [`StringEscapeOptions`] and delegates to
    /// [`string_escape`](Self::string_escape).
    pub fn json_quote(&mut self, e: ExprRef, options: &JsonQuoteOptions) -> Result<ExprRef> {
        for c in options.allowed_escapes.as_bytes() {
            ensure!(
                b"\"\\bfnrtu".contains(c),
                "invalid escape character in allowed_escapes: {}",
                *c as char
            );
        }
        let se_options = self
            .json_quote_options_cache
            .entry(options.clone())
            .or_insert_with(|| options.to_string_escape_options())
            .clone();
        self.string_escape(e, &se_options)
    }

    /// Transform a regex for string-literal escaping.
    ///
    /// Given regex R, produces R' such that strings matching R', when
    /// unescaped according to the given grammar, yield strings matching R.
    pub fn string_escape(&mut self, e: ExprRef, options: &StringEscapeOptions) -> Result<ExprRef> {
        // Normalize Vec fields for consistent Hash/Eq cache behavior
        let mut options = options.clone();
        options.normalize();

        ensure!(
            options.quote_char.is_ascii(),
            "quote_char must be ASCII, got U+{:04X}",
            options.quote_char as u32
        );

        // UnicodeXXXX can only represent bytes 0x00-0x7F; reject must_escape
        // bytes >= 0x80 unless they have a single-char escape mapping.
        if matches!(options.fallback_escape, FallbackEscapeFormat::UnicodeXXXX) {
            for &b in &options.must_escape {
                if b >= 0x80 {
                    ensure!(
                        options
                            .single_char_escapes
                            .iter()
                            .any(|(byte, _)| *byte == b),
                        "UnicodeXXXX fallback cannot represent byte 0x{:02X} (>= 0x80); \
                         use HexHH or add a single_char_escape for it",
                        b
                    );
                }
            }
        }
        let qc = options.quote_char as u8;
        let ep = options.escape_prefix;

        // Build a lookup table: byte -> Some(escape_char) for single-char escapes
        let mut escape_map = [None::<u8>; 256];
        for &(byte, esc) in &options.single_char_escapes {
            escape_map[byte as usize] = Some(esc);
        }

        // Build must_escape byteset (including implicit quote_char, and escape
        // prefix when the grammar uses it as an escape prefix)
        let mut must_escape_set = [false; 256];
        for &b in &options.must_escape {
            must_escape_set[b as usize] = true;
        }
        // The escape prefix is active when: there are single-char escapes,
        // there's a fallback escape format, or the quote char is backslash-escaped.
        let uses_escape_prefix = !options.single_char_escapes.is_empty()
            || !matches!(options.fallback_escape, FallbackEscapeFormat::None)
            || matches!(options.quote_escape, QuoteEscapeMethod::Backslash);
        if uses_escape_prefix {
            must_escape_set[ep as usize] = true;
        }
        if !matches!(options.quote_escape, QuoteEscapeMethod::Normal) {
            must_escape_set[qc as usize] = true;
        }

        // returns Some(X) iff b should be escaped as <ep>X (single-char)
        fn quote_single(
            b: u8,
            escape_map: &[Option<u8>; 256],
        ) -> Option<u8> {
            escape_map[b as usize]
        }

        // Collect the set of escape chars that appear after the escape prefix
        // for single-char escapes (used to build regex byte sets).
        fn single_escape_char_byteset(
            include_nl_byte: bool,
            qc: u8,
            uses_escape_prefix: bool,
            ep: u8,
            escape_map: &[Option<u8>; 256],
            must_escape_set: &[bool; 256],
            quote_escape: &QuoteEscapeMethod,
        ) -> Vec<u32> {
            let mut bs = byteset_256();
            // qc might be a control char (< 0x20); if so, include its escape
            // char in this byteset since it's in the fast-path control range.
            if uses_escape_prefix && qc < 0x20
                && !matches!(quote_escape, QuoteEscapeMethod::Normal)
            {
                byteset_set(&mut bs, qc as usize);
            }
            if uses_escape_prefix && ep < 0x20 {
                byteset_set(&mut bs, ep as usize);
            }
            for b in 0..=255u8 {
                if !must_escape_set[b as usize] {
                    continue;
                }
                if b == b'\n' && !include_nl_byte {
                    continue;
                }
                if let Some(esc) = escape_map[b as usize] {
                    byteset_set(&mut bs, esc as usize);
                }
            }
            bs
        }

        // all hex digits, including A/a or not (for \n → 0x0a, which has 'a')
        fn hex_byteset(include_a: bool) -> Vec<u32> {
            let mut hex_bs = byteset_256();
            for c in b"0123456789bcdefBCDEF" {
                byteset_set(&mut hex_bs, *c as usize);
            }
            if include_a {
                byteset_set(&mut hex_bs, b'A' as usize);
                byteset_set(&mut hex_bs, b'a' as usize);
            }
            hex_bs
        }

        // Build a regex for the fallback escape of all control-range bytes
        fn mk_fallback_all_ctrl(
            exprset: &mut ExprSet,
            include_nl: bool,
            options: &StringEscapeOptions,
        ) -> ExprRef {
            let prefix_str = match options.fallback_escape {
                FallbackEscapeFormat::UnicodeXXXX => "u00",
                FallbackEscapeFormat::HexHH => "x",
                FallbackEscapeFormat::BareHH => "",
                FallbackEscapeFormat::None => return ExprRef::NO_MATCH,
            };
            let hex_part = if include_nl {
                let hex0 = exprset.mk_byte_set(&byteset_from_range(b'0', b'1'));
                let hex1 = exprset.mk_byte_set(&hex_byteset(include_nl));
                exprset.mk_concat(hex0, hex1)
            } else {
                let n0 = exprset.mk_byte(b'0');
                let n1 = exprset.mk_byte(b'1');
                let hex0 = exprset.mk_byte_set(&hex_byteset(false));
                let hex0 = exprset.mk_concat(n0, hex0);
                let hex1 = exprset.mk_byte_set(&hex_byteset(true));
                let hex1 = exprset.mk_concat(n1, hex1);
                exprset.mk_or(&mut vec![hex0, hex1])
            };
            if prefix_str.is_empty() {
                hex_part
            } else {
                let prefix = exprset.mk_literal(prefix_str);
                exprset.mk_concat(prefix, hex_part)
            }
        }

        // Build regex for escaping all control characters (0x00-0x1F range)
        fn quote_all_ctrl(
            exprset: &mut ExprSet,
            include_nl: bool,
            qc: u8,
            uses_escape_prefix: bool,
            ep: u8,
            escape_map: &[Option<u8>; 256],
            must_escape_set: &[bool; 256],
            options: &StringEscapeOptions,
        ) -> ExprRef {
            let esc_prefix = exprset.mk_byte(ep);
            let single_esc = exprset.mk_byte_set(&single_escape_char_byteset(
                include_nl,
                qc,
                uses_escape_prefix,
                ep,
                escape_map,
                must_escape_set,
                &options.quote_escape,
            ));
            let fallback = mk_fallback_all_ctrl(exprset, include_nl, options);

            let combined = exprset.mk_or(&mut vec![fallback, single_esc]);
            exprset.mk_concat(esc_prefix, combined)
        }

        fn quote_byteset(
            exprset: &mut ExprSet,
            bs: Vec<u32>,
            qc: u8,
            uses_escape_prefix: bool,
            ep: u8,
            escape_map: &[Option<u8>; 256],
            must_escape_set: &[bool; 256],
            options: &StringEscapeOptions,
        ) -> ExprRef {
            let has_fallback = !matches!(options.fallback_escape, FallbackEscapeFormat::None);
            let esc_prefix = exprset.mk_byte(ep);

            let used_fast_path;
            let quoted = if bs[0] == !(1 << b'\n') {
                // everything except for \n
                used_fast_path = true;
                quote_all_ctrl(
                    exprset,
                    false,
                    qc,
                    uses_escape_prefix,
                    ep,
                    escape_map,
                    must_escape_set,
                    options,
                )
            } else if bs[0] == 0xffff_ffff {
                // everything
                used_fast_path = true;
                quote_all_ctrl(
                    exprset,
                    true,
                    qc,
                    uses_escape_prefix,
                    ep,
                    escape_map,
                    must_escape_set,
                    options,
                )
            } else {
                used_fast_path = false;
                let mut quoted_bs = byteset_256();
                let mut other_bytes = vec![];
                // Handle all must-escape bytes that are in the byteset
                for b in 0..=255u8 {
                    if !byteset_contains(&bs, b as usize) {
                        continue;
                    }
                    if !must_escape_set[b as usize] {
                        continue;
                    }
                    if b == qc && !matches!(options.quote_escape, QuoteEscapeMethod::Normal) {
                        continue;
                    }
                    if let Some(q) = quote_single(b, escape_map) {
                        byteset_set(&mut quoted_bs, q as usize);
                    }
                    if has_fallback {
                        let other = exprset.mk_literal(&format!("{:02x}", b));
                        other_bytes.push(other);
                        let other = exprset.mk_literal(&format!("{:02X}", b));
                        other_bytes.push(other);
                    }
                }

                let quoted_bs = exprset.mk_byte_set(&quoted_bs);

                let fallback_part = if has_fallback {
                    let other_bytes = exprset.mk_or(&mut other_bytes);
                    match options.fallback_escape {
                        FallbackEscapeFormat::UnicodeXXXX => {
                            let prefix = exprset.mk_literal("u00");
                            exprset.mk_concat(prefix, other_bytes)
                        }
                        FallbackEscapeFormat::HexHH => {
                            let prefix = exprset.mk_literal("x");
                            exprset.mk_concat(prefix, other_bytes)
                        }
                        FallbackEscapeFormat::BareHH => other_bytes,
                        FallbackEscapeFormat::None => unreachable!(),
                    }
                } else {
                    ExprRef::NO_MATCH
                };

                let quoted_or_other = exprset.mk_or(&mut vec![quoted_bs, fallback_part]);
                exprset.mk_concat(esc_prefix, quoted_or_other)
            };

            let mut bs_passthrough = bs;
            let mut alts = vec![quoted];
            // Handle quote_char for non-Normal modes
            if !matches!(options.quote_escape, QuoteEscapeMethod::Normal) {
                if byteset_contains(&bs_passthrough, qc as usize) {
                    match options.quote_escape {
                        QuoteEscapeMethod::Backslash => {
                            let escaped = format!("{}{}", ep as char, qc as char);
                            alts.push(exprset.mk_literal(&escaped));
                        }
                        QuoteEscapeMethod::Doubling => {
                            let doubled = format!("{}{}", qc as char, qc as char);
                            alts.push(exprset.mk_literal(&doubled));
                        }
                        QuoteEscapeMethod::Normal => unreachable!(),
                    }
                    byteset_clear(&mut bs_passthrough, qc as usize);
                }
            }
            // Handle must-escape bytes >= 0x20 that weren't covered by the
            // control-char fast path (the slow path already handles all 256 bytes)
            if used_fast_path {
                for b in 0x20..=0xFFu8 {
                    if b == qc && !matches!(options.quote_escape, QuoteEscapeMethod::Normal) {
                        continue;
                    }
                    if !byteset_contains(&bs_passthrough, b as usize)
                        || !must_escape_set[b as usize]
                    {
                        continue;
                    }
                    if let Some(q) = quote_single(b, escape_map) {
                        let esc = exprset.mk_literal(&format!("{}{}", ep as char, q as char));
                        alts.push(esc);
                    }
                    if has_fallback {
                        let lit_lower = format!("{:02x}", b);
                        let lit_upper = format!("{:02X}", b);
                        let prefix = match options.fallback_escape {
                            FallbackEscapeFormat::UnicodeXXXX => format!("{}u00", ep as char),
                            FallbackEscapeFormat::HexHH => format!("{}x", ep as char),
                            FallbackEscapeFormat::BareHH => format!("{}", ep as char),
                            FallbackEscapeFormat::None => unreachable!(),
                        };
                        alts.push(exprset.mk_literal(&format!("{}{}", prefix, lit_lower)));
                        alts.push(exprset.mk_literal(&format!("{}{}", prefix, lit_upper)));
                    }
                    byteset_clear(&mut bs_passthrough, b as usize);
                }
            }
            // Remove all remaining must-escape bytes from the pass-through set
            for b in 0..=255u8 {
                if must_escape_set[b as usize] {
                    byteset_clear(&mut bs_passthrough, b as usize);
                }
            }
            let bs_passthrough = exprset.mk_byte_set(&bs_passthrough);
            alts.push(bs_passthrough);
            exprset.mk_or(&mut alts)
        }

        fn byte_needs_escape(
            b: u8,
            must_escape_set: &[bool; 256],
        ) -> bool {
            must_escape_set[b as usize]
        }

        let cache = self
            .string_escape_caches
            .entry(options.clone())
            .or_default();
        let options = &options;
        let r = self.exprset.map(
            e,
            cache,
            false,
            |e| e,
            |exprset, args, e| -> ExprRef {
                match exprset.get(e) {
                    Expr::ByteSet(bs) => {
                        // Check if any byte in the set needs escaping
                        let needs = (0..=255u8).any(|b| {
                            byteset_contains(bs, b as usize)
                                && byte_needs_escape(b, &must_escape_set)
                        });
                        if needs {
                            let bs = bs.to_vec();
                            quote_byteset(
                                exprset,
                                bs,
                                qc,
                                uses_escape_prefix,
                                ep,
                                &escape_map,
                                &must_escape_set,
                                options,
                            )
                        } else {
                            e
                        }
                    }
                    Expr::Byte(b) => {
                        if byte_needs_escape(b, &must_escape_set) {
                            quote_byteset(
                                exprset,
                                byteset_from_range(b, b),
                                qc,
                                uses_escape_prefix,
                                ep,
                                &escape_map,
                                &must_escape_set,
                                options,
                            )
                        } else {
                            e
                        }
                    }
                    Expr::ByteConcat(_, bytes, args0) => {
                        if bytes
                            .iter()
                            .any(|b| byte_needs_escape(*b, &must_escape_set))
                        {
                            let mut acc = vec![];
                            let mut idx = 0;
                            let bytes = bytes.to_vec();
                            while idx < bytes.len() {
                                let idx0 = idx;
                                while idx < bytes.len()
                                    && !byte_needs_escape(
                                        bytes[idx],
                                        &must_escape_set,
                                    )
                                {
                                    idx += 1;
                                }
                                let slice = &bytes[idx0..idx];
                                if !slice.is_empty() {
                                    ConcatElement::Bytes(slice).push_owned_to(&mut acc);
                                }
                                if idx < bytes.len() {
                                    let b = bytes[idx];
                                    let q = quote_byteset(
                                        exprset,
                                        byteset_from_range(b, b),
                                        qc,
                                        uses_escape_prefix,
                                        ep,
                                        &escape_map,
                                        &must_escape_set,
                                        options,
                                    );
                                    ConcatElement::Expr(q).push_owned_to(&mut acc);
                                    idx += 1;
                                }
                            }
                            exprset._mk_concat_vec(acc)
                        } else if args[0] == args0 {
                            e
                        } else {
                            let copy = bytes.to_vec();
                            exprset.mk_byte_concat(&copy, args[0])
                        }
                    }
                    // always identity
                    Expr::EmptyString | Expr::NoMatch | Expr::RemainderIs { .. } => e,
                    // if all args map to themselves, return back the same expression
                    x if x.args() == args => e,
                    // otherwise, actually map the args
                    Expr::And(_, _) => exprset.mk_and(args),
                    Expr::Or(_, _) => exprset.mk_or(args),
                    Expr::Concat(_, _) => exprset.mk_concat(args[0], args[1]),
                    Expr::Not(_, _) => exprset.mk_not(args[0]),
                    Expr::Lookahead(_, _, _) => exprset.mk_lookahead(args[0], 0),
                    Expr::Repeat(_, _, min, max) => exprset.mk_repeat(args[0], min, max),
                }
            },
        );

        let qc_byte = self.exprset.mk_byte(qc);
        let r = if options.raw_mode {
            r
        } else {
            self.exprset.mk_concat_vec(&[qc_byte, r, qc_byte])
        };
        Ok(r)
    }

    pub fn mk_regex(&mut self, s: &str) -> Result<ExprRef> {
        let parser = self.parser_builder.build();
        self.exprset.parse_expr(parser, s, false)
    }

    pub fn mk_regex_for_serach(&mut self, s: &str) -> Result<ExprRef> {
        let parser = self.parser_builder.build();
        self.exprset.parse_expr(parser, s, true)
    }

    pub fn mk_regex_and(&mut self, s: &[&str]) -> Result<ExprRef> {
        let args = s
            .iter()
            .map(|s| Ok(RegexAst::ExprRef(self.mk_regex(s)?)))
            .collect::<Result<Vec<_>>>()?;
        self.mk(&RegexAst::And(args))
    }

    pub fn mk_contained_in(&mut self, small: &str, big: &str) -> Result<ExprRef> {
        let a = RegexAst::ExprRef(self.mk_regex(small)?);
        let b = RegexAst::ExprRef(self.mk_regex(big)?);
        self.mk(&a.contained_in(&b))
    }

    pub fn mk_contained_in_ast(&mut self, small: &RegexAst, big: &RegexAst) -> Result<ExprRef> {
        let a = RegexAst::ExprRef(self.mk(small)?);
        let b = RegexAst::ExprRef(self.mk(big)?);
        self.mk(&a.contained_in(&b))
    }

    pub fn is_contained_in(&mut self, small: &str, big: &str, max_fuel: u64) -> Result<bool> {
        let r = self.mk_contained_in(small, big)?;
        Ok(self.clone().to_regex_limited(r, max_fuel)?.always_empty())
    }

    pub fn mk_prefix_tree(&mut self, branches: Vec<(Vec<u8>, ExprRef)>) -> Result<ExprRef> {
        Ok(self.exprset.mk_prefix_tree(branches))
    }

    pub fn mk(&mut self, ast: &RegexAst) -> Result<ExprRef> {
        map_ast(
            ast,
            |ast| ast.get_args(),
            |ast, new_args| {
                let r = match ast {
                    RegexAst::Regex(s) => self.mk_regex(s)?,
                    RegexAst::SearchRegex(s) => self.mk_regex_for_serach(s)?,
                    RegexAst::JsonQuote(_, opts) => self.json_quote(new_args[0], opts)?,
                    RegexAst::StringEscape(_, opts) => self.string_escape(new_args[0], opts)?,
                    RegexAst::ExprRef(r) => {
                        ensure!(self.exprset.is_valid(*r), "invalid ref");
                        *r
                    }
                    RegexAst::And(_) => self.exprset.mk_and(new_args),
                    RegexAst::Or(_) => self.exprset.mk_or(new_args),
                    RegexAst::Concat(_) => self.exprset.mk_concat_vec(new_args),
                    RegexAst::Not(_) => self.exprset.mk_not(new_args[0]),
                    RegexAst::LookAhead(_) => self.exprset.mk_lookahead(new_args[0], 0),
                    RegexAst::EmptyString => ExprRef::EMPTY_STRING,
                    RegexAst::NoMatch => ExprRef::NO_MATCH,
                    RegexAst::Literal(s) => self.exprset.mk_literal(s),
                    RegexAst::ByteLiteral(s) => self.exprset.mk_byte_literal(s),
                    RegexAst::Repeat(_, min, max) => {
                        self.exprset.mk_repeat(new_args[0], *min, *max)
                    }
                    RegexAst::MultipleOf(d, s) => {
                        ensure!(*d > 0, "invalid multiple of");
                        self.exprset.mk_remainder_is(*d, *d, *s, false)
                    }
                    RegexAst::Byte(b) => self.exprset.mk_byte(*b),
                    RegexAst::ByteSet(bs) => {
                        ensure!(
                            bs.len() == self.exprset.alphabet_words,
                            "invalid byteset len"
                        );
                        self.exprset.mk_byte_set(bs)
                    }
                };
                Ok(r)
            },
        )
    }

    pub fn is_nullable(&self, r: ExprRef) -> bool {
        self.exprset.is_nullable(r)
    }
}

// regex flags; docs copied from regex_syntax crate
impl RegexBuilder {
    /// Enable or disable the Unicode flag (`u`) by default.
    ///
    /// By default this is **enabled**. It may alternatively be selectively
    /// disabled in the regular expression itself via the `u` flag.
    ///
    /// Note that unless `utf8` is disabled (it's enabled by default), a
    /// regular expression will fail to parse if Unicode mode is disabled and a
    /// sub-expression could possibly match invalid UTF-8.
    pub fn unicode(&mut self, unicode: bool) -> &mut Self {
        self.parser_builder.unicode(unicode);
        self
    }

    /// When disabled, translation will permit the construction of a regular
    /// expression that may match invalid UTF-8.
    ///
    /// When enabled (the default), the translator is guaranteed to produce an
    /// expression that, for non-empty matches, will only ever produce spans
    /// that are entirely valid UTF-8 (otherwise, the translator will return an
    /// error).
    pub fn utf8(&mut self, utf8: bool) -> &mut Self {
        self.parser_builder.utf8(utf8);
        self
    }

    /// Enable verbose mode in the regular expression.
    ///
    /// When enabled, verbose mode permits insignificant whitespace in many
    /// places in the regular expression, as well as comments. Comments are
    /// started using `#` and continue until the end of the line.
    ///
    /// By default, this is disabled. It may be selectively enabled in the
    /// regular expression by using the `x` flag regardless of this setting.
    pub fn ignore_whitespace(&mut self, ignore_whitespace: bool) -> &mut Self {
        self.parser_builder.ignore_whitespace(ignore_whitespace);
        self
    }

    /// Enable or disable the case insensitive flag by default.
    ///
    /// By default this is disabled. It may alternatively be selectively
    /// enabled in the regular expression itself via the `i` flag.
    pub fn case_insensitive(&mut self, case_insensitive: bool) -> &mut Self {
        self.parser_builder.case_insensitive(case_insensitive);
        self
    }

    /// Enable or disable the "dot matches any character" flag by default.
    ///
    /// By default this is disabled. It may alternatively be selectively
    /// enabled in the regular expression itself via the `s` flag.
    pub fn dot_matches_new_line(&mut self, dot_matches_new_line: bool) -> &mut Self {
        self.parser_builder
            .dot_matches_new_line(dot_matches_new_line);
        self
    }
}
