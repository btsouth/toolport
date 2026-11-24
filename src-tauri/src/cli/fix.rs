//! `toolport fix`: preview, then apply, the repairs Toolport already knows are safe.
//!
//! Skills and rules repairs are `skills::fix_issue`, the checked fixes behind the Skills
//! page: each one is re-planned from disk and refused if the problem changed since it was
//! shown. Secrets go through the reviewed import (`registry_controller`), which vaults
//! values in the OS keychain before anything else is written; with `--connect` the agent's
//! config is rewired to the gateway by the same reviewed migrate the Clients page runs, so
//! the plaintext copy leaves the file (a backup is kept in Toolport's private backups).

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::format::{self, Severity, Style};
use super::scan::{SecretFinding, Snapshot};
use super::{Args, CliError, Ctx, EXIT_OK};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Only {
    All,
    Skills,
    Rules,
    Secrets,
}

fn parse_only(value: Option<&str>) -> Result<Only, CliError> {
    match value.unwrap_or("all") {
        "all" => Ok(Only::All),
        "skills" => Ok(Only::Skills),
        "rules" => Ok(Only::Rules),
        "secrets" => Ok(Only::Secrets),
        other => Err(CliError::new(format!("unknown fix kind '{other}'"))
            .hint("use skills, rules, secrets or all")),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedFix {
    pub kind: &'static str,
    pub id: String,
    pub title: String,
    pub action: String,
    pub detail: String,
    /// Files or links the fix creates, changes or removes.
    pub paths: Vec<String>,
    #[serde(skip)]
    fingerprint: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretPlan {
    pub client_id: String,
    pub client_name: String,
    pub config_path: String,
    pub servers: Vec<String>,
    pub settings: usize,
    /// Reviewed import keys for servers Toolport doesn't have yet.
    #[serde(skip)]
    keys: Vec<String>,
    /// Servers already in Toolport: vaulted earlier, only the agent still has a copy.
    pub already_in_toolport: Vec<String>,
    pub unsupported: Vec<String>,
    /// Whether scan found each of `servers` in Toolport already.
    #[serde(skip)]
    held: Vec<bool>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FixReport {
    pub dry_run: bool,
    pub planned: Vec<PlannedFix>,
    pub secrets: Vec<SecretPlan>,
    pub gateway_available: bool,
    pub applied: Vec<String>,
    pub failed: Vec<String>,
    pub notes: Vec<String>,
}

fn plan_skills(snapshot: &Snapshot, only: Only) -> Vec<PlannedFix> {
    let Some(inv) = &snapshot.inventory else {
        return Vec::new();
    };
    inv.issues
        .iter()
        .filter(|i| i.fix_label.is_some() && !i.dismissed)
        .filter(|i| {
            let rules = i.id.starts_with("rules-");
            match only {
                Only::All => true,
                Only::Rules => rules,
                Only::Skills => !rules,
                Only::Secrets => false,
            }
        })
        .map(|i| {
            let mut paths = i.fix_paths.clone();
            if i.id == "broken-links" {
                paths.extend(i.skills.iter().map(|k| format::home_relative(k.as_ref())));
            }
            let agents: Vec<String> = i
                .agents
                .iter()
                .map(|a| super::scan::agent_name(inv, a))
                .collect();
            let mut detail = i.fix_note.clone().unwrap_or_else(|| i.detail.clone());
            if !i.skill_names.is_empty() {
                detail = format!("{detail} Skills: {}.", i.skill_names.join(", "));
            }
            if !agents.is_empty() {
                detail = format!("{detail} Agents: {}.", agents.join(", "));
            }
            PlannedFix {
                kind: if i.id.starts_with("rules-") {
                    "rules"
                } else {
                    "skills"
                },
                id: i.id.clone(),
                title: i.title.clone(),
                action: i.fix_label.clone().unwrap_or_default(),
                detail,
                paths,
                fingerprint: i.fingerprint.clone(),
            }
        })
        .collect()
}

/// Reviewed import keys for `servers` in one client, read fresh from its config and
/// Toolport's registry: servers Toolport already has, and ones it can't import, apart.
/// A review may read the keychain to confirm identity, but never asks to unlock it.
/// Import keys for `servers` (name, whether Toolport already holds it), plus the ones
/// Toolport already holds and the ones it can't import here. A server the review can't
/// see is only "already in Toolport" when Toolport holds it; a plugin's server or a
/// failed preview is reported as not importable instead.
fn review(client_id: &str, servers: &[(String, bool)]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let (mut keys, mut already, mut unsupported) = (Vec::new(), Vec::new(), Vec::new());
    let candidates = match crate::registry_controller::preview_client_imports_for(client_id) {
        Ok(c) => c,
        Err(e) => {
            for (server, _) in servers {
                unsupported.push(format!("{server}: couldn't review its config ({e})"));
            }
            return (keys, already, unsupported);
        }
    };
    for (server, held) in servers {
        let found = candidates.iter().find(|c| {
            c.source_name.as_deref().unwrap_or(&c.name) == server.as_str() || c.name == *server
        });
        match found {
            Some(c) if c.unsupported.is_none() => keys.push(c.key.clone()),
            Some(c) => unsupported.push(format!(
                "{server}: {}",
                c.unsupported.clone().unwrap_or_default()
            )),
            None if *held => already.push(server.clone()),
            None => unsupported.push(format!(
                "{server}: Toolport can't import this one from here; add it in the app"
            )),
        }
    }
    (keys, already, unsupported)
}

fn plan_secrets(findings: &[SecretFinding]) -> Vec<SecretPlan> {
    let mut by_client: BTreeMap<String, Vec<&SecretFinding>> = BTreeMap::new();
    for f in findings {
        by_client.entry(f.client_id.clone()).or_default().push(f);
    }
    let mut plans = Vec::new();
    for (client_id, items) in by_client {
        let mut servers: Vec<String> = items.iter().map(|f| f.server.clone()).collect();
        servers.dedup();
        let held: Vec<bool> = servers
            .iter()
            .map(|n| items.iter().any(|f| &f.server == n && f.in_toolport))
            .collect();
        let mut plan = SecretPlan {
            client_name: items[0].client_name.clone(),
            config_path: items[0].config_path.clone(),
            settings: items.len(),
            servers: servers.clone(),
            keys: Vec::new(),
            already_in_toolport: Vec::new(),
            unsupported: Vec::new(),
            client_id: client_id.clone(),
            held: held.clone(),
        };
        let pairs: Vec<(String, bool)> = servers.iter().cloned().zip(held).collect();
        let (keys, already, unsupported) = review(&client_id, &pairs);
        plan.keys = keys;
        plan.already_in_toolport = already;
        plan.unsupported = unsupported;
        plans.push(plan);
    }
    plans
}

pub fn plan(snapshot: &Snapshot, only: Only, dry_run: bool) -> FixReport {
    let planned = plan_skills(snapshot, only);
    let secrets = if matches!(only, Only::All | Only::Secrets) {
        plan_secrets(&super::scan::secret_findings(snapshot))
    } else {
        Vec::new()
    };
    FixReport {
        dry_run,
        planned,
        secrets,
        gateway_available: crate::clients::resolve_gateway_path_readonly().is_some(),
        ..Default::default()
    }
}

fn render_plan(r: &FixReport, style: &Style, connect: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} {}\n",
        style.bold("toolport fix"),
        style.dim(if r.dry_run {
            "· dry run, nothing will change"
        } else {
            "· plan"
        })
    ));
    if r.planned.is_empty() && r.secrets.is_empty() {
        out.push_str(&format!(
            "\n  {} Nothing to fix.\n",
            style.mark(Severity::Ok)
        ));
        return out;
    }
    if !r.planned.is_empty() {
        out.push_str(&style.heading("Skills and rules"));
        for p in &r.planned {
            out.push_str(&format!(
                "  {} {}  {}\n",
                style.cyan(if style.unicode { "◆" } else { "*" }),
                style.bold(&p.action),
                style.dim(&p.title)
            ));
            out.push_str(&format!(
                "      {}\n",
                style.dim(&format::truncate(&p.detail, 110))
            ));
            for path in &p.paths {
                out.push_str(&format!("      {} {}\n", style.dim("·"), path));
            }
        }
    }
    if !r.secrets.is_empty() {
        out.push_str(&style.heading("Plaintext secrets"));
        for s in &r.secrets {
            out.push_str(&format!(
                "  {} {}  {}\n",
                style.cyan(if style.unicode { "◆" } else { "*" }),
                style.bold(&s.client_name),
                style.dim(&s.config_path)
            ));
            if !s.keys.is_empty() {
                let names: Vec<&String> = s
                    .servers
                    .iter()
                    .filter(|n| {
                        !s.already_in_toolport.contains(n)
                            && !s
                                .unsupported
                                .iter()
                                .any(|u| u.starts_with(&format!("{n}:")))
                    })
                    .collect();
                out.push_str(&format!(
                    "      move {} into Toolport, credentials into your OS keychain\n",
                    names
                        .iter()
                        .map(|n| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            for n in &s.already_in_toolport {
                out.push_str(&format!(
                    "      {} is already in Toolport; only this agent's copy is left\n",
                    n
                ));
            }
            for u in &s.unsupported {
                out.push_str(&format!(
                    "      {} {}\n",
                    style.yellow("skip"),
                    style.dim(u)
                ));
            }
            if connect && r.gateway_available {
                out.push_str(&format!(
                    "      then connect {} to the Toolport gateway and remove the plaintext copies (backup kept)\n",
                    s.client_name
                ));
            }
        }
        if !connect {
            out.push_str(&format!(
                "  {}\n",
                style.dim("Agent configs are left as they are, so the plaintext copies stay until the agent is connected.")
            ));
            out.push_str(&format!(
                "  {}\n",
                style.hint(&if r.gateway_available {
                    format!("add {} to also rewire the agents and remove the copies", style.cmd("--connect"))
                } else {
                    "install the Toolport app or toolport-gateway, then add --connect to rewire the agents".into()
                })
            ));
        } else if !r.gateway_available {
            out.push_str(&format!(
                "  {} {}\n",
                style.mark(Severity::Warn),
                "No Toolport gateway is installed, so agents can't be connected yet. Install the Toolport app or toolport-gateway first."
            ));
        }
    }
    out
}

pub fn command(ctx: &Ctx, args: &Args) -> Result<i32, CliError> {
    let only = parse_only(args.value("only"))?;
    let dry_run = args.has("dry-run");
    let connect = args.has("connect");
    let project = ctx.project_str();
    let snapshot = Snapshot::read(project.as_deref());
    if let Some(e) = &snapshot.inventory_error {
        return Err(CliError::new(format!("couldn't read skills: {e}")));
    }
    let mut report = plan(&snapshot, only, dry_run);
    let nothing = report.planned.is_empty() && report.secrets.is_empty();
    if dry_run || nothing {
        if ctx.json {
            ctx.emit_json(&report)?;
        } else {
            let mut text = render_plan(&report, &ctx.style, connect);
            if dry_run && !nothing {
                text.push_str(&format!(
                    "\n  {}\n",
                    ctx.style.hint(&format!(
                        "run {} to apply",
                        ctx.style.cmd(&format!(
                            "toolport fix{}{} --yes",
                            match only {
                                Only::All => "",
                                Only::Skills => " --only skills",
                                Only::Rules => " --only rules",
                                Only::Secrets => " --only secrets",
                            },
                            if connect { " --connect" } else { "" }
                        ))
                    ))
                ));
            }
            super::out(&text);
        }
        return Ok(EXIT_OK);
    }
    if !ctx.json {
        super::out(&render_plan(&report, &ctx.style, connect));
    }
    if !args.has("yes") {
        if !ctx.interactive {
            return Err(CliError::new("not applying without confirmation")
                .hint("run with --yes to apply, or --dry-run to preview"));
        }
        if !ctx.confirm("\nApply these fixes?") {
            super::out("Nothing changed.");
            return Ok(EXIT_OK);
        }
    }
    apply(&mut report, project.as_deref(), connect);
    if ctx.json {
        ctx.emit_json(&report)?;
    } else {
        let s = &ctx.style;
        let mut text = String::from("\n");
        for a in &report.applied {
            text.push_str(&format!("  {} {}\n", s.mark(Severity::Ok), a));
        }
        for f in &report.failed {
            text.push_str(&format!("  {} {}\n", s.mark(Severity::Error), f));
        }
        for n in &report.notes {
            text.push_str(&format!("  {} {}\n", s.mark(Severity::Info), s.dim(n)));
        }
        text.push_str(&format!(
            "\n  {}\n",
            s.hint("run 'toolport scan' to check the result")
        ));
        super::out(&text);
    }
    Ok(if report.failed.is_empty() {
        EXIT_OK
    } else {
        super::EXIT_ERROR
    })
}

fn apply(report: &mut FixReport, project: Option<&str>, connect: bool) {
    apply_skills(
        report,
        |id| {
            crate::skills::inventory(project).map(|inv| {
                inv.issues
                    .into_iter()
                    .find(|i| i.id == id)
                    .map(|i| i.fingerprint)
            })
        },
        |id, fingerprint| crate::skills::fix_issue(id, fingerprint, project).map(|_| ()),
    );
    apply_secrets(report, connect);
}

/// Skills and rules repairs, each with the fingerprint the user saw in the preview. An
/// issue that changed since (on disk, or through an earlier repair in this run) is left
/// alone: `fix_issue` would refuse it anyway, and a fresh fingerprint would approve a
/// repair nobody reviewed. `current` reads an issue's fingerprint now (`None`: it's gone).
fn apply_skills(
    report: &mut FixReport,
    current: impl Fn(&str) -> Result<Option<String>, String>,
    fix: impl Fn(&str, &str) -> Result<(), String>,
) {
    // Snapshot which items already matched before any repair ran so we can tell an
    // external edit apart from a change caused by an earlier repair in this same run.
    let matched_at_start: std::collections::BTreeSet<String> = report
        .planned
        .iter()
        .filter(|p| matches!(current(&p.id), Ok(Some(ref fp)) if fp == &p.fingerprint))
        .map(|p| p.id.clone())
        .collect();
    let mut applied_any = false;
    for p in report.planned.clone() {
        match current(&p.id) {
            Ok(None) => report
                .notes
                .push(format!("{}: already gone, nothing to do.", p.title)),
            Ok(Some(now)) if now != p.fingerprint => {
                let reason = if applied_any && matched_at_start.contains(&p.id) {
                    "changed after an earlier repair in this run, so it was left alone. Run 'toolport fix --dry-run' to review the remaining items."
                } else {
                    "changed since the preview, so it was left alone. Run 'toolport fix --dry-run' to review it again."
                };
                report.failed.push(format!("{}: {reason}", p.title));
            }
            // An unreadable inventory falls through: `fix_issue` reports why.
            _ => match fix(&p.id, &p.fingerprint) {
                Ok(()) => {
                    applied_any = true;
                    report.applied.push(format!("{}: {}", p.action, p.title));
                }
                Err(e) => report.failed.push(format!("{}: {e}", p.title)),
            },
        }
    }
}

fn apply_secrets(report: &mut FixReport, connect: bool) {
    let mut imported: BTreeSet<String> = BTreeSet::new();
    for s in report.secrets.clone() {
        if connect && report.gateway_available {
            let names: Vec<String> = s
                .servers
                .iter()
                .filter(|n| {
                    !s.unsupported
                        .iter()
                        .any(|u| u.starts_with(&format!("{n}:")))
                })
                .cloned()
                .collect();
            let outcome =
                crate::registry_controller::preview_client_setup(&s.client_id).and_then(|review| {
                    crate::registry_controller::migrate_client_reviewed(
                        &s.client_id,
                        None,
                        false,
                        &names,
                        &review.revision,
                    )
                });
            match outcome {
                Ok(o) => report.applied.push(format!(
                    "{}: moved {} into Toolport and connected the gateway",
                    s.client_name,
                    if o.moved.is_empty() {
                        names.join(", ")
                    } else {
                        o.moved.join(", ")
                    }
                )),
                Err(e) => report.failed.push(format!("{}: {e}", s.client_name)),
            }
            continue;
        }
        // An earlier client's import may have added the same server: review again now.
        let pairs: Vec<(String, bool)> = s
            .servers
            .iter()
            .zip(&s.held)
            .map(|(n, held)| (n.clone(), *held || imported.contains(&n.to_lowercase())))
            .collect();
        let (keys, already, _) = review(&s.client_id, &pairs);
        if keys.is_empty() {
            if !already.is_empty() {
                report.notes.push(format!(
                    "{}: {} already in Toolport. Connect the agent to remove its plaintext copy.",
                    s.client_name,
                    already.join(", ")
                ));
            }
            continue;
        }
        match crate::registry_controller::import_client_servers(keys) {
            Ok((_, added)) => {
                imported.extend(
                    pairs
                        .iter()
                        .filter(|(n, _)| !already.contains(n))
                        .map(|(n, _)| n.to_lowercase()),
                );
                report.applied.push(format!(
                    "{}: {} into Toolport, credentials in your OS keychain",
                    s.client_name,
                    format::plural(added, "server moved", "servers moved")
                ));
                report.notes.push(format!(
                    "{} still has its own copy in {}. Connect it to the gateway to remove it: toolport fix --only secrets --connect",
                    s.client_name, s.config_path
                ));
            }
            Err(e) => report.failed.push(format!("{}: {e}", s.client_name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn planned(id: &str, fingerprint: &str) -> PlannedFix {
        PlannedFix {
            kind: "skills",
            id: id.into(),
            title: format!("{id} title"),
            action: format!("Fix {id}"),
            detail: String::new(),
            paths: Vec::new(),
            fingerprint: fingerprint.into(),
        }
    }

    #[test]
    fn a_repair_that_changed_since_the_preview_is_refused_not_re_approved() {
        // The machine as previewed: three issues, each with the fingerprint the user saw.
        let state = RefCell::new(BTreeMap::from([
            ("broken-links".to_string(), "broken-links:aaaa".to_string()),
            (
                "rules-missing".to_string(),
                "rules-missing:bbbb".to_string(),
            ),
            ("gone".to_string(), "gone:cccc".to_string()),
        ]));
        let mut report = FixReport {
            planned: vec![
                planned("broken-links", "broken-links:aaaa"),
                planned("rules-missing", "rules-missing:bbbb"),
                planned("gone", "gone:cccc"),
            ],
            ..Default::default()
        };
        // Between the preview and the confirmation, one more link broke and another
        // problem went away on its own.
        state
            .borrow_mut()
            .insert("broken-links".into(), "broken-links:dddd".into());
        state.borrow_mut().remove("gone");

        let calls = RefCell::new(Vec::new());
        apply_skills(
            &mut report,
            |id| Ok(state.borrow().get(id).cloned()),
            |id, fingerprint| {
                calls
                    .borrow_mut()
                    .push((id.to_string(), fingerprint.to_string()));
                // Same check as `skills::fix_issue`: only the fingerprint as it is now.
                if state.borrow().get(id).map(String::as_str) != Some(fingerprint) {
                    return Err("That problem changed since you looked.".into());
                }
                state.borrow_mut().remove(id);
                Ok(())
            },
        );

        assert_eq!(
            *calls.borrow(),
            vec![(
                "rules-missing".to_string(),
                "rules-missing:bbbb".to_string()
            )],
            "only the unchanged repair runs, with the fingerprint from the preview"
        );
        assert_eq!(
            report.applied,
            vec!["Fix rules-missing: rules-missing title"]
        );
        assert_eq!(report.failed.len(), 1, "{:?}", report.failed);
        assert!(report.failed[0].starts_with("broken-links title: changed since the preview"));
        assert!(report.failed[0].contains("toolport fix --dry-run"));
        assert!(report
            .notes
            .iter()
            .any(|n| n.contains("gone title: already gone")));
        // The changed issue was left exactly as it is now.
        assert_eq!(
            state.borrow().get("broken-links").map(String::as_str),
            Some("broken-links:dddd")
        );
    }

    #[test]
    fn an_unreadable_inventory_still_goes_through_the_checked_fix() {
        let mut report = FixReport {
            planned: vec![planned("broken-links", "broken-links:aaaa")],
            ..Default::default()
        };
        let calls = RefCell::new(Vec::new());
        apply_skills(
            &mut report,
            |_| Err("no home".into()),
            |id, fingerprint| {
                calls
                    .borrow_mut()
                    .push((id.to_string(), fingerprint.to_string()));
                Err("Couldn't find your home folder".into())
            },
        );
        assert_eq!(
            *calls.borrow(),
            vec![("broken-links".to_string(), "broken-links:aaaa".to_string())]
        );
        assert_eq!(
            report.failed,
            vec!["broken-links title: Couldn't find your home folder"]
        );
    }
}
