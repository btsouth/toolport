//! Decoded "logical" view of text for secret matching.
//!
//! A secret can be written with any mix of JSON `\uXXXX` escapes, percent
//! encoding and HTML entities, character by character. `normalize` decodes
//! two levels of those encodings and records, for every decoded char, the
//! source byte range it came from, so matcher hits on the decoded text can be
//! mapped back to the source with `source_range`.

/// Decoded view of `text` plus, for each logical char, the source byte range it came from.
pub struct Normalized {
    pub text: String,
    /// `spans[i]` is the source byte range of the i-th char (not byte) of `text`.
    pub spans: Vec<(usize, usize)>,
}

/// One decoded char and the source byte range it covers.
#[derive(Clone, Copy)]
struct Unit {
    c: char,
    s: usize,
    e: usize,
}

/// Longest supported entity name (`newline`).
const MAX_NAME: usize = 7;
/// Most digits accepted in a numeric entity.
const MAX_NUMERIC_DIGITS: usize = 7;
/// Bytes of tail examined by `incomplete_suffix_start`. The longest sequence
/// that can still be undecided is a surrogate pair (`\uD83D\uDE0`, 11 chars)
/// with every char itself encoded once (at most 10 bytes each, `&#0000092;`).
const SUFFIX_WINDOW: usize = 256;
/// Chars examined for an undecided sequence at one level.
const LEVEL_WINDOW: usize = 16;

/// Entity names, lowercase. Lookup is case-insensitive.
const ENTITIES: &[(&str, char)] = &[
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("nbsp", '\u{a0}'),
    ("excl", '!'),
    ("num", '#'),
    ("dollar", '$'),
    ("percnt", '%'),
    ("lpar", '('),
    ("rpar", ')'),
    ("ast", '*'),
    ("midast", '*'),
    ("plus", '+'),
    ("comma", ','),
    ("period", '.'),
    ("sol", '/'),
    ("colon", ':'),
    ("semi", ';'),
    ("equals", '='),
    ("quest", '?'),
    ("commat", '@'),
    ("lsqb", '['),
    ("lbrack", '['),
    ("bsol", '\\'),
    ("rsqb", ']'),
    ("rbrack", ']'),
    ("hat", '^'),
    ("lowbar", '_'),
    ("grave", '`'),
    ("lcub", '{'),
    ("lbrace", '{'),
    ("verbar", '|'),
    ("vert", '|'),
    ("rcub", '}'),
    ("rbrace", '}'),
    // HTML5 `&tilde;` is U+02DC SMALL TILDE, not `~`.
    ("tilde", '\u{2dc}'),
    ("newline", '\n'),
    ("tab", '\t'),
];

fn is_introducer(c: char) -> bool {
    matches!(c, '\\' | '%' | '&')
}

/// Whether `text` contains any introducer that could change under decoding
/// (`\`, `%`, `&`). If false, `normalize` would return `text` unchanged.
pub fn has_encoding(text: &str) -> bool {
    text.bytes().any(|b| matches!(b, b'\\' | b'%' | b'&'))
}

/// Decode two levels of JSON escapes, percent encoding and HTML entities.
pub fn normalize(text: &str) -> Normalized {
    if !has_encoding(text) {
        return Normalized {
            text: text.to_owned(),
            spans: text
                .char_indices()
                .map(|(i, c)| (i, i + c.len_utf8()))
                .collect(),
        };
    }
    let source = units_of(text, 0);
    let mut buf = Vec::new();
    let mut first = Vec::with_capacity(source.len());
    decode_level(&source, &mut first, &mut buf);
    let decoded = if first.iter().any(|u| is_introducer(u.c)) {
        let mut second = Vec::with_capacity(first.len());
        decode_level(&first, &mut second, &mut buf);
        second
    } else {
        first
    };
    let mut out = String::with_capacity(decoded.len());
    let mut spans = Vec::with_capacity(decoded.len());
    for u in &decoded {
        out.push(u.c);
        spans.push((u.s, u.e));
    }
    Normalized { text: out, spans }
}

/// Map a byte range of `n.text` back to the source byte range covering it
/// (start of the first char's source, end of the last char's source). An
/// empty range maps to an empty range at the corresponding source position.
pub fn source_range(n: &Normalized, start: usize, end: usize) -> (usize, usize) {
    let Some(&(_, source_end)) = n.spans.last() else {
        return (0, 0);
    };
    let len = n.text.len();
    let start = start.min(len);
    let end = end.clamp(start, len);
    let first = char_index(n, start);
    let count = char_count(&n.text.as_bytes()[start..end]);
    if count == 0 {
        let at = n.spans.get(first).map_or(source_end, |s| s.0);
        return (at, at);
    }
    (n.spans[first].0, n.spans[first + count - 1].1)
}

/// The byte offset in `text` from which `text` may end inside an incomplete
/// encoded sequence, at either decoding level, or None. Conservative: it may
/// hold back a sequence that turns out to be literal, never the reverse.
pub fn incomplete_suffix_start(text: &str) -> Option<usize> {
    let mut window = text.len().saturating_sub(SUFFIX_WINDOW);
    while !text.is_char_boundary(window) {
        window += 1;
    }
    let tail = &text[window..];
    if !has_encoding(tail) {
        return None;
    }
    // At each level, the decided part is decoded into the next level and
    // searched there for an undecided sequence reaching its end (which may
    // continue into what this level has not decided yet).
    let mut units = units_of(tail, window);
    let mut earliest: Option<usize> = None;
    let mut buf = Vec::new();
    for _ in 0..=MAX_LEVELS {
        let undecided = undecided_start(&units);
        if let Some(i) = undecided {
            let at = units[i].s;
            earliest = Some(earliest.map_or(at, |e| e.min(at)));
        }
        let decided = undecided.unwrap_or(units.len());
        if !units[..decided].iter().any(|u| is_introducer(u.c)) {
            break;
        }
        let mut next = Vec::with_capacity(decided);
        decode_level(&units[..decided], &mut next, &mut buf);
        units = next;
    }
    earliest
}

/// How many levels [`levels`] decodes at most: two levels of encoding (JSON
/// escapes, percent or HTML entities, in any mix) plus two JSON string
/// unwraps, which is what a secret inside MCP text content that itself carries
/// serialized JSON, read from a serialized log line, needs. Every decoding is
/// one level here whatever its kind, so any mix of four is covered.
pub const MAX_LEVELS: usize = 4;

/// Every decoded view of `text`, one per level (level 1 first), each with spans
/// back to `text`. A level is only produced when decoding changed something;
/// text with no introducer has none.
pub fn levels(text: &str) -> Vec<Normalized> {
    let mut out = Vec::new();
    if !has_encoding(text) {
        return out;
    }
    let mut units = units_of(text, 0);
    let mut buf = Vec::new();
    for _ in 0..MAX_LEVELS {
        if !units.iter().any(|u| is_introducer(u.c)) {
            break;
        }
        let mut next = Vec::with_capacity(units.len());
        decode_level(&units, &mut next, &mut buf);
        if next.len() == units.len() && next.iter().zip(&units).all(|(a, b)| a.c == b.c) {
            break;
        }
        let mut text_out = String::with_capacity(next.len());
        let mut spans = Vec::with_capacity(next.len());
        for u in &next {
            text_out.push(u.c);
            spans.push((u.s, u.e));
        }
        out.push(Normalized {
            text: text_out,
            spans,
        });
        units = next;
    }
    out
}

fn units_of(text: &str, base: usize) -> Vec<Unit> {
    text.char_indices()
        .map(|(i, c)| Unit {
            c,
            s: base + i,
            e: base + i + c.len_utf8(),
        })
        .collect()
}

fn char_count(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| b & 0xC0 != 0x80).count()
}

fn char_index(n: &Normalized, byte: usize) -> usize {
    if n.text.len() == n.spans.len() {
        byte
    } else {
        char_count(&n.text.as_bytes()[..byte])
    }
}

fn hex_at(u: &[Unit], i: usize) -> Option<u32> {
    u.get(i).and_then(|u| u.c.to_digit(16))
}

fn hex4(u: &[Unit], i: usize) -> Option<u32> {
    let mut v = 0;
    for k in 0..4 {
        v = (v << 4) | hex_at(u, i + k)?;
    }
    Some(v)
}

fn char_at(u: &[Unit], i: usize) -> Option<char> {
    u.get(i).map(|u| u.c)
}

/// Decode one level of `input` into `out`. `buf` is scratch for percent runs.
fn decode_level(input: &[Unit], out: &mut Vec<Unit>, buf: &mut Vec<u8>) {
    let mut i = 0;
    while i < input.len() {
        let rest = &input[i..];
        let decoded = match rest[0].c {
            '\\' => json_escape(rest),
            '&' => entity(rest),
            '%' => {
                let n = percent_run(rest, out, buf);
                if n > 0 {
                    i += n;
                    continue;
                }
                None
            }
            _ => None,
        };
        match decoded {
            Some((c, n)) => {
                out.push(Unit {
                    c,
                    s: rest[0].s,
                    e: rest[n - 1].e,
                });
                i += n;
            }
            None => {
                out.push(rest[0]);
                i += 1;
            }
        }
    }
}

/// A JSON escape at the start of `u`: the char and how many units it used.
fn json_escape(u: &[Unit]) -> Option<(char, usize)> {
    let c = match char_at(u, 1)? {
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        'b' => '\u{8}',
        'f' => '\u{c}',
        '/' => '/',
        '\\' => '\\',
        '"' => '"',
        'u' => {
            let v = hex4(u, 2)?;
            if (0xD800..0xDC00).contains(&v) {
                if char_at(u, 6) != Some('\\') || char_at(u, 7) != Some('u') {
                    return None;
                }
                let lo = hex4(u, 8)?;
                if !(0xDC00..0xE000).contains(&lo) {
                    return None;
                }
                let c = char::from_u32(0x10000 + ((v - 0xD800) << 10) + (lo - 0xDC00))?;
                return Some((c, 12));
            }
            return char::from_u32(v).map(|c| (c, 6));
        }
        _ => return None,
    };
    Some((c, 2))
}

/// Decode a run of `%XX` at the start of `u` into `out`; returns units used
/// (0 when `u` does not start with a valid `%XX`).
fn percent_run(u: &[Unit], out: &mut Vec<Unit>, buf: &mut Vec<u8>) -> usize {
    buf.clear();
    let mut j = 0;
    while char_at(u, j) == Some('%') {
        match (hex_at(u, j + 1), hex_at(u, j + 2)) {
            (Some(h), Some(l)) => buf.push(((h << 4) | l) as u8),
            _ => break,
        }
        j += 3;
    }
    let span = |b0: usize, len: usize| (u[3 * b0].s, u[3 * (b0 + len) - 1].e);
    let mut off = 0;
    for chunk in buf.utf8_chunks() {
        for (k, c) in chunk.valid().char_indices() {
            let (s, e) = span(off + k, c.len_utf8());
            out.push(Unit { c, s, e });
        }
        off += chunk.valid().len();
        for &b in chunk.invalid() {
            let (s, e) = span(off, 1);
            out.push(Unit {
                c: char::from(b),
                s,
                e,
            });
            off += 1;
        }
    }
    j
}

fn lookup_entity(name: &[u8]) -> Option<char> {
    ENTITIES
        .iter()
        .find(|(n, _)| n.as_bytes() == name)
        .map(|&(_, c)| c)
}

/// An HTML entity at the start of `u`: the char and how many units it used.
fn entity(u: &[Unit]) -> Option<(char, usize)> {
    let semi = |end: usize| usize::from(char_at(u, end) == Some(';'));
    if char_at(u, 1) == Some('#') {
        let (radix, digits_at) = match char_at(u, 2) {
            Some('x' | 'X') => (16, 3),
            _ => (10, 2),
        };
        let mut value: u32 = 0;
        let mut digits = 0;
        while let Some(d) = char_at(u, digits_at + digits).and_then(|c| c.to_digit(radix)) {
            digits += 1;
            if digits > MAX_NUMERIC_DIGITS {
                return None;
            }
            value = value * radix + d;
        }
        if digits == 0 {
            return None;
        }
        let c = char::from_u32(value)?;
        let end = digits_at + digits;
        return Some((c, end + semi(end)));
    }
    let mut name = [0u8; MAX_NAME];
    let mut len = 0;
    while len < MAX_NAME {
        match char_at(u, 1 + len) {
            Some(c) if c.is_ascii_alphabetic() => {
                name[len] = c.to_ascii_lowercase() as u8;
                len += 1;
            }
            _ => break,
        }
    }
    (2..=len).rev().find_map(|l| {
        lookup_entity(&name[..l]).map(|c| {
            let end = 1 + l;
            (c, end + semi(end))
        })
    })
}

/// Index of the earliest unit from which `u` may end inside an undecided sequence.
fn undecided_start(u: &[Unit]) -> Option<usize> {
    (u.len().saturating_sub(LEVEL_WINDOW)..u.len())
        .find(|&p| is_introducer(u[p].c) && is_undecided(&u[p..]))
}

/// Whether all of `s` (starting with an introducer) is a prefix of a sequence
/// that more input could still complete or change.
fn is_undecided(s: &[Unit]) -> bool {
    let all = |from: usize, f: fn(char) -> bool| s.iter().skip(from).all(|u| f(u.c));
    let is_hex = |c: char| c.is_ascii_hexdigit();
    match s[0].c {
        '\\' => match char_at(s, 1) {
            None => true,
            Some('u') if s.len() < 6 => all(2, is_hex),
            Some('u') => match hex4(s, 2) {
                // A complete high surrogate waits for its low half.
                Some(v) if (0xD800..0xDC00).contains(&v) && s.len() < 12 => {
                    char_at(s, 6).is_none_or(|c| c == '\\')
                        && char_at(s, 7).is_none_or(|c| c == 'u')
                        && all(8, is_hex)
                }
                _ => false,
            },
            Some(_) => false,
        },
        '%' => percent_undecided(s),
        '&' => match char_at(s, 1) {
            None => true,
            Some('#') => {
                let digits_at = if matches!(char_at(s, 2), Some('x' | 'X')) {
                    3
                } else {
                    2
                };
                let digits = s.len().saturating_sub(digits_at);
                digits <= MAX_NUMERIC_DIGITS && all(digits_at, is_hex)
            }
            Some(_) => {
                let name = &s[1..];
                name.len() <= MAX_NAME
                    && name.iter().all(|u| u.c.is_ascii_alphabetic())
                    && ENTITIES.iter().any(|(n, _)| {
                        n.len() >= name.len()
                            && n.chars()
                                .zip(name)
                                .all(|(a, u)| a == u.c.to_ascii_lowercase())
                    })
            }
        },
        _ => false,
    }
}

/// `%`, `%H`, or the `%XX` triples of an unfinished UTF-8 char, optionally
/// followed by `%` or `%H`.
fn percent_undecided(s: &[Unit]) -> bool {
    let mut bytes = [0u8; 4];
    let mut n = 0;
    let mut j = 0;
    while n < 4 && j + 3 <= s.len() && s[j].c == '%' {
        match (hex_at(s, j + 1), hex_at(s, j + 2)) {
            (Some(h), Some(l)) => bytes[n] = ((h << 4) | l) as u8,
            _ => break,
        }
        n += 1;
        j += 3;
    }
    let partial = &s[j..];
    let partial_ok = match partial.len() {
        0 => true,
        1 | 2 => partial[0].c == '%' && partial.get(1).is_none_or(|u| u.c.is_ascii_hexdigit()),
        _ => false,
    };
    if !partial_ok {
        return false;
    }
    if n == 0 {
        return !partial.is_empty();
    }
    let need = match bytes[0] {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return false,
    };
    n < need && bytes[1..n].iter().all(|b| (0x80..=0xBF).contains(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> String {
        normalize(s).text
    }

    fn assert_spans_valid(src: &str, n: &Normalized) {
        assert_eq!(n.text.chars().count(), n.spans.len());
        let mut prev = 0;
        for &(s, e) in &n.spans {
            assert!(
                prev <= s && s < e && e <= src.len(),
                "{src:?}: {:?}",
                n.spans
            );
            assert!(src.is_char_boundary(s) && src.is_char_boundary(e));
            prev = e;
        }
    }

    #[test]
    fn plain_text_is_identity() {
        for s in ["", "abc", "héllo wörld 😀", "a+b=c;#x"] {
            assert!(!has_encoding(s));
            let n = normalize(s);
            assert_eq!(n.text, s);
            let ident: Vec<_> = s
                .char_indices()
                .map(|(i, c)| (i, i + c.len_utf8()))
                .collect();
            assert_eq!(n.spans, ident);
        }
        assert!(has_encoding("a\\b") && has_encoding("50%") && has_encoding("a&b"));
    }

    #[test]
    fn json_escapes() {
        assert_eq!(text("\\u0041\\u00e9\\u00E9"), "Aéé");
        assert_eq!(
            text("\\n\\r\\t\\b\\f\\/\\\"x\\\\"),
            "\n\r\t\u{8}\u{c}/\"x\\"
        );
        // `\\` decodes to `\`, which level two may combine with what follows.
        assert_eq!(text("\\\\\\\""), "\"");
        assert_eq!(text("\\x\\u12\\uZZZZ\\"), "\\x\\u12\\uZZZZ\\");
        let n = normalize("a\\u0042c");
        assert_eq!(n.spans, vec![(0, 1), (1, 7), (7, 8)]);
    }

    #[test]
    fn surrogates() {
        let n = normalize("\\uD83D\\uDE00!");
        assert_eq!(n.text, "😀!");
        assert_eq!(n.spans, vec![(0, 12), (12, 13)]);
        assert_eq!(text("\\ud83d\\ude00"), "😀");
        assert_eq!(text("\\uD800x"), "\\uD800x");
        assert_eq!(text("\\uDE00"), "\\uDE00");
        assert_eq!(text("\\uD83D\\u0041"), "\\uD83DA");
        assert_eq!(text("\\uD83D\\uD83D\\uDE00"), "\\uD83D😀");
    }

    #[test]
    fn percent() {
        let n = normalize("%41%62x%E2%82%AC");
        assert_eq!(n.text, "Abx€");
        assert_eq!(n.spans, vec![(0, 3), (3, 6), (6, 7), (7, 16)]);
        assert_eq!(text("%c3%a9"), "é");
        assert_eq!(text("%FF%41"), "ÿA");
        assert_eq!(text("%E2%82"), "\u{e2}\u{82}");
        assert_eq!(text("%E2%82A"), "\u{e2}\u{82}A");
        assert_eq!(text("a+b%2B"), "a+b+");
        assert_eq!(text("%G1%4 %"), "%G1%4 %");
    }

    #[test]
    fn html_entities() {
        assert_eq!(text("&#65;&#66&#x43;&#X44&#x00065;"), "ABCDe");
        assert_eq!(text("&amp;&AMP&lt;&Gt&quot;&apos;&nbsp;"), "&&<>\"'\u{a0}");
        assert_eq!(
            text("&excl;&num;&dollar;&lpar;&rpar;&ast;&plus;&comma;&period;&sol;&colon;&semi;"),
            "!#$()*+,./:;"
        );
        assert_eq!(
            text("&equals;&quest;&commat;&lsqb;&rsqb;&hat;&lowbar;&grave;&lcub;&verbar;&rcub;"),
            "=?@[]^_`{|}"
        );
        assert_eq!(text("&newline;&tab;&tilde;"), "\n\t\u{2dc}");
        assert_eq!(text("&ampx &ltb"), "&x <b");
        assert_eq!(
            text("&foo; &#; &#x; &#99999999; &#x110000; &#xD800; &"),
            "&foo; &#; &#x; &#99999999; &#x110000; &#xD800; &"
        );
        let n = normalize("&#66;x&lt");
        assert_eq!(n.spans, vec![(0, 5), (5, 6), (6, 9)]);
    }

    #[test]
    fn mixed_literal_and_encoded() {
        let n = normalize("s%6B-A&#66;C");
        assert_eq!(n.text, "sk-ABC");
        assert_eq!(
            n.spans,
            vec![(0, 1), (1, 4), (4, 5), (5, 6), (6, 11), (11, 12)]
        );
        assert_eq!(text("s\\u006B%2DA&#x42C"), "sk-A\u{42c}");
        assert_eq!(text("s\\u006B%2DA&#x42;C"), "sk-ABC");
    }

    #[test]
    fn nested_once() {
        let n = normalize("%5Cu0041z");
        assert_eq!(n.text, "Az");
        assert_eq!(n.spans, vec![(0, 8), (8, 9)]);
        let n = normalize("\\u002541");
        assert_eq!(n.text, "A");
        assert_eq!(n.spans, vec![(0, 8)]);
        assert_eq!(text("&#37;41"), "A");
        assert_eq!(text("&amp;lt;"), "<");
        assert_eq!(text("&bsol;u0041"), "A");
        assert_eq!(text("\\\\u0041"), "A");
        // A third level is not decoded.
        assert_eq!(text("%252541"), "%41");
    }

    #[test]
    fn source_range_maps_back() {
        let n = normalize("s%6B-A&#66;C");
        assert_eq!(source_range(&n, 1, 4), (1, 6));
        assert_eq!(source_range(&n, 4, 5), (6, 11));
        assert_eq!(source_range(&n, 0, 6), (0, 12));
        assert_eq!(source_range(&n, 2, 2), (4, 4));
        assert_eq!(source_range(&n, 6, 6), (12, 12));
        let n = normalize("%E2%82%ACx\\u00e9");
        assert_eq!(n.text, "€xé");
        assert_eq!(source_range(&n, 0, 3), (0, 9));
        assert_eq!(source_range(&n, 3, 4), (9, 10));
        assert_eq!(source_range(&n, 3, 6), (9, 16));
        assert_eq!(source_range(&normalize(""), 0, 0), (0, 0));
    }

    #[test]
    fn incomplete_suffix() {
        for (s, want) in [
            ("abc\\u00", Some(3)),
            ("x\\", Some(1)),
            ("x\\u", Some(1)),
            ("x%", Some(1)),
            ("x%4", Some(1)),
            ("x&", Some(1)),
            ("x&#", Some(1)),
            ("x&#x4", Some(1)),
            ("x&#1234567", Some(1)),
            ("x&am", Some(1)),
            ("x&amp", Some(1)),
            ("x&LOWBA", Some(1)),
            ("x\\uD83D", Some(1)),
            ("x\\uD83D\\", Some(1)),
            ("x\\uD83D\\uDE", Some(1)),
            ("x%E2", Some(1)),
            ("x%E2%82", Some(1)),
            ("x%E2%8", Some(1)),
            ("x%41%4", Some(4)),
            // Nested: complete at level one, undecided at level two.
            ("x%5Cu00", Some(1)),
            ("x&#37;4", Some(1)),
            ("x\\u0025", Some(1)),
            ("x&amp;", Some(1)),
            ("x\\\\", Some(1)),
            ("x%5", Some(1)),
            ("x%5Cu00%34", Some(1)),
            ("abc", None),
            ("x%41", None),
            ("x\\u0041", None),
            ("x\\n", None),
            ("x&lt;", None),
            ("x&#65;", None),
            ("x&#12345678", None),
            ("x&foo", None),
            ("x%G", None),
            ("x%E2%82%AC", None),
            ("x\\uD83D\\uDE00", None),
            ("x\\uD83D\\u0041", None),
            ("50% off", None),
        ] {
            assert_eq!(incomplete_suffix_start(s), want, "{s:?}");
        }
        let long = format!("{}%4", "a".repeat(10_000));
        assert_eq!(incomplete_suffix_start(&long), Some(10_000));
        let long = format!("{}é\\u00", "ü".repeat(300));
        assert_eq!(incomplete_suffix_start(&long), Some(602));
    }

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as u32
        }
        fn below(&mut self, n: u32) -> u32 {
            self.next() % n
        }
    }

    fn hex(rng: &mut Lcg, value: u32, width: usize) -> String {
        let digits = if width == 0 {
            format!("{value:x}")
        } else {
            format!("{value:0width$x}")
        };
        digits
            .chars()
            .map(|c| {
                if rng.below(2) == 0 {
                    c.to_ascii_uppercase()
                } else {
                    c
                }
            })
            .collect()
    }

    #[test]
    fn random_mixed_encodings_round_trip() {
        let mut rng = Lcg(0x5EC2_E7ED);
        // Printable ASCII except the introducers, whose literal use could form
        // new sequences together with decoded neighbours.
        let alphabet: Vec<char> = (0x20u8..0x7F)
            .map(char::from)
            .filter(|&c| !is_introducer(c))
            .chain(['\n', '\t'])
            .collect();
        for _ in 0..2_000 {
            let len = rng.below(40) as usize;
            let original: String = (0..len)
                .map(|_| alphabet[rng.below(alphabet.len() as u32) as usize])
                .collect();
            // (encoding, droppable trailing `;`)
            let mut pieces: Vec<(String, bool)> = original
                .chars()
                .map(|c| {
                    let v = c as u32;
                    match rng.below(5) {
                        0 => (c.to_string(), false),
                        1 => (format!("\\u{}", hex(&mut rng, v, 4)), false),
                        2 => (format!("%{}", hex(&mut rng, v, 2)), false),
                        3 => (format!("&#{v};"), true),
                        _ => {
                            let x = if rng.below(2) == 0 { 'x' } else { 'X' };
                            (format!("&#{x}{};", hex(&mut rng, v, 0)), true)
                        }
                    }
                })
                .collect();
            for i in 0..pieces.len() {
                let next = pieces.get(i + 1).and_then(|p| p.0.chars().next());
                let safe = next.is_none_or(|c| !c.is_ascii_hexdigit() && c != ';');
                if pieces[i].1 && safe && rng.below(2) == 0 {
                    pieces[i].0.pop();
                }
            }
            let mut encoded = String::new();
            let mut expected = Vec::new();
            for (p, _) in &pieces {
                expected.push((encoded.len(), encoded.len() + p.len()));
                encoded.push_str(p);
            }
            let n = normalize(&encoded);
            assert_eq!(n.text, original, "{encoded:?}");
            assert_spans_valid(&encoded, &n);
            assert_eq!(n.spans, expected, "{encoded:?}");
            if !original.is_empty() {
                assert_eq!(source_range(&n, 0, n.text.len()), (0, encoded.len()));
            }
            assert_eq!(
                incomplete_suffix_start(&encoded).is_some(),
                {
                    // Only an entity left without `;` at the very end is undecided.
                    pieces
                        .last()
                        .is_some_and(|(p, _)| p.starts_with('&') && !p.ends_with(';'))
                },
                "{encoded:?}"
            );
        }
    }
}
