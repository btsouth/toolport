//! `toolport report`: a single static HTML report card of `scan` and `usage`.
//!
//! One self-contained file: inline CSS that follows the system light or dark theme, no
//! scripts, no fonts, images or requests to anywhere. Every value is HTML-escaped, and the
//! report holds counts, names and paths only. Secret values never reach it because neither
//! report it is built from carries them.

use std::path::{Path, PathBuf};

use super::format;
use super::scan::{ScanReport, Snapshot};
use super::usage::UsageReport;
use super::{Args, CliError, Ctx, EXIT_OK};

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

const CSS: &str = r#"
:root{--bg:#f7f7f5;--card:#fff;--ink:#1d1d1f;--dim:#6b6b70;--line:#e4e4e0;--accent:#2f6fec;--warn:#b7791f;--bad:#c0392b;--ok:#2e8b57;--floor:#8e44ad;--mcp:#d4a017;--img:#c0392b}
@media (prefers-color-scheme:dark){:root{--bg:#111214;--card:#1a1b1e;--ink:#ececef;--dim:#9a9aa2;--line:#2b2c31;--accent:#6ea0ff;--warn:#e0a84a;--bad:#ef6b5b;--ok:#5cc48a;--floor:#c08ae0;--mcp:#f0c64a;--img:#ef6b5b}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:15px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Helvetica,Arial,sans-serif}
main{max-width:980px;margin:0 auto;padding:32px 16px 48px}
header h1{font-size:26px;margin:0 0 4px;letter-spacing:-.01em}header p{margin:0;color:var(--dim)}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(170px,1fr));gap:12px;margin:24px 0}
.stat{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:14px 16px}
.stat b{display:block;font-size:24px;font-variant-numeric:tabular-nums}.stat span{color:var(--dim);font-size:13px}
section{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:18px 20px;margin:14px 0}
h2{font-size:16px;margin:0 0 12px}h2 small{color:var(--dim);font-weight:normal;margin-left:6px}
table{width:100%;border-collapse:collapse;font-size:14px}td,th{padding:6px 8px;border-top:1px solid var(--line);text-align:left;vertical-align:top}
th{color:var(--dim);font-weight:600;border-top:none;font-size:12px;text-transform:uppercase;letter-spacing:.04em}
td.n{text-align:right;font-variant-numeric:tabular-nums;white-space:nowrap}
.bar{height:10px;border-radius:5px;background:var(--accent);min-width:2px}.track{background:var(--line);border-radius:5px;width:100%}
.floor{background:var(--floor)}.mcp{background:var(--mcp)}.img{background:var(--img)}.conv{background:var(--dim)}
.pill{display:inline-block;padding:1px 8px;border-radius:999px;font-size:12px;border:1px solid var(--line);color:var(--dim)}
.bad{color:var(--bad)}.warn{color:var(--warn)}.ok{color:var(--ok)}.dim{color:var(--dim)}
code{font:13px ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;word-break:break-all}
ul.recs{list-style:none;padding:0;margin:0}ul.recs li{padding:8px 0;border-top:1px solid var(--line)}ul.recs li:first-child{border-top:none}
footer{color:var(--dim);font-size:12px;text-align:center;margin-top:24px}
@media (max-width:600px){td.hide,th.hide{display:none}}
"#;

fn stat(value: &str, label: &str) -> String {
    format!(
        "<div class=stat><b>{}</b><span>{}</span></div>",
        escape(value),
        escape(label)
    )
}

pub fn render(scan: &ScanReport, usage: &UsageReport, generated: &str) -> String {
    let mut h = String::new();
    h.push_str("<!doctype html><html lang=en><head><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\">");
    h.push_str("<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\">");
    h.push_str("<title>Toolport report card</title><style>");
    h.push_str(CSS);
    h.push_str("</style></head><body><main>");
    h.push_str(&format!(
        "<header><h1>Toolport report card</h1><p>{} · {} agents · last {} days of usage</p></header>",
        escape(generated),
        scan.agents.len(),
        usage.days
    ));

    let t = &usage.totals;
    h.push_str("<div class=grid>");
    h.push_str(&stat(&format::thousands(t.sessions as u64), "sessions"));
    h.push_str(&stat(
        &format::compact(t.mean_context),
        "mean context per turn",
    ));
    h.push_str(&stat(
        &format::percent(t.cache_read_share),
        "input served from cache",
    ));
    h.push_str(&stat(
        &t.list_price_usd
            .map(format::money)
            .unwrap_or_else(|| "n/a".into()),
        "list-price equivalent",
    ));
    h.push_str(&stat(
        &scan.secrets.len().to_string(),
        "plaintext secrets on disk",
    ));
    h.push_str(&stat(
        &(scan.counts.errors + scan.counts.warnings).to_string(),
        "problems and warnings",
    ));
    h.push_str("</div>");

    // Resident context.
    if !usage.resident.is_empty() {
        h.push_str("<section><h2>What stays in context<small>share of every turn's input, by source</small></h2><table><tr><th>Source</th><th>Share</th><th class=n>%</th></tr>");
        let max = usage
            .resident
            .iter()
            .map(|s| s.share)
            .fold(0.0, f64::max)
            .max(0.0001);
        for s in usage.resident.iter().take(14) {
            let class = match s.source.as_str() {
                "floor" => "bar floor",
                "conversation" => "bar conv",
                "image" => "bar img",
                src if src.starts_with("mcp:") => "bar mcp",
                _ => "bar",
            };
            h.push_str(&format!(
                "<tr><td>{}</td><td style=\"width:55%\"><div class=track><div class=\"{class}\" style=\"width:{:.1}%\"></div></div></td><td class=n>{}</td></tr>",
                escape(&s.label),
                (s.share / max * 100.0).clamp(0.0, 100.0),
                escape(&format::percent(s.share))
            ));
        }
        h.push_str("</table></section>");
    }

    // Shell output breakdown.
    if !usage.shell.is_empty() {
        let bash_share = usage
            .resident
            .iter()
            .find(|s| s.source == "bash")
            .map(|s| s.share)
            .unwrap_or(0.0);
        h.push_str(&format!(
            "<section><h2>Shell output<small>where the {} bash residency came from</small></h2><table><tr><th>Category</th><th>Share</th><th class=n>%</th><th class=n>Added</th><th class=n>Calls</th><th class=hide>Top commands</th></tr>",
            escape(&format::percent(bash_share))
        ));
        let smax = usage
            .shell
            .iter()
            .map(|s| s.shell_share)
            .fold(0.0, f64::max)
            .max(0.0001);
        for s in usage
            .shell
            .iter()
            .filter(|s| s.tokens > 0 || s.produced > 0)
        {
            let class = match s.category.as_str() {
                "git" => "bar mcp",
                "test" | "build" => "bar floor",
                "other" => "bar conv",
                _ => "bar",
            };
            let cmds = s
                .top_commands
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            h.push_str(&format!(
                "<tr><td>{}</td><td style=\"width:38%\"><div class=track><div class=\"{class}\" style=\"width:{:.1}%\"></div></div></td><td class=n>{}</td><td class=n>{}</td><td class=n>{}</td><td class=hide><code>{}</code></td></tr>",
                escape(&s.label),
                (s.shell_share / smax * 100.0).clamp(0.0, 100.0),
                escape(&format::percent(s.shell_share)),
                escape(&format::compact(s.produced)),
                escape(&format::thousands(s.calls)),
                escape(&cmds)
            ));
        }
        h.push_str("</table></section>");
    }

    // Recommendations.
    if !usage.recommendations.is_empty() {
        h.push_str("<section><h2>Recommendations</h2><ul class=recs>");
        for r in &usage.recommendations {
            h.push_str(&format!(
                "<li><b>{}</b><br><span class=dim>{}</span></li>",
                escape(&r.title),
                escape(&r.detail)
            ));
        }
        h.push_str("</ul></section>");
    }

    // Agents.
    h.push_str("<section><h2>Agents<small>");
    h.push_str(&escape(&format!(
        "{} found, {} connected to Toolport",
        scan.agents.len(),
        scan.agents
            .iter()
            .filter(|a| a.gateway == "connected")
            .count()
    )));
    h.push_str("</small></h2><table><tr><th>Agent</th><th class=n>Own servers</th><th>Gateway</th><th class=hide>Config</th></tr>");
    for a in &scan.agents {
        let status = match (&a.error, a.gateway) {
            (Some(_), _) => "<span class=warn>config unreadable</span>",
            (None, "connected") => "<span class=ok>connected</span>",
            (None, "customized") => "<span class=dim>edited entry</span>",
            (None, _) if a.servers == 0 => "<span class=dim>no MCP</span>",
            _ => "<span class=dim>direct</span>",
        };
        h.push_str(&format!(
            "<tr><td>{}</td><td class=n>{}</td><td>{status}</td><td class=hide><code>{}</code></td></tr>",
            escape(&a.name),
            a.servers,
            escape(&a.config_path)
        ));
    }
    h.push_str("</table></section>");

    // Secrets.
    h.push_str("<section><h2>Plaintext secrets on disk</h2>");
    if scan.secrets.is_empty() {
        h.push_str("<p class=ok>None. No credentials sit in plain text in agent configs.</p>");
    } else {
        h.push_str(
            "<table><tr><th>Server</th><th>Setting</th><th>Agent</th><th class=hide>File</th></tr>",
        );
        for s in &scan.secrets {
            h.push_str(&format!(
                "<tr><td>{}</td><td class=bad>{}</td><td>{}</td><td class=hide><code>{}</code></td></tr>",
                escape(&s.server),
                escape(&s.setting),
                escape(&s.client_name),
                escape(&s.config_path)
            ));
        }
        h.push_str("</table><p class=dim>Values are never shown. <code>toolport fix --only secrets</code> moves them into the OS keychain.</p>");
    }
    h.push_str("</section>");

    // Drift and skills.
    if !scan.drift.is_empty() {
        h.push_str("<section><h2>MCP drift</h2><table><tr><th>Server</th><th>Kind</th><th>Agents</th></tr>");
        for d in &scan.drift {
            h.push_str(&format!(
                "<tr><td>{}</td><td><span class=\"pill {}\">{}</span></td><td>{}</td></tr>",
                escape(&d.server),
                if d.kind == "conflict" { "warn" } else { "" },
                escape(d.kind),
                escape(&d.clients.join(", "))
            ));
        }
        h.push_str("</table></section>");
    }
    h.push_str("<section><h2>Skills and rules<small>");
    h.push_str(&escape(&format!(
        "{} skills, {} rules files{}",
        scan.skills.total,
        scan.skills.rules_files,
        if scan.skills.shared_folder.is_some() {
            ", works with skills.sh"
        } else {
            ""
        }
    )));
    h.push_str("</small></h2>");
    if scan.skills.issues.is_empty() {
        h.push_str("<p class=ok>No skills or rules problems.</p>");
    } else {
        h.push_str("<ul class=recs>");
        for i in &scan.skills.issues {
            h.push_str(&format!(
                "<li><b class=\"{}\">{}</b>{}<br><span class=dim>{}</span></li>",
                match i.level.as_str() {
                    "error" => "bad",
                    "warn" => "warn",
                    _ => "",
                },
                escape(&i.title),
                if i.fixable {
                    " <span class=pill>fixable</span>"
                } else {
                    ""
                },
                escape(&i.detail)
            ));
        }
        h.push_str("</ul>");
    }
    h.push_str("</section>");

    // Startup tax.
    if !scan.startup.is_empty() {
        h.push_str("<section><h2>Turn-0 context<small>rules and skill listings loaded before the first prompt</small></h2><table><tr><th>Agent</th><th class=n>Tokens</th><th class=n>Rules</th><th class=n>Skills</th></tr>");
        for b in &scan.startup {
            h.push_str(&format!(
                "<tr><td>{}</td><td class=n>{}</td><td class=n>{}</td><td class=n>{} ({})</td></tr>",
                escape(&b.agent_name),
                format::thousands(b.total_tokens),
                format::thousands(b.rules_tokens),
                format::thousands(b.skills_tokens),
                b.skills_loaded
            ));
        }
        h.push_str("</table></section>");
    }

    // Models.
    if !usage.models.is_empty() {
        h.push_str("<section><h2>Models<small>list-price equivalent, not a subscription bill</small></h2><table><tr><th>Model</th><th class=n>Turns</th><th class=n>Cache read</th><th class=n>Output</th><th class=n>List price</th></tr>");
        for m in usage.models.iter().take(10) {
            h.push_str(&format!(
                "<tr><td><code>{}</code></td><td class=n>{}</td><td class=n>{}</td><td class=n>{}</td><td class=n>{}</td></tr>",
                escape(&m.model),
                format::compact(m.tokens.turns),
                format::compact(m.tokens.cache_read),
                format::compact(m.tokens.output),
                m.list_price_usd.map(format::money).unwrap_or_else(|| "n/a".into())
            ));
        }
        h.push_str("</table></section>");
    }

    h.push_str(&format!(
        "<footer>Generated by toolport {} from local files only. Token figures are estimates.</footer>",
        escape(super::VERSION)
    ));
    h.push_str("</main></body></html>\n");
    h
}

fn today() -> String {
    let (y, m, d) = crate::usage_report::civil_from_days((super::now_ms() / 86_400_000) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn command(ctx: &Ctx, args: &Args) -> Result<i32, CliError> {
    let days = args.number("days", 30, 3650)?;
    let project = ctx.project_str();
    let snapshot = Snapshot::read(project.as_deref());
    let scan = super::scan::collect(&snapshot, ctx.project.as_deref(), false);
    let usage = super::usage::build(ctx, days, super::usage::parse_clients(None)?, true);
    let html = render(&scan, &usage, &today());
    let out: PathBuf = args
        .value("out")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("toolport-report.html"));
    std::fs::write(&out, html)
        .map_err(|e| CliError::new(format!("couldn't write {}: {e}", out.display())))?;
    let shown = std::fs::canonicalize(&out).unwrap_or(out.clone());
    if ctx.json {
        ctx.emit_json(&serde_json::json!({"path": shown}))?;
    } else {
        super::out(&format!(
            "{} Wrote {}\n  {}",
            ctx.style.mark(format::Severity::Ok),
            ctx.style.bold(&format::home_relative(Path::new(&shown))),
            ctx.style
                .dim("Static HTML with no scripts or external requests. Open it in any browser.")
        ));
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_is_escaped_static_and_dash_free() {
        let mut scan = super::super::scan::ScanReport {
            version: "x",
            project: None,
            agents: Vec::new(),
            secrets: vec![super::super::scan::SecretFinding {
                client_id: "c".into(),
                client_name: "<script>alert(1)</script>".into(),
                config_path: "~/x".into(),
                server: "s\"".into(),
                kind: "env",
                setting: "TOKEN".into(),
                in_toolport: false,
            }],
            drift: Vec::new(),
            skills: Default::default(),
            startup: Vec::new(),
            counts: Default::default(),
            next_steps: Vec::new(),
        };
        scan.counts.errors = 1;
        let mut usage = UsageReport::default();
        usage.shell.push(super::super::usage::ShellRow {
            category: "read".into(),
            label: "File reads & search".into(),
            calls: 12,
            tokens: 4_000,
            share: 0.2,
            shell_share: 1.0,
            produced: 1_200,
            top_commands: vec!["sed".into(), "rg".into()],
        });
        let html = render(&scan, &usage, "2026-11-24");
        assert!(html.contains("Shell output"));
        assert!(html.contains("File reads &amp; search"));
        assert!(html.contains("<code>sed, rg</code>"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("http://") && !html.contains("https://"));
        assert!(!html.contains("<script") && !html.contains("<link") && !html.contains("<img"));
        assert!(!html.contains('\u{2014}') && !html.contains('\u{2013}'));
    }
}
