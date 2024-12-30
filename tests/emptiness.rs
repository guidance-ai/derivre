use derivre::{Regex, RegexAst, RegexBuilder};

const REL_FUEL: u64 = 1_000_000;

fn mk_and(a: &str, b: &str) -> Regex {
    let mut bld = RegexBuilder::new();
    let a = RegexAst::ExprRef(bld.mk_regex(a).unwrap());
    let b = RegexAst::ExprRef(bld.mk_regex(b).unwrap());
    let r = bld.mk(&RegexAst::And(vec![a, b])).unwrap();
    bld.to_regex(r)
}

fn is_contained_in(small: &str, big: &str) -> bool {
    RegexBuilder::new()
        // .unicode(false)
        // .utf8(false)
        .is_contained_in(small, big, REL_FUEL)
        .unwrap()
}

fn is_contained_in_prefixes(small: &str, big: &str) -> bool {
    is_contained_in_prefixes_except(small, big, "")
}

fn is_contained_in_prefixes_except(small: &str, big: &str, except: &str) -> bool {
    let mut bld = RegexBuilder::new();
    // bld.unicode(false).utf8(false);
    let mut big = RegexAst::Regex(big.to_string());
    if !except.is_empty() {
        let excl = RegexAst::Not(Box::new(RegexAst::Regex(except.to_string())));
        big = RegexAst::And(vec![big, excl]);
    }

    let big = bld.mk(&big).unwrap();
    let small = bld.mk_regex(small).unwrap();
    Regex::is_contained_in_prefixes(bld.exprset(), small, big, REL_FUEL).unwrap()
}

fn check_empty(a: &str, b: &str) {
    let mut r = mk_and(a, b);
    assert!(r.always_empty());

    let mut r = Regex::new(a).unwrap();
    assert!(!r.always_empty());

    let mut r = Regex::new(b).unwrap();
    assert!(!r.always_empty());
}

fn check_non_empty(a: &str, b: &str) {
    let mut r = mk_and(a, b);
    assert!(!r.always_empty());
}

fn check_contains(small: &str, big: &str) {
    let t0 = std::time::Instant::now();
    print!("{} in {} ", small, big);
    if !is_contained_in(small, big) {
        panic!("{} is not contained in {}", small, big);
    }

    if is_contained_in(big, small) {
        panic!("{} is contained in {}", big, small);
    }
    println!("time: {:?}", t0.elapsed());
}

fn check_not_contains(small: &str, big: &str) {
    if is_contained_in(small, big) {
        panic!("{} is contained in {}", small, big);
    }
    if is_contained_in(big, small) {
        panic!("{} is contained in {}", big, small);
    }
}

fn check_contains_prefixes(small: &str, big: &str) {
    if !is_contained_in_prefixes(small, big) {
        panic!("{} is not contained in {}", small, big);
    }

    if is_contained_in_prefixes(big, small) {
        panic!("{} is contained in {}", big, small);
    }
}

fn check_contains_prefixes_except(small: &str, big: &str, except: &str) {
    if !is_contained_in_prefixes_except(small, big, except) {
        panic!("{} is not contained in {} - {}", small, big, except);
    }
}

fn check_not_contains_prefixes_except(small: &str, big: &str, except: &str) {
    if is_contained_in_prefixes_except(small, big, except) {
        panic!("{} is contained in {} - {}", small, big, except);
    }
}

fn check_not_contains_prefixes(small: &str, big: &str) {
    if is_contained_in_prefixes(small, big) {
        panic!("{} is contained in {}", small, big);
    }
    if is_contained_in_prefixes(big, small) {
        panic!("{} is contained in {}", big, small);
    }
}

#[test]
fn test_relevance() {
    check_non_empty(r"[a-z]*X", r"[a-b]*X");
    check_empty(r"[a-z]*X", r"[a-z]*Y");
    check_empty(r"[a-z]+X", r"[a-z]+Y");
    check_non_empty(r"[a-z]+X", r"[a-z]+[XY]");
    check_non_empty(r"[a-z]+X", r"[a-z]+q*X");

    // doesn't seem exponential
    check_empty(r".*A.{135}", r".*B.{135}");
    check_non_empty(r".*A.{135}", r".*B.{134}");
    check_empty(r".*A.{135}", r"[B-Z]+");
    check_non_empty(r".*A.{135}", r"[A-Z]+");
}

#[test]
fn test_contains() {
    check_contains(r"[a-b]", r"[a-z]");
    check_contains(r"[a-b]*", r"[a-z]*");
    check_contains(r"[a-b]+", r"[a-z]+");
    check_contains(r"aX", r"[a-z]X");
    check_contains(r"aX", r"[a-z]*X");
    check_contains(r"[a-b]*X", r"[a-z]*X");

    let json_str = r#"(\\([\"\\\/bfnrt]|u[a-fA-F0-9]{4})|[^\"\\\x00-\x1F\x7F])*"#;

    check_contains(r"[a-z]+", &json_str);
    check_contains(r"[a-z\u{0080}-\u{FFFF}]+", &json_str);
    check_contains(r"[a-zA-Z\u{0080}-\u{10FFFF}]+", &json_str);
    check_contains(r" [a-zA-Z\u{0080}-\u{10FFFF}]*", &json_str);

    check_not_contains(r"[\\a-z\u{0080}-\u{FFFF}]+", &json_str);
    check_not_contains(r#"["a-z\u{0080}-\u{FFFF}]+"#, &json_str);
    check_not_contains(r#"[\na-z\u{0080}-\u{FFFF}]+"#, &json_str);
    check_not_contains(r"[\\a-z]+", &json_str);

    check_contains(r"[Bb]*B[Bb]{4}", r"[BQb]*B[Bb]{4}");
    check_contains(r"[B]*B[Bb]", r"[BC]*B[Bb]");

    check_contains(r"[aA]{0,1}A", r"[abA]{0,1}A");
    check_contains(r".*A.{15}", r".+");
    // exponential
    check_contains(r".*A.{8}", r".*[AB].{8}");

    // expecting this to be exponential
    // the actual cost is around 1M
    let r = RegexBuilder::new().is_contained_in(r".*A.{8}", r".*[AB].{8}", 100_000);
    assert!(r.is_err());

    let r = RegexBuilder::new().is_contained_in(r".*A.{8}", r".*[AB].{8}", 5_000_000);
    assert!(r.unwrap() == true);
}

#[test]
fn test_prefixes_normal() {
    // note the final "
    let json_str = r#"(\\([\"\\\/bfnrt]|u[a-fA-F0-9]{4})|[^\"\\\x00-\x1F\x7F])*""#;

    //check_contains_prefixes(r"a", r"aB");
    check_contains_prefixes(r"[a-z]+", r"[a-z]+BBB");

    check_contains_prefixes(r"[a-z]+", &json_str);
    check_contains_prefixes(r"[a-z\u{0080}-\u{FFFF}]+", &json_str);
    check_contains_prefixes(r"[a-zA-Z\u{0080}-\u{10FFFF}]+", &json_str);
    check_contains_prefixes(r" [a-zA-Z\u{0080}-\u{10FFFF}]*", &json_str);
    check_not_contains_prefixes(r"[\\a-z\u{0080}-\u{FFFF}]+", &json_str);
    check_not_contains_prefixes(r"[\\a-z\u{0080}-\u{FFFF}]+", &json_str);
    check_not_contains_prefixes(r#"["a-z\u{0080}-\u{FFFF}]+"#, &json_str);
    check_not_contains_prefixes(r#"[\na-z\u{0080}-\u{FFFF}]+"#, &json_str);
    check_not_contains_prefixes(r"[\\a-z]+", &json_str);
    // check_contains_prefixes(r"[Bb]*B[Bb]{4}", r"[BQb]*B[Bb]{4}X");
    // check_contains_prefixes(r"[B]*B[Bb]", r"[BC]*B[Bb]X");
    // check_contains_prefixes(r"[Bb]*B[Bb]{4}", r"[BQb]*B[Bb]{4}");
    // check_contains_prefixes(r"[B]*B[Bb]", r"[BC]*B[Bb]");

    check_contains_prefixes(r"[a-z]+", &json_str);

    check_contains_prefixes_except(r"[abc]+", "[abcd]+Q", r#"aQ"#);
    check_contains_prefixes_except(r"[a-z]+", &json_str, r#"(foo|bar)""#);
}

#[test]
fn test_prefixes_except() {
    check_not_contains_prefixes_except(r"f", "fQ", r#"fQ"#);

    check_contains_prefixes_except(r"[a-z]+", "[a-zB]+Q", r#"(foo|bar)Q"#);
    check_contains_prefixes_except(r"[a-z]{0,5}", "[a-zB]{0,6}Q", r#"(foox|bar)Q"#);
    check_contains_prefixes_except(r"[a-z]{0,5}", "[a-zB]{0,5}Q", r#"(foox|bar)Q"#);
    check_not_contains_prefixes_except(r"[a-z]{0,5}", "[a-zB]{0,5}Q", r#"(fooxx|bar)Q"#);
    check_contains_prefixes_except(r"[a-z]+", "[a-zB]+", r#"(foo|bar)"#);

    check_contains_prefixes_except(r"[a-z]{0,5}", "[a-zB]{0,5}Q", r#"(fooQ|barQ)"#);
    // we're not smart enough to factor Q out of the expression, so this one may be fixed in future
    check_not_contains_prefixes_except(r"[a-z]{0,4}", "[a-zB]{0,4}Q", r#"(fooQ|barQ)"#);
    // but this one should fail
    check_not_contains_prefixes_except(r"[a-z]{0,4}", "[a-zB]{0,4}Q", r#"(foozQ|barQ)"#);

    check_contains_prefixes_except(r"[a-z]{0,5}", "[a-zB]{0,6}Q", r#"(foo|bar)M"#);
}

#[test]
fn test_multiple_of() {
    for d in 1..=300 {
        let mut r = RegexBuilder::new();
        let id = r.mk(&RegexAst::MultipleOf(d)).unwrap();
        let mut rx = r.to_regex(id);
        assert!(!rx.is_match(""));
        assert!(!rx.is_match("-1"));
        for t in 0..(7 * d) {
            let s = format!("{}", t);
            if rx.is_match(&s) != (t % d == 0) {
                panic!("{} % {} == {}", t, d, t % d);
            }
        }
    }
}

fn remainder_is_check(should_be_empty: bool, d: u32, other_rx: &str) {
    let mut bld = RegexBuilder::new();
    let id = bld
        .mk(&RegexAst::And(vec![
            RegexAst::MultipleOf(d),
            RegexAst::Regex(other_rx.to_string()),
        ]))
        .unwrap();
    let mut rx = bld.to_regex(id);
    if rx.always_empty() != should_be_empty {
        panic!(
            "empty({} % & {:?}) != {}",
            d, other_rx, should_be_empty
        );
    }
}

fn remainder_is_empty(d: u32, other_rx: &str) {
    remainder_is_check(true, d, other_rx);
}

fn remainder_is_non_empty(d: u32, other_rx: &str) {
    remainder_is_check(false, d, other_rx);
}

#[test]
fn test_remainder_is_relevance() {
    remainder_is_non_empty(2, "[0-9]+");
    remainder_is_non_empty(3, "[2]+");
    remainder_is_empty(3, "[a-z]*");
    remainder_is_empty(2, "[3579]+");
}
