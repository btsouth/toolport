//! False-positive rate of the secret matcher's three length thresholds:
//! fragment windows (window length and the shortest secret windowed), dangling
//! edges (`redact_fragment`) and embedded base64 (inner strings).
//!
//! A realistic secret set is generated at random (API tokens, JWTs, PEM keys,
//! passwords, UUIDs, URLs with tokens), so no corpus text can contain one: every
//! replaced span is a false positive. Each threshold is swept with the other two
//! at their production values, and every corpus line is run through
//! `redact_text` (the log path) and `redact_fragment` (cut text: remote error
//! lines, truncation, stream ends).
//!
//! `cargo run --release --example secret-redact-false-positives -- LABEL=PATH...`
//!
//! Each PATH is a file or a directory walked recursively; text files only.
//! `--coverage` prints only the table of leaks each threshold alone prevents.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use conduit_lib::secret_redact::{Matcher, Thresholds};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn from(&mut self, alphabet: &str, n: usize) -> String {
        let a: Vec<char> = alphabet.chars().collect();
        (0..n).map(|_| a[self.next() as usize % a.len()]).collect()
    }
}

const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const URLSAFE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const B64: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const HEX: &str = "0123456789abcdef";
const DIGITS: &str = "0123456789";

fn b64url(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn pem(r: &mut Rng, label: &str, prefix: &str, body_chars: usize) -> String {
    let body = format!("{prefix}{}", r.from(B64, body_chars - prefix.len()));
    let lines: Vec<&str> = body
        .as_bytes()
        .chunks(64)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();
    format!(
        "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
        lines.join("\n")
    )
}

/// About 45 values, the secrets of a dozen configured servers.
fn secret_set() -> Vec<(String, String)> {
    let mut r = Rng(0x5eed_f00d);
    let mut s: Vec<(String, String)> = Vec::new();
    let mut add = |name: &str, value: String| s.push((format!("FPX_{name}"), value));
    add("openai", format!("sk-proj-{}", r.from(URLSAFE, 156)));
    add("openai_legacy", format!("sk-{}", r.from(ALNUM, 48)));
    add(
        "anthropic",
        format!(
            "sk-ant-api03-{}-{}AA",
            r.from(URLSAFE, 86),
            r.from(URLSAFE, 6)
        ),
    );
    add("github", format!("ghp_{}", r.from(ALNUM, 36)));
    add(
        "github_pat",
        format!("github_pat_11{}_{}", r.from(ALNUM, 20), r.from(ALNUM, 59)),
    );
    add(
        "slack_bot",
        format!(
            "xoxb-{}-{}-{}",
            r.from(DIGITS, 12),
            r.from(DIGITS, 13),
            r.from(ALNUM, 24)
        ),
    );
    add(
        "slack_user",
        format!(
            "xoxp-{}-{}-{}-{}",
            r.from(DIGITS, 11),
            r.from(DIGITS, 12),
            r.from(DIGITS, 13),
            r.from(HEX, 32)
        ),
    );
    add(
        "aws_key_id",
        format!("AKIA{}", r.from("ABCDEFGHIJKLMNOPQRSTUVWXYZ234567", 16)),
    );
    add("aws_secret", r.from(B64, 40));
    add("stripe", format!("sk_live_{}", r.from(ALNUM, 99)));
    add("google", format!("AIza{}", r.from(URLSAFE, 35)));
    add("npm", format!("npm_{}", r.from(ALNUM, 36)));
    add("linear", format!("lin_api_{}", r.from(ALNUM, 40)));
    add("notion", format!("ntn_{}", r.from(ALNUM, 46)));
    add(
        "telegram",
        format!("{}:AA{}", r.from(DIGITS, 10), r.from(URLSAFE, 33)),
    );
    add("hex32", r.from(HEX, 32));
    add("hex40", r.from(HEX, 40));
    add("hex64", r.from(HEX, 64));
    add("hex64_upper", r.from(HEX, 64).to_uppercase());
    add("uuid_a", uuid(&mut r));
    add("uuid_b", uuid(&mut r));
    // JWTs share their header; payloads start with the same claim names.
    for (i, (header, claims)) in [
        (
            r#"{"alg":"HS256","typ":"JWT"}"#,
            r#"{"sub":"1234567890","name":"svc","iat":1731600000}"#,
        ),
        (
            r#"{"alg":"RS256","typ":"JWT"}"#,
            r#"{"iss":"https://auth.example.com/","sub":"auth0|65f0","aud":"api","exp":1731686400}"#,
        ),
        (
            r#"{"alg":"RS256","typ":"JWT","kid":"k1"}"#,
            r#"{"exp":1731686400,"iat":1731600000,"jti":"9f1c","iss":"https://login.example.org"}"#,
        ),
    ]
    .iter()
    .enumerate()
    {
        let sig = b64url(r.from(ALNUM, 256).as_bytes());
        let sig = &sig[..if i == 0 { 43 } else { 342 }];
        add(
            &format!("jwt{i}"),
            format!(
                "{}.{}.{sig}",
                b64url(header.as_bytes()),
                b64url(claims.as_bytes())
            ),
        );
    }
    // Keys: every RSA 2048 PKCS#8 key and every OpenSSH key starts alike.
    add(
        "rsa_key",
        pem(
            &mut r,
            "PRIVATE KEY",
            "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQ",
            1624,
        ),
    );
    add(
        "ssh_key",
        pem(
            &mut r,
            "OPENSSH PRIVATE KEY",
            "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW",
            400,
        ),
    );
    add("ec_key", pem(&mut r, "EC PRIVATE KEY", "MHcCAQEEI", 164));
    // Passwords people choose.
    for (i, pw) in [
        "Password123!",
        "Summer2024!",
        "Tr0ub4dor&3",
        "admin123",
        "Qwerty!23",
        "correct horse battery staple",
        "Correct-Horse-Battery-Staple-42",
        "MyDogRex#1987",
        "Welcome2Bluebird!",
        "letmein-prod-2025",
        "hunter2",
        "s3cr3t!",
    ]
    .iter()
    .enumerate()
    {
        add(&format!("password{i}"), pw.to_string());
    }
    add("pin", "4821".into());
    add("numeric", "938271".into());
    // URLs and headers: registered whole, the token inside on its own.
    let hook = r.from(ALNUM, 24);
    add("slack_hook_token", hook.clone());
    add(
        "slack_hook",
        format!(
            "https://hooks.slack.com/services/T0{}/B0{}/{hook}",
            r.from("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789", 8),
            r.from("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789", 9)
        ),
    );
    let dbpw = r.from(ALNUM, 20);
    add("db_password", dbpw.clone());
    add(
        "db_url",
        format!("postgres://app:{dbpw}@db.internal:5432/app?sslmode=require"),
    );
    let qtoken = r.from(URLSAFE, 32);
    add("api_query_token", qtoken.clone());
    add(
        "api_url",
        format!("https://api.example.com/v1/events?token={qtoken}"),
    );
    let bearer = r.from(URLSAFE, 40);
    add("bearer_token", bearer.clone());
    add("auth_header", format!("Bearer {bearer}"));
    s
}

fn uuid(r: &mut Rng) -> String {
    format!(
        "{}-{}-4{}-{}{}-{}",
        r.from(HEX, 8),
        r.from(HEX, 4),
        r.from(HEX, 3),
        r.from("89ab", 1),
        r.from(HEX, 3),
        r.from(HEX, 12)
    )
}

struct Corpus {
    /// (source label, line)
    lines: Vec<(usize, String)>,
    labels: Vec<String>,
    bytes: Vec<usize>,
    files: Vec<usize>,
}

fn walk(path: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_dir() {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if matches!(name, ".git" | "target" | ".verify") {
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for e in entries {
            walk(&e, out);
        }
    } else if meta.is_file() && meta.len() <= 8 << 20 {
        out.push(path.to_path_buf());
    }
}

fn load(args: &[String]) -> Corpus {
    let mut c = Corpus {
        lines: Vec::new(),
        labels: Vec::new(),
        bytes: Vec::new(),
        files: Vec::new(),
    };
    for arg in args {
        let (label, path) = arg.split_once('=').expect("LABEL=PATH");
        let idx = match c.labels.iter().position(|l| l == label) {
            Some(i) => i,
            None => {
                c.labels.push(label.to_string());
                c.bytes.push(0);
                c.files.push(0);
                c.labels.len() - 1
            }
        };
        let mut files = Vec::new();
        walk(Path::new(path), &mut files);
        for f in files {
            let Ok(bytes) = std::fs::read(&f) else {
                continue;
            };
            if bytes.contains(&0) {
                continue;
            }
            let Ok(text) = String::from_utf8(bytes) else {
                continue;
            };
            c.bytes[idx] += text.len();
            c.files[idx] += 1;
            for line in text.lines() {
                c.lines.push((idx, line.to_string()));
            }
        }
    }
    c
}

/// A synthetic audit log in the shape `npm run bench:audit` generates, with
/// request ids and payload hashes so it is not all one repeated row.
fn audit_fixture(c: &mut Corpus) {
    let mut r = Rng(42);
    c.labels.push("audit-fixture".into());
    c.bytes.push(0);
    c.files.push(1);
    let idx = c.labels.len() - 1;
    for i in 0..10_000 {
        let line = serde_json::json!({
            "ts": 1731600000000u64 + i * 997,
            "server": format!("server-{}", i % 10),
            "tool": format!("tool-{}", i % 100),
            "ok": i % 11 != 0,
            "durationMs": i % 500,
            "requestId": uuid(&mut r),
            "argsHash": r.from(HEX, 64),
            "error": if i % 11 == 0 { format!("upstream returned 401: invalid token {}", r.from(B64, 24)) } else { String::new() },
        })
        .to_string();
        c.bytes[idx] += line.len() + 1;
        c.lines.push((idx, line));
    }
}

/// Log lines carrying other parties' JWTs (not registered): the same standard
/// headers and claim names as the registered ones, new random values.
fn foreign_jwts(c: &mut Corpus) {
    let mut r = Rng(7);
    c.labels.push("foreign-jwt-log".into());
    c.bytes.push(0);
    c.files.push(1);
    let idx = c.labels.len() - 1;
    let headers = [
        r#"{"alg":"HS256","typ":"JWT"}"#,
        r#"{"alg":"RS256","typ":"JWT"}"#,
        r#"{"alg":"RS256","typ":"JWT","kid":"k1"}"#,
    ];
    for i in 0..2_000u64 {
        let header = headers[i as usize % 3];
        let claims = format!(
            r#"{{"sub":"{}","iat":{},"exp":{}}}"#,
            r.from(DIGITS, 10),
            1731600000 + i,
            1731686400 + i
        );
        let sig = b64url(r.from(ALNUM, 64).as_bytes());
        let line = format!(
            "2026-11-15T10:{:02}:{:02}Z INFO upstream request id={} authorization=\"Bearer {}.{}.{}\"",
            i / 60 % 60,
            i % 60,
            uuid(&mut r),
            b64url(header.as_bytes()),
            b64url(claims.as_bytes()),
            &sig[..43]
        );
        c.bytes[idx] += line.len() + 1;
        c.lines.push((idx, line));
    }
}

const MARK: &str = "[secret:FPX";

fn hits(s: &str) -> usize {
    s.matches(MARK).count()
}

/// Markers this redaction added to `input`: a line it left as it was adds
/// none, whatever markers it already held.
fn new_hits(input: &str, output: &str) -> usize {
    if output == input {
        return 0;
    }
    hits(output).saturating_sub(hits(input))
}

/// The input text each marker in `output` replaced, as best it can be lined up.
fn hidden(input: &str, output: &str) -> Vec<String> {
    let mut segs: Vec<&str> = Vec::new();
    let mut rest = output;
    while let Some(i) = rest.find(MARK) {
        segs.push(&rest[..i]);
        let after = &rest[i..];
        let end = after.find(']').map_or(after.len(), |e| e + 1);
        rest = &after[end..];
    }
    segs.push(rest);
    let mut out = Vec::new();
    let mut pos = segs[0].len().min(input.len());
    for (k, seg) in segs.iter().enumerate().skip(1) {
        let found = if k == segs.len() - 1 && seg.is_empty() {
            Some(input.len())
        } else if seg.is_empty() {
            None
        } else if k == segs.len() - 1 {
            input
                .len()
                .checked_sub(seg.len())
                .filter(|&s| s >= pos && input.is_char_boundary(s))
        } else {
            input.get(pos..).and_then(|t| t.find(seg)).map(|f| pos + f)
        };
        match found {
            Some(f) if f >= pos => {
                out.push(input[pos..f].to_string());
                pos = f + seg.len();
            }
            _ => {}
        }
    }
    out
}

#[derive(Default)]
struct Tally {
    text_hits: usize,
    text_lines: usize,
    frag_hits: usize,
    frag_lines: usize,
    text_top: HashMap<String, usize>,
    frag_top: HashMap<String, usize>,
    /// Per source: text hits, text lines changed, fragment hits, fragment lines.
    per_source: Vec<[usize; 4]>,
    memory: usize,
}

fn run(c: &Corpus, secrets: &[(String, String)], t: &Thresholds, top: bool) -> Tally {
    let m = Matcher::new_tuned(secrets, &whole_only(secrets), t);
    assert!(!m.is_broken());
    let mut tally = Tally {
        per_source: vec![[0; 4]; c.labels.len()],
        memory: m.memory_usage(),
        ..Default::default()
    };
    for (src, line) in &c.lines {
        let text = m.redact_text(line);
        let h = new_hits(line, &text);
        if h > 0 {
            tally.text_hits += h;
            tally.text_lines += 1;
            tally.per_source[*src][0] += h;
            tally.per_source[*src][1] += 1;
            if top {
                for s in hidden(line, &text) {
                    *tally.text_top.entry(s).or_default() += 1;
                }
            }
        }
        let frag = m.redact_fragment(line);
        let h = new_hits(line, &frag);
        if h > 0 {
            tally.frag_hits += h;
            tally.frag_lines += 1;
            tally.per_source[*src][2] += h;
            tally.per_source[*src][3] += 1;
            if top {
                for s in hidden(line, &frag) {
                    *tally.frag_top.entry(s).or_default() += 1;
                }
            }
        }
    }
    tally
}

/// As the registry does: a URL or header holding a registered token is matched
/// whole, never by fragment (it is a container anyway).
fn whole_only(secrets: &[(String, String)]) -> HashSet<String> {
    secrets
        .iter()
        .filter(|(n, _)| n.ends_with("_url") || n.ends_with("_hook") || n.ends_with("_header"))
        .map(|(_, v)| v.clone())
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--coverage") {
        coverage(&secret_set());
        return;
    }
    let mut corpus = load(&args);
    audit_fixture(&mut corpus);
    foreign_jwts(&mut corpus);
    let secrets = secret_set();
    // No random secret can appear in the corpus. A chosen password can (an
    // xkcd quote): those whole-value hits are in every row, the "all three
    // off" row included, and are listed here.
    let mut whole: HashMap<&str, usize> = HashMap::new();
    for (_, line) in &corpus.lines {
        for (name, value) in &secrets {
            if value.chars().count() >= 8 && line.contains(value.as_str()) {
                *whole.entry(name.as_str()).or_default() += 1;
            }
        }
    }
    for (name, n) in &whole {
        assert!(name.starts_with("FPX_password"), "corpus contains {name}");
        println!("whole-value occurrences of chosen {name}: {n} lines");
    }
    let total_bytes: usize = corpus.bytes.iter().sum();
    let mb = total_bytes as f64 / 1_000_000.0;
    let lines = corpus.lines.len();
    println!(
        "corpus: {:.2} MB, {} lines, {} secrets",
        mb,
        lines,
        secrets.len()
    );
    for (i, l) in corpus.labels.iter().enumerate() {
        println!(
            "  source {l}: {:.2} MB in {} files",
            corpus.bytes[i] as f64 / 1e6,
            corpus.files[i]
        );
    }
    let d = Thresholds::default();
    let off = usize::MAX;
    let mut configs: Vec<(String, Thresholds, bool)> = vec![
        ("default".into(), d, true),
        (
            "all three off".into(),
            Thresholds {
                fragment_min_secret_chars: off,
                edge_chars: off,
                base64_inner_chars: off,
                ..d
            },
            true,
        ),
    ];
    for w in [6, 8, 10, 12, 16, 20] {
        configs.push((
            format!("window_chars={w}"),
            Thresholds {
                fragment_chars: w,
                ..d
            },
            w == 6,
        ));
    }
    for w in [8, 10] {
        configs.push((
            format!("window={w}, letters 1 class"),
            Thresholds {
                fragment_chars: w,
                letters_one_class: true,
                ..d
            },
            w == 8,
        ));
    }
    configs.push((
        "windows off".into(),
        Thresholds {
            fragment_min_secret_chars: off,
            ..d
        },
        true,
    ));
    for n in [8, 12, 16, 20] {
        configs.push((
            format!("window_min_secret={n}"),
            Thresholds {
                fragment_min_secret_chars: n,
                ..d
            },
            false,
        ));
    }
    for e in [4, 6, 8, 10, 12] {
        configs.push((
            format!("edge_chars={e}"),
            Thresholds { edge_chars: e, ..d },
            e == 4,
        ));
    }
    configs.push((
        "edges off".into(),
        Thresholds {
            edge_chars: off,
            ..d
        },
        false,
    ));
    for b in [8, 12, 16, 20, 24] {
        configs.push((
            format!("base64_inner={b}"),
            Thresholds {
                base64_inner_chars: b,
                ..d
            },
            b <= 12,
        ));
    }
    configs.push((
        "base64 inner off".into(),
        Thresholds {
            base64_inner_chars: off,
            ..d
        },
        false,
    ));
    // The synthetic log of other parties' JWTs is reported on its own: it is
    // built to show the shared-header case, not a typical text mix.
    let jwt = corpus
        .labels
        .iter()
        .position(|l| l == "foreign-jwt-log")
        .unwrap();
    let mut src_lines = vec![0usize; corpus.labels.len()];
    for (src, _) in &corpus.lines {
        src_lines[*src] += 1;
    }
    let nat_mb = (total_bytes - corpus.bytes[jwt]) as f64 / 1e6;
    let nat_lines = (lines - src_lines[jwt]) as f64;
    let jwt_mb = corpus.bytes[jwt] as f64 / 1e6;
    let jwt_lines = src_lines[jwt] as f64;
    println!(
        "\nnatural corpus {nat_mb:.2} MB / {nat_lines} lines; jwt log {jwt_mb:.2} MB / {jwt_lines} lines"
    );
    println!(
        "{:<26} {:>9} {:>9} {:>9} {:>9} | {:>9} {:>9} | {:>8}",
        "config", "text/MB", "text %ln", "frag/MB", "frag %ln", "jwt/MB", "jwt %ln", "KiB"
    );
    let mut tops = Vec::new();
    for (name, t, top) in &configs {
        let tally = run(&corpus, &secrets, t, *top);
        let mut nat = [0usize; 4];
        for (i, row) in tally.per_source.iter().enumerate() {
            if i != jwt {
                for k in 0..4 {
                    nat[k] += row[k];
                }
            }
        }
        let j = tally.per_source[jwt];
        println!(
            "{:<26} {:>9.2} {:>8.4}% {:>9.2} {:>8.4}% | {:>9.0} {:>8.2}% | {:>8}",
            name,
            nat[0] as f64 / nat_mb,
            100.0 * nat[1] as f64 / nat_lines,
            nat[2] as f64 / nat_mb,
            100.0 * nat[3] as f64 / nat_lines,
            j[0] as f64 / jwt_mb,
            100.0 * j[1] as f64 / jwt_lines,
            tally.memory / 1024,
        );
        if *top {
            tops.push((name.clone(), tally));
        }
    }
    for (name, tally) in tops {
        println!("\n== {name}: hits per source (text, fragment)");
        for (i, l) in corpus.labels.iter().enumerate() {
            let [a, _, b, _] = tally.per_source[i];
            if a + b > 0 {
                println!("  {l}: {a}, {b}");
            }
        }
        for (kind, map) in [("text", &tally.text_top), ("fragment", &tally.frag_top)] {
            let mut v: Vec<_> = map.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            println!("  top {kind} spans:");
            for (s, n) in v.iter().take(12) {
                let shown: String = s.chars().take(70).collect();
                println!("    {n:>6}  {shown:?}");
            }
        }
    }
    coverage(&secrets);
}

/// The leaks each threshold alone prevents: text where the production matcher
/// removes a piece of a secret and the matcher without that threshold (or with
/// a stricter value) writes it. `leaks` when the piece survives.
fn coverage(secrets: &[(String, String)]) {
    use base64::Engine as _;
    let get = |name: &str| {
        secrets
            .iter()
            .find(|(n, _)| n == &format!("FPX_{name}"))
            .unwrap()
            .1
            .clone()
    };
    let openai = get("openai");
    let pw13 = get("password7"); // MyDogRex#1987
    let pw11 = get("password1"); // Summer2024!
    let phrase = get("password5"); // correct horse battery staple
    let slack = get("slack_bot");
    let piece =
        |v: &str, from: usize, len: usize| v.chars().skip(from).take(len).collect::<String>();
    let basic = |user: &str, pw: &str| {
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pw}"))
    };
    // (case, entry point is redact_fragment, text, piece that must not survive)
    let cases: Vec<(&str, bool, String, String)> = vec![
        (
            "mid-text piece of a token (upstream masked it)",
            false,
            format!("401: key {}... is invalid", piece(&openai, 0, 24)),
            piece(&openai, 8, 16),
        ),
        (
            "mid-text piece of a 13-char password",
            false,
            format!("login 'MyDog{}' rejected", piece(&pw13, 5, 6)),
            piece(&pw13, 0, 11),
        ),
        (
            "11-char password cut at line end",
            true,
            format!("auth failed for password {}", piece(&pw11, 0, 7)),
            piece(&pw11, 0, 7),
        ),
        (
            "passphrase cut at line end (no windows: one class)",
            true,
            format!("bad passphrase: {}", piece(&phrase, 0, 18)),
            piece(&phrase, 0, 18),
        ),
        (
            "passphrase tail at line start",
            true,
            format!("{} rejected", piece(&phrase, 10, 100)),
            piece(&phrase, 10, 100),
        ),
        (
            "Slack token tail at line start",
            true,
            format!("{} rejected", piece(&slack, 6, 100)),
            piece(&slack, 6, 100),
        ),
    ];
    // A username of 3k bytes plus the colon leaves the password aligned, so its
    // own base64 matches; "bob" and "dave" shift it by one and two bytes.
    let mut cases = cases;
    for (user, pw, label) in [
        ("alice", &pw11, "Basic alice:<11-char password>"),
        ("bob", &pw11, "Basic bob:<11-char password>"),
        ("dave", &pw11, "Basic dave:<11-char password>"),
        ("bob", &pw13, "Basic bob:<13-char password>"),
        ("dave", &pw13, "Basic dave:<13-char password>"),
        ("dave", &openai, "Basic dave:<openai key>"),
    ] {
        cases.push((
            label,
            false,
            format!("Authorization: Basic {}", basic(user, pw)),
            basic(user, pw),
        ));
    }
    let d = Thresholds::default();
    let off = usize::MAX;
    let configs: Vec<(&str, Thresholds)> = vec![
        ("default", d),
        (
            "windows off",
            Thresholds {
                fragment_min_secret_chars: off,
                ..d
            },
        ),
        (
            "window 10",
            Thresholds {
                fragment_chars: 10,
                ..d
            },
        ),
        (
            "win letters 1cls",
            Thresholds {
                letters_one_class: true,
                ..d
            },
        ),
        (
            "win min 16",
            Thresholds {
                fragment_min_secret_chars: 16,
                ..d
            },
        ),
        (
            "edges off",
            Thresholds {
                edge_chars: off,
                ..d
            },
        ),
        ("edge 8", Thresholds { edge_chars: 8, ..d }),
        (
            "b64 inner 12",
            Thresholds {
                base64_inner_chars: 12,
                ..d
            },
        ),
        (
            "b64 inner off",
            Thresholds {
                base64_inner_chars: off,
                ..d
            },
        ),
    ];
    println!("\n== coverage (leaks = the piece is written)");
    print!("{:<52}", "case");
    for (name, _) in &configs {
        print!(" {name:>16}");
    }
    println!();
    let matchers: Vec<Matcher> = configs
        .iter()
        .map(|(_, t)| Matcher::new_tuned(secrets, &whole_only(secrets), t))
        .collect();
    for (case, fragment, text, secret_piece) in &cases {
        print!("{case:<52}");
        for m in &matchers {
            let out = if *fragment {
                m.redact_fragment(text)
            } else {
                m.redact_text(text)
            };
            let verdict = if out.contains(secret_piece.as_str()) {
                "leaks"
            } else {
                "caught"
            };
            print!(" {verdict:>16}");
        }
        println!();
    }
}

#[cfg(test)]
mod tests {
    /// Failure mode: a corpus line that already holds a marker (an earlier
    /// redaction quoted in a log) counted as a hit and a changed line even
    /// when redaction left it as it was.
    #[test]
    fn a_marker_already_in_the_input_is_not_a_hit() {
        let line = "copied from a log: [secret:FPX_not_registered] and more";
        assert_eq!(super::new_hits(line, line), 0);
        let redacted = "copied from a log: [secret:FPX_not_registered] and [secret:FPX_1]";
        assert_eq!(
            super::new_hits(line.replace("more", "x").as_str(), redacted),
            1
        );
    }
}
