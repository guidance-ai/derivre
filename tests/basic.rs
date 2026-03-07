use derivre::{JsonQuoteOptions, NextByte, Regex, RegexAst, RegexBuilder, StringEscapeOptions};

fn check_is_match(rx: &mut Regex, s: &str, exp: bool) {
    if rx.is_match(s) == exp {
    } else {
        panic!(
            "error for: {:?}; expected {}",
            s,
            if exp { "match" } else { "no match" }
        );
    }
}

fn match_(rx: &mut Regex, s: &str) {
    check_is_match(rx, s, true);
}

fn match_many(rx: &mut Regex, ss: &[&str]) {
    for s in ss {
        match_(rx, s);
    }
}

fn no_match(rx: &mut Regex, s: &str) {
    check_is_match(rx, s, false);
}

fn no_match_many(rx: &mut Regex, ss: &[&str]) {
    for s in ss {
        no_match(rx, s);
    }
}

fn look(rx: &mut Regex, s: &str, exp: Option<usize>) {
    let res = rx.lookahead_len(s);
    if res == exp {
    } else {
        panic!(
            "lookahead len error for: {:?}; expected {:?}, got {:?}",
            s, exp, res
        )
    }
}

#[test]
fn test_basic() {
    let mut rx = Regex::new("a[bc](de|fg)").unwrap();
    println!("{:?}", rx);
    no_match(&mut rx, "abd");
    match_(&mut rx, "abde");

    no_match(&mut rx, "abdea");
    println!("{:?}", rx);

    let mut rx = Regex::new("a[bc]*(de|fg)*x").unwrap();

    no_match_many(&mut rx, &["", "a", "b", "axb"]);
    match_many(&mut rx, &["ax", "abdex", "abcbcbcbcdex", "adefgdefgx"]);
    println!("{:?}", rx);

    let mut rx = Regex::new("(A|foo)*").unwrap();
    match_many(
        &mut rx,
        &["", "A", "foo", "Afoo", "fooA", "foofoo", "AfooA", "Afoofoo"],
    );

    let mut rx = Regex::new("[abcquv][abdquv]").unwrap();
    match_many(
        &mut rx,
        &["aa", "ab", "ba", "ca", "cd", "ad", "aq", "qa", "qd"],
    );
    no_match_many(&mut rx, &["cc", "dd", "ac", "ac", "bc"]);

    println!("{:?}", rx);

    let mut rx = Regex::new("ab{3,5}c").unwrap();
    match_many(&mut rx, &["abbbc", "abbbbc", "abbbbbc"]);
    no_match_many(
        &mut rx,
        &["", "ab", "abc", "abbc", "abbb", "abbbx", "abbbbbbc"],
    );

    let mut rx = Regex::new("x*A[0-9]{5}").unwrap();
    match_many(&mut rx, &["A12345", "xxxxxA12345", "xA12345"]);
    no_match_many(&mut rx, &["A1234", "xxxxxA123456", "xA123457"]);
}

#[test]
fn test_unicode() {
    let mut rx = Regex::new("źółw").unwrap();
    println!("{:?}", rx);
    no_match(&mut rx, "zolw");
    match_(&mut rx, "źółw");
    no_match(&mut rx, "źół");
    println!("{:?}", rx);

    let mut rx = Regex::new("[źó]łw").unwrap();
    match_(&mut rx, "ółw");
    match_(&mut rx, "źłw");
    no_match(&mut rx, "źzłw");

    let mut rx = Regex::new("x[©ª«]y").unwrap();
    match_many(&mut rx, &["x©y", "xªy", "x«y"]);
    no_match_many(&mut rx, &["x®y", "x¶y", "x°y", "x¥y"]);

    let mut rx = Regex::new("x[ab«\u{07ff}\u{0800}]y").unwrap();
    match_many(&mut rx, &["xay", "xby", "x«y", "x\u{07ff}y", "x\u{0800}y"]);
    no_match_many(&mut rx, &["xcy", "xªy", "x\u{07fe}y", "x\u{0801}y"]);

    let mut rx = Regex::new("x[ab«\u{07ff}-\u{0801}]y").unwrap();
    match_many(
        &mut rx,
        &[
            "xay",
            "xby",
            "x«y",
            "x\u{07ff}y",
            "x\u{0800}y",
            "x\u{0801}y",
        ],
    );
    no_match_many(&mut rx, &["xcy", "xªy", "x\u{07fe}y", "x\u{0802}y"]);

    let mut rx = Regex::new(".").unwrap();
    no_match(&mut rx, "\n");
    match_many(&mut rx, &["a", "1", " ", "\r"]);

    let mut rx = Regex::new("a.*b").unwrap();
    match_many(&mut rx, &["ab", "a123b", "a \r\t123b"]);
    no_match_many(&mut rx, &["a", "a\nb", "a1\n2b"]);
}

#[test]
fn test_lookaround() {
    let mut rx = Regex::new("[ab]*(?P<stop>xx)").unwrap();
    match_(&mut rx, "axx");
    look(&mut rx, "axx", Some(2));
    look(&mut rx, "ax", None);

    let mut rx = Regex::new("[ab]*(?P<stop>x*y)").unwrap();
    look(&mut rx, "axy", Some(2));
    look(&mut rx, "ay", Some(1));
    look(&mut rx, "axxy", Some(3));
    look(&mut rx, "aaaxxy", Some(3));
    look(&mut rx, "abaxxy", Some(3));
    no_match_many(&mut rx, &["ax", "bx", "aaayy", "axb", "axyxx"]);

    let mut rx = Regex::new("[abx]*(?P<stop>[xq]*y)").unwrap();
    look(&mut rx, "axxxxxxxy", Some(1));
    look(&mut rx, "axxxxxxxqy", Some(2));
    look(&mut rx, "axxxxxxxqqqy", Some(4));

    let mut rx = Regex::new("(f|foob)(?P<stop>o*y)").unwrap();
    look(&mut rx, "fooby", Some(1));
    look(&mut rx, "fooy", Some(3));
    look(&mut rx, "fy", Some(1));
}

#[test]
fn utf8_dfa() {
    let parser = regex_syntax::ParserBuilder::new()
        .unicode(false)
        .utf8(false)
        .ignore_whitespace(true)
        .build();

    let utf8_rx = r#"
   ( [\x00-\x7F]                        # ASCII
   | [\xC2-\xDF][\x80-\xBF]             # non-overlong 2-byte
   |  \xE0[\xA0-\xBF][\x80-\xBF]        # excluding overlongs
   | [\xE1-\xEC\xEE\xEF][\x80-\xBF]{2}  # straight 3-byte
   |  \xED[\x80-\x9F][\x80-\xBF]        # excluding surrogates
   |  \xF0[\x90-\xBF][\x80-\xBF]{2}     # planes 1-3
   | [\xF1-\xF3][\x80-\xBF]{3}          # planes 4-15
   |  \xF4[\x80-\x8F][\x80-\xBF]{2}     # plane 16
   )*
   "#;

    let mut rx = Regex::new_with_parser(parser, utf8_rx).unwrap();
    println!("UTF8 {:?}", rx);
    //match_many(&mut rx, &["a", "ą", "ę", "ó", "≈ø¬", "\u{1f600}"]);
    println!("UTF8 {:?}", rx);
    let compiled = rx.dfa();
    println!("UTF8 {:?}", rx);
    println!("mapping ({}) {:?}", rx.alpha().len(), &compiled[0..256]);
    println!("states {:?}", &compiled[256..]);
    println!("initial {:?}", rx.initial_state());
}

#[test]
fn utf8_restrictions() {
    let mut rx = Regex::new("(.|\n)*").unwrap();
    println!("{:?}", rx);
    match_many(&mut rx, &["", "a", "\n", "\n\n", "\x00", "\x7f"]);
    let s0 = rx.initial_state();
    assert!(rx.transition(s0, 0x80).is_dead());
    assert!(rx.transition(s0, 0xC0).is_dead());
    assert!(rx.transition(s0, 0xC1).is_dead());
    // more overlong:
    assert!(rx.transition_bytes(s0, &[0xE0, 0x80]).is_dead());
    assert!(rx.transition_bytes(s0, &[0xE0, 0x9F]).is_dead());
    assert!(rx.transition_bytes(s0, &[0xF0, 0x80]).is_dead());
    assert!(rx.transition_bytes(s0, &[0xF0, 0x8F]).is_dead());
    // surrogates:
    assert!(rx.transition_bytes(s0, &[0xED, 0xA0]).is_dead());
    assert!(rx.transition_bytes(s0, &[0xED, 0xAF]).is_dead());
    assert!(rx.transition_bytes(s0, &[0xED, 0xBF]).is_dead());
}

#[test]
fn trie() {
    let mut rx = Regex::new("(foo|far|bar|baz)").unwrap();
    match_many(&mut rx, &["foo", "far", "bar", "baz"]);
    no_match_many(&mut rx, &["fo", "fa", "b", "ba", "baa", "f", "faz"]);

    let mut rx = Regex::new("(foobarbazqux123|foobarbazqux124)").unwrap();
    match_many(&mut rx, &["foobarbazqux123", "foobarbazqux124"]);
    no_match_many(
        &mut rx,
        &["foobarbazqux12", "foobarbazqux125", "foobarbazqux12x"],
    );

    let mut rx = Regex::new("(1a|12a|123a|1234a|12345a|123456a)").unwrap();
    match_many(
        &mut rx,
        &["1a", "12a", "123a", "1234a", "12345a", "123456a"],
    );
    no_match_many(
        &mut rx,
        &["1234567a", "123456", "12345", "1234", "123", "12", "1"],
    );
}

#[test]
fn unicode_case() {
    let mut rx = Regex::new("(?i)Żółw").unwrap();
    match_many(&mut rx, &["Żółw", "żółw", "ŻÓŁW", "żóŁw"]);
    no_match_many(&mut rx, &["zółw"]);

    let mut rx = Regex::new("Żółw").unwrap();
    match_(&mut rx, "Żółw");
    no_match_many(&mut rx, &["żółw", "ŻÓŁW", "żóŁw"]);
}

fn validate_next_byte(rx: &mut Regex, data: Vec<(NextByte, u8)>) {
    let mut s = rx.initial_state();
    for (exp, b) in data {
        println!("next_byte {:?} {:?}", exp, b as char);
        let nb = rx.next_byte(s);
        if nb != exp {
            panic!("expected {:?}, got {:?}", exp, nb);
        }
        if nb == NextByte::ForcedEOI {
            assert!(rx.is_accepting(s));
        } else if nb == NextByte::Dead {
            assert!(s.is_dead());
        }
        s = rx.transition(s, b);
        if nb == NextByte::ForcedEOI {
            assert!(s.is_dead());
            assert!(rx.next_byte(s) == NextByte::Dead);
        }
    }
}

#[test]
fn next_byte() {
    let mut rx = Regex::new("a[bc]*dx").unwrap();
    validate_next_byte(
        &mut rx,
        vec![
            (NextByte::ForcedByte(b'a'), b'a'),
            (NextByte::SomeBytes2([b'b', b'c']), b'b'),
            (NextByte::SomeBytes2([b'b', b'c']), b'd'),
            (NextByte::ForcedByte(b'x'), b'x'),
            (NextByte::ForcedEOI, b'x'),
        ],
    );

    rx = Regex::new("abdx|aBDy").unwrap();
    validate_next_byte(
        &mut rx,
        vec![
            (NextByte::ForcedByte(b'a'), b'a'),
            (NextByte::SomeBytes2([b'B', b'b']), b'B'),
            (NextByte::ForcedByte(b'D'), b'D'),
        ],
    );

    rx = Regex::new("foo|bar").unwrap();
    validate_next_byte(
        &mut rx,
        vec![
            (NextByte::SomeBytes2([b'b', b'f']), b'f'),
            (NextByte::ForcedByte(b'o'), b'o'),
            (NextByte::ForcedByte(b'o'), b'o'),
            (NextByte::ForcedEOI, b'X'),
        ],
    );
}

fn check_one_quote(rx: &str, options: &JsonQuoteOptions, allow_nl: bool) -> Regex {
    let valid_any_string = &[
        "a", "A", "!", " ", "\\\"", "\\b", "\\f", "\\r", "\\t", "\\\\",
    ];
    let valid_any_string_unicode = &["\\u001A", "\\u001a", "\\u0000", "\\u0001", "\\u0008"];
    let invalid_any_string = &["\n", "\t", "\"", "\\", "\\'", "aa"];
    let string_allowing_nl = if options.is_allowed(b'u') {
        &["\\n", "\\u000A", "\\u000a"]
    } else {
        &["\\n", "\\n", "\\n"]
    };

    let mut b = RegexBuilder::new();

    let e = b.mk_regex(rx).unwrap();
    let e = b.json_quote(e, options).unwrap();
    println!("*** {:?} {}", rx, b.exprset().expr_to_string(e));

    let mut rx = b.to_regex(e);
    match_many(&mut rx, valid_any_string);
    no_match_many(&mut rx, invalid_any_string);
    if options.is_allowed(b'u') {
        match_many(&mut rx, valid_any_string_unicode);
    } else {
        no_match_many(&mut rx, valid_any_string_unicode);
    }
    if allow_nl {
        match_many(&mut rx, string_allowing_nl);
    } else {
        no_match_many(&mut rx, string_allowing_nl);
    }

    rx
}

fn check_json_quote(
    options: &JsonQuoteOptions,
    rx: &str,
    should_match: &[&str],
    should_not_match: &[&str],
) {
    let mut b = RegexBuilder::new();
    let e = b
        .mk(&RegexAst::JsonQuote(
            Box::new(RegexAst::Regex(rx.to_string())),
            options.clone(),
        ))
        .unwrap();
    let mut rx = b.to_regex(e);
    match_many(&mut rx, should_match);
    no_match_many(&mut rx, should_not_match);
}

#[test]
fn test_json_quote() {
    let mut b = RegexBuilder::new();

    for options in [
        JsonQuoteOptions::no_unicode_raw(),
        JsonQuoteOptions::with_unicode_raw(),
    ] {
        let e = b.mk_regex(r#"[abc"]"#).unwrap();
        let e = b.json_quote(e, &options).unwrap();

        let mut rx = b.to_regex(e);
        match_many(&mut rx, &["a", "b", "c", "\\\""]);
        no_match_many(&mut rx, &["A", "\"", "\\"]);

        check_json_quote(&options, r#"""#, &["\\\""], &["\"", "a", ""]);
        check_json_quote(&options, r#"\\"#, &["\\\\"], &["\\", "a", ""]);
        if options.is_allowed(b'u') {
            check_json_quote(&options, r#"\x7F"#, &["\\u007F"], &["\x7F", "a", ""]);
        }

        check_one_quote(r#"."#, &options, false);
        check_one_quote(r#".|\n"#, &options, true);

        let mut rx = check_one_quote(r#"[^\u0017]"#, &options, true);
        no_match_many(&mut rx, &["\\u0017"]);
    }
}

#[test]
fn test_json_qbig() {
    let mut b = RegexBuilder::new();
    let options = JsonQuoteOptions::with_unicode_raw();
    let rx = "\\w+[\\\\](\\w+\\.)*\\w+\\.dll";
    // let rx = "a[\\\\]b";
    let t0 = std::time::Instant::now();
    let e0 = b.mk_regex(rx).unwrap();
    let el0 = t0.elapsed();
    let c0 = b.exprset().cost();
    let e = b.json_quote(e0, &options).unwrap();
    let c1 = b.exprset().cost();
    println!("*** {:?} {}", rx, b.exprset().expr_to_string(e0).len());
    println!("  >>> {}", b.exprset().expr_to_string(e).len());
    println!("  cost {} {} {:?}", c0, c1, el0);
}

#[test]
fn test_json_uxxxx() {
    let mut b = RegexBuilder::new();
    let options = JsonQuoteOptions::with_unicode_raw();
    let e0 = b.mk_regex(".").unwrap();
    let e = b.json_quote(e0, &options).unwrap();
    let mut rx = b.to_regex(e);
    for x in 0..=0xffff {
        for s in &[format!("\\u{:04X}", x), format!("\\u{:04x}", x)] {
            // Any must-escape byte with a \uXXXX fallback should match.
            // must_escape for JSON: 0x00-0x1F, 0x22 ("), 0x5C (\), 0x7F.
            // \n (0x0A) is excluded because `.` doesn't match newline.
            let is_must_escape =
                (0x0000..=0x001f).contains(&x) || x == 0x0022 || x == 0x005c || x == 0x007f;
            if is_must_escape && x != 0x000a {
                match_(&mut rx, s);
            } else {
                no_match(&mut rx, s);
            }
        }
    }
}

#[test]
fn test_json_and() {
    let mut b = RegexBuilder::new();
    let options = JsonQuoteOptions::with_unicode_raw();

    let e0 = b.mk_regex_and(&["[a-z]+", "(foo|bar|Baz)"]).unwrap();
    let e = b.json_quote(e0, &options).unwrap();
    let mut rx = b.to_regex(e);
    match_many(&mut rx, &["foo", "bar"]);
    no_match_many(&mut rx, &["xoo", "Baz"]);

    let e0 = b.mk_regex_and(&["[a-z\n]+", "(foo\n|bar|Baz)"]).unwrap();
    let e = b.json_quote(e0, &options).unwrap();
    let mut rx = b.to_regex(e);
    match_many(&mut rx, &["foo\\n", "bar"]);
    no_match_many(&mut rx, &["foo\n", "xoo", "Baz"]);

    // contained_in(a,b) == a & ~b
    let e0 = b.mk_contained_in("[a-z\n]+", "(foo\n|bar|Baz)").unwrap();
    let e = b.json_quote(e0, &options).unwrap();
    let mut rx = b.to_regex(e);
    no_match_many(
        &mut rx,
        &["foo\\n", "q\n", "foo\\u000a", "bar", "Baz", "QUX"],
    );
    match_many(&mut rx, &["foo", "fooo\\n", "baar"]);
}

#[test]
fn test_string_escape_hex_fallback() {
    // Test \xHH fallback format
    let mut b = RegexBuilder::new();
    let opts = StringEscapeOptions {
        escape_sequences: vec![
            (b'\n', b"\\n".to_vec()),
            (b'\r', b"\\r".to_vec()),
            (b'\t', b"\\t".to_vec()),
        ],
        fallback_prefix: Some(b"\\x".to_vec()),
        max_fallback_byte: None,
        must_escape: (0x00..=0x1Fu8).chain(std::iter::once(0x7Fu8)).collect(),
    };

    // Control char 0x01 should be representable as \x01
    let e = b.mk_regex(r#"\x01"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["\\x01"]);
    no_match_many(&mut rx, &["\x01", "\\u0001"]);

    // \n has both explicit (\n) and fallback (\x0A)
    let e = b.mk_regex(r#"\n"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["\\n", "\\x0A", "\\x0a"]);
    // Confirm hex is byte-specific: \x0D is \r's encoding, not \n's
    no_match_many(&mut rx, &["\n", "\\x0D"]);

    // \x7F should work
    let e = b.mk_regex(r#"\x7F"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["\\x7F", "\\x7f"]);
    no_match_many(&mut rx, &["\x7F", "\\u007F"]);

    // Regular chars pass through
    let e = b.mk_regex(r#"[abc]"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["a", "b", "c"]);
    no_match_many(&mut rx, &["A", "d"]);
}

#[test]
fn test_string_escape_single_quote() {
    // Test Python-style single-quoted strings with \xHH fallback
    let mut b = RegexBuilder::new();
    let opts = StringEscapeOptions {
        escape_sequences: vec![
            (0x07, b"\\a".to_vec()),
            (0x08, b"\\b".to_vec()),
            (0x0C, b"\\f".to_vec()),
            (b'\n', b"\\n".to_vec()),
            (b'\r', b"\\r".to_vec()),
            (b'\t', b"\\t".to_vec()),
            (0x0B, b"\\v".to_vec()),
            (b'\\', b"\\\\".to_vec()),
            (b'\'', b"\\'".to_vec()),
        ],
        fallback_prefix: Some(b"\\x".to_vec()),
        max_fallback_byte: None,
        must_escape: (0x00..=0x1Fu8).chain([b'\\', b'\'', 0x7F]).collect(),
    };

    // A simple regex — no wrapping
    let e = b.mk_regex(r#"[a']"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    // ' has both explicit (\') and fallback (\x27) — both valid
    match_many(&mut rx, &["a", "\\'", "\\x27"]);
    no_match_many(&mut rx, &["'", "\\\\'"]);

    // Bell character (0x07) should escape as \a or \x07 (both valid)
    let e = b.mk_regex(r#"\x07"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let s = b.exprset().expr_to_string(r);
    println!("bell escape: {}", s);
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["\\a", "\\x07"]);
}

#[test]
fn test_string_escape_doubling() {
    // Test YAML single-quoted style: ' is escaped as ''
    let mut b = RegexBuilder::new();
    let opts = StringEscapeOptions {
        escape_sequences: vec![(b'\'', b"''".to_vec())],
        fallback_prefix: None,
        max_fallback_byte: None,
        must_escape: vec![b'\''],
    };

    let e = b.mk_regex(r#"[a']"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    // 'a' passes through, single quote becomes ''
    match_many(&mut rx, &["a", "''"]);
    // No fallback configured, so \x27 is not a valid representation
    no_match_many(&mut rx, &["'", "\\'", "\\x27"]);
    // Backslash is literal in YAML single-quoted mode — not an escape prefix
    let e = b.mk_regex(r#"[a\\]"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["a", "\\"]); // literal backslash, not escaped
    no_match_many(&mut rx, &["\\\\"]); // double-backslash should NOT match

    // Newlines are literal in YAML single-quoted strings (per YAML 1.2)
    let e = b.mk_regex(r#"\n"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["\n"]); // literal newline passes through
    no_match_many(&mut rx, &["\\n"]); // \n escape sequence is NOT valid here
}

#[test]
fn test_string_escape_cache_correctness() {
    // Different options for the same regex must produce different results
    let mut b = RegexBuilder::new();

    let opts_json = StringEscapeOptions::json();
    let opts_hex = StringEscapeOptions {
        escape_sequences: vec![
            (0x08, b"\\b".to_vec()),
            (0x0C, b"\\f".to_vec()),
            (b'\n', b"\\n".to_vec()),
            (b'\r', b"\\r".to_vec()),
            (b'\t', b"\\t".to_vec()),
            (b'\\', b"\\\\".to_vec()),
            (b'"', b"\\\"".to_vec()),
        ],
        fallback_prefix: Some(b"\\x".to_vec()),
        max_fallback_byte: None,
        must_escape: (0x00..=0x1Fu8).chain([b'\\', b'"', 0x7F]).collect(),
    };

    let e = b.mk_regex(r#"\x01"#).unwrap();
    let r_json = b.string_escape(e, &opts_json).unwrap();
    let r_hex = b.string_escape(e, &opts_hex).unwrap();

    let s_json = b.exprset().expr_to_string(r_json);
    let s_hex = b.exprset().expr_to_string(r_hex);

    // They should be different — one uses \u0001, the other \x01
    assert_ne!(
        s_json, s_hex,
        "different options should produce different results"
    );

    // Verify each matches its expected format and rejects the other
    let mut rx_json = b.to_regex(r_json);
    match_(&mut rx_json, "\\u0001");
    no_match(&mut rx_json, "\\x01");

    let mut rx_hex = b.to_regex(r_hex);
    match_(&mut rx_hex, "\\x01");
    no_match(&mut rx_hex, "\\u0001");
}

#[test]
fn test_string_escape_must_escape_high_byte() {
    // must_escape byte 0x7F (DEL) with \xHH fallback
    let mut b = RegexBuilder::new();
    let opts = StringEscapeOptions {
        escape_sequences: vec![],
        fallback_prefix: Some(b"\\x".to_vec()),
        max_fallback_byte: None,
        must_escape: vec![0x7F],
    };
    let e = b.mk_regex(r#"\x7F"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["\\x7F", "\\x7f"]);
    no_match_many(&mut rx, &["\x7F"]);
}

#[test]
fn test_string_escape_unicode_rejects_high_byte() {
    // Fallback with max_fallback_byte must reject must_escape bytes above the limit
    let mut b = RegexBuilder::new();
    let opts = StringEscapeOptions {
        escape_sequences: vec![],
        fallback_prefix: Some(b"\\u00".to_vec()),
        max_fallback_byte: Some(0x7F),
        must_escape: vec![0x80],
    };
    let e = b.mk_regex(r#"\x80"#).unwrap();
    assert!(b.string_escape(e, &opts).is_err());
}

#[test]
fn test_string_escape_control_only_regex() {
    // Regex matching only control characters
    let mut b = RegexBuilder::new();
    let opts = StringEscapeOptions::json();
    let e = b.mk_regex(r#"[\x00-\x1F]"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    // Should match escaped control chars (no wrapping)
    // \u0009 verifies both explicit (\t) and fallback (\u0009) are valid for the same byte
    match_many(&mut rx, &["\\n", "\\t", "\\u0001", "\\u0009", "\\u000A"]);
    // Should not match printable chars or unescaped controls
    no_match_many(&mut rx, &["a", "\x01"]);
}

#[test]
fn test_string_escape_percent_encoding() {
    let mut b = RegexBuilder::new();
    let opts = StringEscapeOptions::url_percent_encoding();

    // Space (0x20) should be %20 (RFC 3986, not + as in HTML form encoding)
    let e = b.mk_regex(r#" "#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["%20"]);
    no_match_many(&mut rx, &[" ", "+", "%2G"]);

    // Unreserved chars pass through
    let e = b.mk_regex(r#"[a-z]"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["a", "z"]);
    no_match_many(&mut rx, &["%61"]);

    // Percent itself is escaped as %25
    let e = b.mk_regex(r#"%"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["%25"]);
    no_match_many(&mut rx, &["%", "%%"]);

    // Forward slash (reserved) should be escaped
    let e = b.mk_regex(r#"/"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["%2F", "%2f"]);
    no_match_many(&mut rx, &["/"]);

    // Multi-byte UTF-8: é (U+00E9) = bytes 0xC3 0xA9 → %C3%A9
    let e = b.mk_regex("é").unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["%C3%A9", "%c3%a9", "%C3%a9"]);
    no_match_many(&mut rx, &["é", "%C3", "%A9"]);
}

#[test]
fn test_string_escape_no_quote_special() {
    // Quote char escaped via fallback, not specially
    let mut b = RegexBuilder::new();
    let opts = StringEscapeOptions {
        escape_sequences: vec![(b'\\', b"\\\\".to_vec())],
        fallback_prefix: Some(b"\\x".to_vec()),
        max_fallback_byte: None,
        must_escape: vec![b'"', b'\\'],
    };
    let e = b.mk_regex(r#"""#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    // " has no escape_sequence, falls through to \xHH fallback
    match_many(&mut rx, &["\\x22"]);
    no_match_many(&mut rx, &["\\\"", "\""]);

    // \\ has an explicit escape, but \x5C fallback also valid
    let e = b.mk_regex(r#"\\"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["\\\\", "\\x5C", "\\x5c"]);
    no_match_many(&mut rx, &["\\"]);
}

#[test]
fn test_string_escape_json_solidus() {
    // RFC 8259 allows \/ as an escape for /, but / does not require escaping.
    // Our JSON config treats / as literal (not in must_escape), so \/ is not
    // accepted. This is correct for canonical output generation.
    let mut b = RegexBuilder::new();
    let opts = StringEscapeOptions::json();
    let e = b.mk_regex(r#"/"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["/"]);
    no_match_many(&mut rx, &["\\/", "%2F"]);
}

#[test]
fn test_string_escape_multi_char() {
    // Multi-character regex: tests the ByteConcat path in string_escape
    let mut b = RegexBuilder::new();
    let opts = StringEscapeOptions::json();

    // Literal with embedded must-escape bytes
    let e = b.mk_regex(r#"a\nb"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["a\\nb", "a\\u000Ab"]);
    no_match_many(&mut rx, &["a\nb", "anb"]);

    // Literal with no must-escape bytes passes through unchanged
    let e = b.mk_regex("hello").unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_(&mut rx, "hello");
    no_match_many(&mut rx, &["HELLO", "hell"]);
}

#[test]
fn test_string_escape_nullable() {
    // Nullable regex: empty string should pass through
    let mut b = RegexBuilder::new();
    let opts = StringEscapeOptions::json();

    let e = b.mk_regex(r#"a?"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_many(&mut rx, &["", "a"]);
    no_match_many(&mut rx, &["b", "aa"]);
}

#[test]
fn test_string_escape_unrepresentable() {
    // When a must-escape byte has no explicit escape and no fallback,
    // it cannot be represented and is excluded from the output regex.
    let mut b = RegexBuilder::new();
    let opts = StringEscapeOptions {
        escape_sequences: vec![(b'\n', b"\\n".to_vec())],
        fallback_prefix: None,
        max_fallback_byte: None,
        must_escape: (0x00..=0x1Fu8).collect(),
    };

    // \x01 is must_escape but has no explicit escape and no fallback → excluded
    let e = b.mk_regex(r#"\x01"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    // Nothing should match — the byte is unrepresentable
    no_match_many(&mut rx, &["\x01", "\\x01", "\\u0001"]);

    // \n IS representable (has explicit escape), so it works
    let e = b.mk_regex(r#"\n"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_(&mut rx, "\\n");
    no_match_many(&mut rx, &["\n", "\\x0A"]);

    // Mixed: [\x01\n] — only \n is representable
    let e = b.mk_regex(r#"[\x01\n]"#).unwrap();
    let r = b.string_escape(e, &opts).unwrap();
    let mut rx = b.to_regex(r);
    match_(&mut rx, "\\n");
    no_match_many(&mut rx, &["\x01", "\n", "\\x01"]);
}

fn mk_search_regex(rx: &str) -> Regex {
    let mut b = RegexBuilder::new();
    let e0 = b.mk_regex_for_serach(rx).unwrap();
    b.to_regex(e0)
}

fn assert_search(search_rx: &str, match_rx: &str) {
    let mut b = RegexBuilder::new();
    let e0 = b.mk_regex_for_serach(search_rx).unwrap();
    let e1 = b.mk_regex(&format!("(?s:{})", match_rx)).unwrap();
    if e0 != e1 {
        panic!(
            "search regex {:?} != match regex {:?} (based on {:?} != {:?})",
            b.exprset().expr_to_string(e0),
            b.exprset().expr_to_string(e1),
            search_rx,
            match_rx
        );
    }
}

#[test]
fn test_search() {
    let mut rx = mk_search_regex("foo");
    match_many(&mut rx, &["foo", "fooa", "afoo", "afooa"]);
    no_match_many(&mut rx, &["fo", "foO", ""]);

    let mut rx = mk_search_regex("^foo");
    match_many(&mut rx, &["foo", "fooa"]);
    no_match_many(&mut rx, &["fo", "foO", "afoo", "afooa"]);

    assert_search("foo", ".*foo.*");
    assert_search("^foo", "foo.*");
    assert_search("foo$", ".*foo");
    assert_search("[ab]$", ".*[ab]");
    assert_search("^$|foo", "|.*foo.*");
    assert_search("^$|foo$", "|.*foo");
    assert_search("^$|^foo$", "|foo");
    assert_search("^$|^foo", "|foo.*");
    assert_search("(^$)|(foo$)", "|.*foo");
    assert_search("(?s:a.*b)", ".*a.*b.*");
    assert_search("(abc)", ".*(abc).*");
    assert_search("baz|qux", ".*(baz|qux).*");
    assert_search("^hello", "hello.*");
    assert_search("world$", ".*world");
    assert_search("^hello$", "hello");
    assert_search("\\d+", ".*\\d+.*");
    assert_search("(?:abc)+", ".*(?:abc)+.*");
    assert_search("^[a-z]*$", "[a-z]*");
    assert_search("^(a|b)$", "a|b");
    assert_search("^(foo)*$", "(foo)*");
    assert_search("colou?r", ".*colou?r.*");
    assert_search("[0-9]{2,4}", ".*[0-9]{2,4}.*");
    assert_search("^[^a-z]+$", "[^a-z]+");
    assert_search("file\\.txt", ".*file\\.txt.*");
}
