use std::fmt::Debug;

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

/// Describes the hex-based fallback encoding for bytes without an explicit
/// escape sequence.
///
/// The fallback format is: `prefix` + 2 case-insensitive hex digits + `suffix`.
/// For example, a `FallbackFormat` with prefix `b"\\u00"`, empty suffix, and
/// `valid_byte_ranges: Some(vec![(0x00, 0x7F)])` produces `\u001F` for byte
/// 0x1F and rejects bytes above 0x7F.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FallbackFormat {
    /// Prefix bytes before the 2 hex digits.
    /// E.g. `b"\\u00"` for JSON, `b"%"` for URL, `b"&#x"` for XML.
    pub prefix: Vec<u8>,

    /// Suffix bytes after the 2 hex digits.
    /// E.g. `b";"` for XML `&#xNN;`. Empty for most formats.
    pub suffix: Vec<u8>,

    /// Inclusive byte ranges eligible for fallback encoding.
    /// Each `(lo, hi)` pair means bytes `lo..=hi` can use this fallback.
    /// Must-escape bytes outside all ranges must have an explicit
    /// `escape_sequence` or they cause an error. `None` means all byte
    /// values (0x00–0xFF) are valid.
    ///
    /// Examples:
    /// - JSON `\u00XX`: `Some(vec![(0x00, 0x7F)])` — code points only
    /// - URL `%HH`: `None` — all bytes
    /// - XML `&#xNN;`: `Some(vec![(0x09, 0x0A), (0x0D, 0x0D), (0x20, 0xFF)])`
    ///   — only valid XML 1.0 characters
    pub valid_byte_ranges: Option<Vec<(u8, u8)>>,
}

impl FallbackFormat {
    /// Check whether a byte is eligible for this fallback format.
    pub fn covers_byte(&self, b: u8) -> bool {
        match &self.valid_byte_ranges {
            None => true,
            Some(ranges) => ranges.iter().any(|&(lo, hi)| b >= lo && b <= hi),
        }
    }
}

/// Declarative description of a string literal escape grammar.
///
/// This struct describes how bytes are escaped within a string literal for a
/// given language/format. The [`RegexBuilder::string_escape`] method uses these
/// options to transform a regex R into R' such that strings matching R', when
/// unescaped according to this grammar, produce strings matching R.
///
/// Each byte in [`must_escape`](Self::must_escape) is represented by its
/// [`escape_sequences`](Self::escape_sequences) entry and/or its
/// [`fallback`](Self::fallback) hex form. When both exist, both are accepted
/// (e.g., JSON `\b` and `\u0008` are both valid for byte 0x08). Must-escape
/// bytes with neither form are excluded from the output regex — the byte
/// simply cannot be represented. However, if `fallback` has
/// [`valid_byte_ranges`](FallbackFormat::valid_byte_ranges) set, a must-escape
/// byte outside those ranges without an explicit escape will cause
/// [`string_escape`](RegexBuilder::string_escape) to return an error rather
/// than silently excluding it.
///
/// Delimiter wrapping (e.g., surrounding with `"`) is NOT handled here;
/// that is the caller's responsibility.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StringEscapeOptions {
    /// Explicit escape sequences for specific bytes.
    /// Each entry `(byte, sequence)` means: when `byte` appears in the input
    /// and needs escaping, it is represented as `sequence` in the output.
    /// For example, `(b'\n', b"\\n".to_vec())` means newline → `\n`.
    pub escape_sequences: Vec<(u8, Vec<u8>)>,

    /// Hex-based fallback format for must-escape bytes without an explicit
    /// escape sequence. `None` means such bytes have no representation and
    /// are excluded from the output regex.
    pub fallback: Option<FallbackFormat>,

    /// Bytes that MUST be escaped. Any byte in this set will be replaced by
    /// its escape sequence (explicit or fallback). Bytes not in this set
    /// pass through literally. There are no implicit additions — list
    /// everything that needs escaping, including escape prefix bytes,
    /// quote characters, etc.
    pub must_escape: Vec<u8>,
}

impl StringEscapeOptions {
    /// Sort and deduplicate `escape_sequences` and `must_escape`, and
    /// canonicalize `fallback.valid_byte_ranges` so that semantically
    /// equivalent options hash identically when used as cache keys.
    /// Called automatically by [`RegexBuilder::string_escape`].
    ///
    /// Returns an error if `escape_sequences` contains duplicate entries
    /// for the same byte with different sequences, or if `valid_byte_ranges`
    /// contains an entry where `start > end`.
    pub fn normalize(&mut self) -> Result<()> {
        self.escape_sequences.sort_by_key(|(b, _)| *b);
        // Check for conflicting duplicates before dedup
        for w in self.escape_sequences.windows(2) {
            if w[0].0 == w[1].0 && w[0].1 != w[1].1 {
                anyhow::bail!("conflicting escape_sequences for byte 0x{:02X}", w[0].0);
            }
        }
        self.escape_sequences.dedup_by_key(|(b, _)| *b);
        self.must_escape.sort();
        self.must_escape.dedup();

        // Canonicalize valid_byte_ranges: sort, validate, merge overlapping/adjacent.
        if let Some(ref mut fb) = self.fallback {
            if let Some(ref mut ranges) = fb.valid_byte_ranges {
                ranges.sort_by_key(|(lo, _)| *lo);
                let mut merged: Vec<(u8, u8)> = Vec::with_capacity(ranges.len());
                for &(lo, hi) in ranges.iter() {
                    ensure!(
                        lo <= hi,
                        "invalid valid_byte_ranges entry: start 0x{:02X} > end 0x{:02X}",
                        lo,
                        hi
                    );
                    if let Some((_, last_hi)) = merged.last_mut() {
                        if last_hi.saturating_add(1) >= lo {
                            if hi > *last_hi {
                                *last_hi = hi;
                            }
                            continue;
                        }
                    }
                    merged.push((lo, hi));
                }
                *ranges = merged;
            }
        }

        Ok(())
    }

    /// Build options for JSON string escaping.
    ///
    /// Uses `\u00XX` fallback, and explicit escape sequences for
    /// `\b`, `\f`, `\n`, `\r`, `\t`, `\\`, `\"`.
    /// Control characters 0x00–0x1F, 0x7F, `\`, and `"` are in `must_escape`.
    ///
    /// Note: RFC 8259 only requires escaping 0x00–0x1F, `\`, and `"`.
    /// We additionally escape 0x7F (DEL) as a conservative safety measure.
    pub fn json() -> Self {
        Self {
            escape_sequences: vec![
                (0x08, b"\\b".to_vec()),
                (0x0C, b"\\f".to_vec()),
                (b'\n', b"\\n".to_vec()),
                (b'\r', b"\\r".to_vec()),
                (b'\t', b"\\t".to_vec()),
                (b'\\', b"\\\\".to_vec()),
                (b'"', b"\\\"".to_vec()),
            ],
            fallback: Some(FallbackFormat {
                prefix: b"\\u00".to_vec(),
                suffix: vec![],
                valid_byte_ranges: Some(vec![(0x00, 0x7F)]),
            }),
            must_escape: (0x00..=0x1Fu8).chain([b'\\', b'"', 0x7F]).collect(),
        }
    }

    /// Build options for JSON without `\uXXXX` fallback.
    ///
    /// Same as [`json()`](Self::json) but with no fallback. Bytes that
    /// lack an explicit escape sequence (e.g., 0x01) cannot be represented
    /// and will be excluded from the output regex.
    pub fn json_raw() -> Self {
        let mut opts = Self::json();
        opts.fallback = None;
        opts
    }

    /// Build options for URL percent-encoding (RFC 3986).
    ///
    /// Uses `%HH` for all bytes outside the unreserved set
    /// (`A-Z`, `a-z`, `0-9`, `-`, `.`, `_`, `~`). No string delimiters.
    pub fn url_percent_encoding() -> Self {
        let unreserved: Vec<u8> = (b'A'..=b'Z')
            .chain(b'a'..=b'z')
            .chain(b'0'..=b'9')
            .chain([b'-', b'.', b'_', b'~'])
            .collect();
        let must_escape: Vec<u8> = (0x00..=0xFFu8)
            .filter(|b| !unreserved.contains(b))
            .collect();
        Self {
            escape_sequences: vec![],
            fallback: Some(FallbackFormat {
                prefix: b"%".to_vec(),
                suffix: vec![],
                valid_byte_ranges: None,
            }),
            must_escape,
        }
    }

    /// Build options for XML attribute/text escaping.
    ///
    /// Uses named entities for `&`, `<`, `>`, `"`, `'` and `&#xNN;` hex
    /// fallback for the XML 1.0–legal control characters (TAB, LF, CR).
    ///
    /// Only bytes whose corresponding Unicode scalar values are valid XML
    /// 1.0 characters are included in `must_escape`. XML 1.0 forbids
    /// 0x00–0x08, 0x0B, 0x0C, and 0x0E–0x1F entirely — these code
    /// points cannot appear in an XML document at all (not even as numeric
    /// character references).
    ///
    /// Note: this constructor does not prevent forbidden control bytes from
    /// passing through literally if they appear in the input regex. If the
    /// input regex can match forbidden bytes, the caller should intersect it
    /// with a character-class restriction (e.g., `[\x09\x0A\x0D\x20-\xFF]`)
    /// before calling `string_escape`.
    pub fn xml() -> Self {
        Self {
            escape_sequences: vec![
                (b'&', b"&amp;".to_vec()),
                (b'<', b"&lt;".to_vec()),
                (b'>', b"&gt;".to_vec()),
                (b'"', b"&quot;".to_vec()),
                (b'\'', b"&apos;".to_vec()),
            ],
            fallback: Some(FallbackFormat {
                prefix: b"&#x".to_vec(),
                suffix: b";".to_vec(),
                valid_byte_ranges: Some(vec![(0x09, 0x0A), (0x0D, 0x0D), (0x20, 0xFF)]),
            }),
            // Only valid XML 1.0 characters: TAB, LF, CR, and the five
            // special markup characters that need entity escaping.
            must_escape: [0x09, 0x0A, 0x0D, b'&', b'<', b'>', b'"', b'\'']
                .into_iter()
                .collect(),
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
    /// `u` — `\u00XX` fallback for any must-escape byte ≤ 0x7F;
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

    /// Convert to [`StringEscapeOptions`].
    ///
    /// The `\\` and `"` entries in `allowed_escapes` are ignored; backslash
    /// and double-quote escaping are always enabled in the result.
    ///
    /// Note: this method does not validate `allowed_escapes`. Invalid
    /// characters are silently ignored. Use [`RegexBuilder::json_quote`]
    /// for validation, or validate before calling this method.
    pub fn to_string_escape_options(&self) -> StringEscapeOptions {
        // (escape_char, source_byte) — escape_char doubles as the allowed_escapes key
        let escape_map: &[(u8, u8)] = &[
            (b'b', 0x08),
            (b'f', 0x0C),
            (b'n', b'\n'),
            (b'r', b'\r'),
            (b't', b'\t'),
        ];

        let mut escape_sequences: Vec<(u8, Vec<u8>)> = escape_map
            .iter()
            .filter(|(key, _)| self.is_allowed(*key))
            .map(|(esc, byte)| (*byte, vec![b'\\', *esc]))
            .collect();

        // Backslash and double-quote are always escaped
        escape_sequences.push((b'\\', b"\\\\".to_vec()));
        escape_sequences.push((b'"', b"\\\"".to_vec()));

        let fallback = if self.is_allowed(b'u') {
            Some(FallbackFormat {
                prefix: b"\\u00".to_vec(),
                suffix: vec![],
                valid_byte_ranges: Some(vec![(0x00, 0x7F)]),
            })
        } else {
            None
        };

        StringEscapeOptions {
            escape_sequences,
            fallback,
            must_escape: (0x00..=0x1Fu8).chain([b'\\', b'"', 0x7F]).collect(),
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
        let r = self.string_escape(e, &se_options)?;
        if options.raw_mode {
            Ok(r)
        } else {
            let quote = self.exprset.mk_byte(b'"');
            Ok(self.exprset.mk_concat_vec(&[quote, r, quote]))
        }
    }

    /// Transform a regex for string-literal escaping.
    ///
    /// Given regex R, produces R' such that strings matching R', when
    /// unescaped according to the given grammar, yield strings matching R.
    pub fn string_escape(&mut self, e: ExprRef, options: &StringEscapeOptions) -> Result<ExprRef> {
        let mut options = options.clone();
        options.normalize()?;

        // Validate valid_byte_ranges: every must-escape byte outside the
        // valid ranges needs an explicit escape_sequence.
        if let Some(ref fb) = options.fallback {
            if fb.valid_byte_ranges.is_some() {
                for &b in &options.must_escape {
                    if !fb.covers_byte(b)
                        && !options.escape_sequences.iter().any(|(byte, _)| *byte == b)
                    {
                        anyhow::bail!(
                            "fallback cannot represent byte 0x{:02X} (outside valid_byte_ranges); \
                             add an explicit escape_sequence for it",
                            b,
                        );
                    }
                }
            }
        }

        // Build escape lookup: byte -> index into escape_sequences
        let mut escape_idx = [None::<usize>; 256];
        for (i, (byte, _)) in options.escape_sequences.iter().enumerate() {
            escape_idx[*byte as usize] = Some(i);
        }

        let fallback = options.fallback.clone();

        // Build must_escape bitset
        let mut must_escape_set = [false; 256];
        for &b in &options.must_escape {
            must_escape_set[b as usize] = true;
        }

        // Build a regex node for a single hex digit (case-insensitive for A-F)
        fn mk_hex_digit(exprset: &mut ExprSet, nibble: u8) -> ExprRef {
            if nibble < 10 {
                exprset.mk_byte(b'0' + nibble)
            } else {
                let upper = b'A' + (nibble - 10);
                let lower = b'a' + (nibble - 10);
                let mut bs = byteset_256();
                byteset_set(&mut bs, upper as usize);
                byteset_set(&mut bs, lower as usize);
                exprset.mk_byte_set(&bs)
            }
        }

        // Build a regex for prefix + 2 case-insensitive hex digits + suffix for byte b
        fn mk_fallback_hex(exprset: &mut ExprSet, fb: &FallbackFormat, b: u8) -> ExprRef {
            let prefix_expr = if fb.prefix.is_empty() {
                ExprRef::EMPTY_STRING
            } else {
                exprset.mk_byte_literal(&fb.prefix)
            };
            let high = mk_hex_digit(exprset, b >> 4);
            let low = mk_hex_digit(exprset, b & 0x0F);
            let suffix_expr = if fb.suffix.is_empty() {
                ExprRef::EMPTY_STRING
            } else {
                exprset.mk_byte_literal(&fb.suffix)
            };
            exprset.mk_concat_vec(&[prefix_expr, high, low, suffix_expr])
        }

        fn quote_byteset(
            exprset: &mut ExprSet,
            bs: Vec<u32>,
            escape_sequences: &[(u8, Vec<u8>)],
            escape_idx: &[Option<usize>; 256],
            fallback: Option<&FallbackFormat>,
            must_escape: &[u8],
        ) -> ExprRef {
            let mut alts = vec![];
            let mut bs_passthrough = bs;

            for &b in must_escape {
                if !byteset_contains(&bs_passthrough, b as usize) {
                    continue;
                }
                if let Some(idx) = escape_idx[b as usize] {
                    let seq = &escape_sequences[idx].1;
                    alts.push(exprset.mk_byte_literal(seq));
                }
                // Also add fallback — a byte can have both an explicit
                // escape and a fallback (e.g., JSON \b and \u0008 for 0x08).
                // Only generate fallback if the byte is within valid_byte_ranges.
                if let Some(fb) = fallback {
                    if fb.covers_byte(b) {
                        alts.push(mk_fallback_hex(exprset, fb, b));
                    }
                }
                // If neither exists, byte is excluded from output
                byteset_clear(&mut bs_passthrough, b as usize);
            }

            let passthrough = exprset.mk_byte_set(&bs_passthrough);
            alts.push(passthrough);
            exprset.mk_or(&mut alts)
        }

        let cache = self
            .string_escape_caches
            .entry(options.clone())
            .or_default();
        let r = self.exprset.map(
            e,
            cache,
            false,
            |e| e,
            |exprset, args, e| -> ExprRef {
                match exprset.get(e) {
                    Expr::ByteSet(bs) => {
                        let needs = options
                            .must_escape
                            .iter()
                            .any(|&b| byteset_contains(bs, b as usize));
                        if needs {
                            let bs = bs.to_vec();
                            quote_byteset(
                                exprset,
                                bs,
                                &options.escape_sequences,
                                &escape_idx,
                                fallback.as_ref(),
                                &options.must_escape,
                            )
                        } else {
                            e
                        }
                    }
                    Expr::Byte(b) => {
                        if must_escape_set[b as usize] {
                            quote_byteset(
                                exprset,
                                byteset_from_range(b, b),
                                &options.escape_sequences,
                                &escape_idx,
                                fallback.as_ref(),
                                &options.must_escape,
                            )
                        } else {
                            e
                        }
                    }
                    Expr::ByteConcat(_, bytes, args0) => {
                        if bytes.iter().any(|b| must_escape_set[*b as usize]) {
                            let mut acc = vec![];
                            let mut idx = 0;
                            let bytes = bytes.to_vec();
                            while idx < bytes.len() {
                                let idx0 = idx;
                                while idx < bytes.len() && !must_escape_set[bytes[idx] as usize] {
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
                                        &options.escape_sequences,
                                        &escape_idx,
                                        fallback.as_ref(),
                                        &options.must_escape,
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
                    Expr::EmptyString | Expr::NoMatch | Expr::RemainderIs { .. } => e,
                    x if x.args() == args => e,
                    Expr::And(_, _) => exprset.mk_and(args),
                    Expr::Or(_, _) => exprset.mk_or(args),
                    Expr::Concat(_, _) => exprset.mk_concat(args[0], args[1]),
                    Expr::Not(_, _) => exprset.mk_not(args[0]),
                    Expr::Lookahead(_, _, _) => exprset.mk_lookahead(args[0], 0),
                    Expr::Repeat(_, _, min, max) => exprset.mk_repeat(args[0], min, max),
                }
            },
        );

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
