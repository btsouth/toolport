//! `toolport export` and `toolport import`: a secret-free setup file you can commit or share
//! as a gist. Both sides are `sharing_controller`, the code behind the apps' Export and
//! Import: export strips every credential, and import adds servers switched off, after
//! showing what each one runs or connects to.

use std::io::Read as _;
use std::path::Path;

use serde::Serialize;

use super::format::{self, Severity};
use super::{Args, CliError, Ctx, EXIT_OK};

pub fn export_command(ctx: &Ctx, args: &Args) -> Result<i32, CliError> {
    let json = crate::sharing_controller::export_json(
        args.value("name"),
        args.value("description"),
        None,
    )?;
    let count = serde_json::from_str::<serde_json::Value>(&json)
        .ok()
        .and_then(|v| v["servers"].as_array().map(|a| a.len()))
        .unwrap_or(0);
    match args.value("out").unwrap_or("-") {
        "-" => super::out(&json),
        path => {
            let path = Path::new(path);
            crate::sharing_controller::write_setup_file(path, &format!("{json}\n"))?;
            if ctx.json {
                ctx.emit_json(&serde_json::json!({"path": path, "servers": count}))?;
            } else {
                super::out(&format!(
                    "{} Exported {} to {}\n  {}",
                    ctx.style.mark(Severity::Ok),
                    format::plural(count, "server", "servers"),
                    ctx.style.bold(&format::home_relative(path)),
                    ctx.style.dim(
                        "Credentials are stripped. Others bring it in with: toolport import <file>"
                    )
                ));
            }
        }
    }
    Ok(EXIT_OK)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportRow {
    name: String,
    transport: String,
    target: String,
    is_new: bool,
    selected: bool,
    warnings: Vec<&'static str>,
    references: Vec<String>,
}

fn read_input(source: &str) -> Result<String, CliError> {
    if source == "-" {
        let mut text = String::new();
        std::io::stdin()
            .take(4 * 1024 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(|e| CliError::new(format!("couldn't read stdin: {e}")))?;
        if text.len() > 4 * 1024 * 1024 {
            return Err(CliError::new(
                "that input is too large to be a Toolport setup",
            ));
        }
        return Ok(text);
    }
    if let Some(id) = crate::sharing_controller::parse_share_url(source) {
        return crate::sharing_controller::fetch_shared_setup(&id).map_err(CliError::new);
    }
    crate::sharing_controller::read_setup_file(Path::new(source)).map_err(CliError::new)
}

pub fn import_command(ctx: &Ctx, args: &Args) -> Result<i32, CliError> {
    let source = &args.positionals[0];
    let json = read_input(source)?;
    let items = crate::sharing_controller::preview_import(&json)?;
    let only = args.list("only");
    for name in &only {
        if !items.iter().any(|i| i.name.eq_ignore_ascii_case(name)) {
            return Err(CliError::new(format!(
                "the setup has no server named '{name}'"
            )));
        }
    }
    let rows: Vec<ImportRow> = items
        .iter()
        .map(|i| ImportRow {
            name: i.name.clone(),
            transport: i.transport.clone(),
            target: match (&i.command, &i.url) {
                (Some(c), _) => {
                    let mut parts = vec![c.clone()];
                    parts.extend(crate::import_credentials::shown_args(&i.args));
                    parts.join(" ")
                }
                (None, Some(u)) => crate::sharing_controller::redact_share_url(u),
                _ => String::new(),
            },
            is_new: i.is_new,
            selected: i.is_new
                && (only.is_empty() || only.iter().any(|n| n.eq_ignore_ascii_case(&i.name))),
            warnings: crate::sharing_controller::import_item_warnings(i),
            references: i.reference_review.clone(),
        })
        .collect();
    let selected: Vec<String> = rows
        .iter()
        .filter(|r| r.selected)
        .map(|r| r.name.clone())
        .collect();
    let s = &ctx.style;
    if !ctx.json {
        let mut text = format!(
            "{} {}\n\n",
            s.bold("toolport import"),
            s.dim(&format!(
                "· {}",
                if source == "-" { "stdin" } else { source }
            ))
        );
        for r in &rows {
            let mark = if !r.is_new {
                s.dim("=")
            } else if r.selected {
                s.cyan("+")
            } else {
                s.dim("-")
            };
            text.push_str(&format!(
                "  {mark} {}  {}{}\n",
                s.bold(&r.name),
                s.dim(&format::truncate(&r.target, 80)),
                if r.is_new {
                    String::new()
                } else {
                    s.dim("  (already in Toolport)")
                }
            ));
            for w in &r.warnings {
                text.push_str(&format!("      {} {w}\n", s.yellow("!")));
            }
            for line in &r.references {
                text.push_str(&format!("      {}\n", s.dim(line)));
            }
        }
        text.push_str(&format!(
            "\n  {}\n",
            s.dim("Imported servers start switched off. Turn each one on once you trust it.")
        ));
        super::out(&text);
    }
    if selected.is_empty() || args.has("dry-run") {
        if ctx.json {
            ctx.emit_json(
                &serde_json::json!({"dryRun": args.has("dry-run"), "servers": rows, "imported": 0}),
            )?;
        } else if selected.is_empty() {
            super::out(&format!(
                "  {} Nothing new to import.",
                s.mark(Severity::Ok)
            ));
        } else {
            super::out(&format!(
                "  {}",
                s.hint(&format!(
                    "run {} to import",
                    s.cmd("toolport import <file> --yes")
                ))
            ));
        }
        return Ok(EXIT_OK);
    }
    if !args.has("yes") {
        if !ctx.interactive || source == "-" {
            return Err(CliError::new("not importing without confirmation")
                .hint("run with --yes to import, or --dry-run to preview"));
        }
        if !ctx.confirm(&format!(
            "Import {}?",
            format::plural(selected.len(), "server", "servers")
        )) {
            super::out("Nothing changed.");
            return Ok(EXIT_OK);
        }
    }
    let (_, added) = crate::sharing_controller::import_json_selected(&json, &selected)?;
    if ctx.json {
        ctx.emit_json(&serde_json::json!({"dryRun": false, "servers": rows, "imported": added}))?;
    } else {
        super::out(&format!(
            "  {} Imported {} (switched off)",
            s.mark(Severity::Ok),
            format::plural(added, "server", "servers")
        ));
    }
    Ok(EXIT_OK)
}
