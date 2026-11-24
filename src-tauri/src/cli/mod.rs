//! The standalone `toolport` command: a cross-agent doctor, token profiler and project
//! bootstrapper built on the same discovery, skills, secrets and sharing code as the apps.
//!
//! Every command reads through Toolport's own modules (`clients`, `skills`,
//! `registry_controller`, `sharing_controller`, `secrets`), so the CLI never carries a second
//! copy of how an agent stores its config. The binary links none of the desktop or search
//! features.

pub mod fix;
pub mod format;
pub mod hook;
pub mod manifest;
pub mod portability;
pub mod report;
pub mod scan;
pub mod usage;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal as _, Write as _};
use std::path::PathBuf;

use format::Style;

/// Exit codes. `Findings` only with `--check`, so CI can fail on drift.
pub const EXIT_OK: i32 = 0;
pub const EXIT_ERROR: i32 = 1;
pub const EXIT_FINDINGS: i32 = 2;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The binary the CLI runs in: the standalone `toolport`, or the Linux desktop shell,
/// whose `/usr/bin/toolport` runs these commands too.
pub struct Host {
    /// `binary` in `toolport --version --json`. The standalone binary passes the marker
    /// the npm launcher looks for; it lives only in `src/bin/toolport.rs`, so the desktop
    /// shell, which opens its window when run without arguments, never contains it.
    pub binary: &'static str,
    /// A line printed after the version, such as the desktop build.
    pub version_detail: Option<String>,
    /// A section printed after the main help, such as how to open the desktop app.
    pub help_footer: Option<fn(&Style) -> String>,
}

/// Whether `args` (without the program name) are a CLI command line rather than a
/// desktop launch. Commands, their typos and the CLI's own options are; no arguments,
/// `--hidden`, `toolport://` and other links, and options the CLI doesn't know (GTK and
/// GApplication ones) are not.
pub fn is_cli_invocation(args: &[std::ffi::OsString]) -> bool {
    let Some(first) = args.first() else {
        return false;
    };
    let first = first.to_string_lossy();
    if first.contains("://") {
        return false;
    }
    if first == "-" || first == "--" || !first.starts_with('-') {
        return true;
    }
    if first == "--fix" {
        return true;
    }
    let known = |flag: &Flag| -> bool {
        match first.strip_prefix("--") {
            Some(long) => long.split_once('=').map_or(long, |(n, _)| n) == flag.long,
            None => first.len() == 2 && flag.short == first.chars().nth(1),
        }
    };
    let scan = find_command("scan").expect("scan exists");
    GLOBAL_FLAGS.iter().chain(scan.flags).any(known)
}

/// What every command gets: output mode, style and the project folder.
pub struct Ctx {
    pub json: bool,
    pub style: Style,
    /// The project folder: `--project`, else the current folder.
    pub project: Option<PathBuf>,
    /// Both stdin and stdout are terminals, so a question can be asked.
    pub interactive: bool,
}

impl Ctx {
    pub fn project_str(&self) -> Option<String> {
        self.project
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// Print a JSON document on stdout.
    pub fn emit_json(&self, value: &impl serde::Serialize) -> Result<(), CliError> {
        let text = serde_json::to_string_pretty(value).map_err(|e| CliError::new(e.to_string()))?;
        out(&text);
        Ok(())
    }

    /// Ask a yes/no question on the terminal. Default is no.
    pub fn confirm(&self, question: &str) -> bool {
        if !self.interactive {
            return false;
        }
        eprint!("{} {} ", question, self.style.dim("[y/N]"));
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }
}

/// A failure with an optional next step.
#[derive(Debug)]
pub struct CliError {
    pub message: String,
    pub hint: Option<String>,
    pub code: i32,
}

impl CliError {
    pub fn new(message: impl Into<String>) -> Self {
        CliError {
            message: message.into(),
            hint: None,
            code: EXIT_ERROR,
        }
    }
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl From<String> for CliError {
    fn from(message: String) -> Self {
        CliError::new(message)
    }
}

/// Write to stdout, ignoring a closed pipe (`toolport usage | head`).
pub fn out(text: &str) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_all(text.as_bytes());
    if !text.ends_with('\n') {
        let _ = lock.write_all(b"\n");
    }
    let _ = lock.flush();
}

// ---------------------------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------------------------

/// One option a command accepts.
#[derive(Clone, Copy)]
pub struct Flag {
    pub long: &'static str,
    pub short: Option<char>,
    pub value: Option<&'static str>,
    pub help: &'static str,
}

const fn switch(long: &'static str, help: &'static str) -> Flag {
    Flag {
        long,
        short: None,
        value: None,
        help,
    }
}

const fn valued(long: &'static str, value: &'static str, help: &'static str) -> Flag {
    Flag {
        long,
        short: None,
        value: Some(value),
        help,
    }
}

const GLOBAL_FLAGS: &[Flag] = &[
    switch("json", "Print machine-readable JSON"),
    switch("no-color", "Disable color (also NO_COLOR=1)"),
    valued(
        "project",
        "DIR",
        "Project folder to check (default: current folder)",
    ),
    Flag {
        long: "help",
        short: Some('h'),
        value: None,
        help: "Print help",
    },
    Flag {
        long: "version",
        short: Some('V'),
        value: None,
        help: "Print version",
    },
];

/// A subcommand: its name, summary, usage line, options and examples.
pub struct Command {
    pub name: &'static str,
    pub summary: &'static str,
    pub usage: &'static str,
    pub flags: &'static [Flag],
    pub positionals: (usize, usize),
    pub examples: &'static [&'static str],
    pub about: &'static str,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "scan",
        summary: "Check every coding agent on this machine (the default)",
        usage: "toolport scan [--check] [--all]",
        flags: &[
            switch("check", "Exit 2 when anything needs attention (for CI)"),
            switch("all", "Show every finding, including informational ones"),
        ],
        positionals: (0, 0),
        examples: &["toolport", "toolport scan --check", "toolport scan --json"],
        about: "Finds every agent and profile, plaintext secrets in their configs, MCP servers that drift between agents, skills and rules problems, and what each agent loads before your first prompt. Secret values are never printed.",
    },
    Command {
        name: "usage",
        summary: "See where your tokens go across Claude Code, Codex and OpenCode",
        usage: "toolport usage [--days N] [--client claude|codex|opencode|all]",
        flags: &[
            valued("days", "N", "Look back N days (default 30)"),
            valued("client", "NAME", "claude, codex, opencode or all (default all)"),
            switch("no-cache", "Read every transcript again instead of the summary cache"),
        ],
        positionals: (0, 0),
        examples: &["toolport usage", "toolport usage --days 7 --client claude", "toolport usage --json"],
        about: "Reads local agent transcripts and Toolport's activity log, entirely offline. Shows context per turn, cache health, what stays resident in context (startup floor, Bash, Read, screenshots, each MCP server) and list-price equivalent cost.",
    },
    Command {
        name: "fix",
        summary: "Preview and apply safe fixes for skills, rules and secrets",
        usage: "toolport fix [--dry-run] [--yes] [--only skills|rules|secrets|all]",
        flags: &[
            switch("dry-run", "Show every change without touching disk"),
            Flag { long: "yes", short: Some('y'), value: None, help: "Apply without asking" },
            valued("only", "KIND", "skills, rules, secrets or all (default all)"),
            switch("connect", "With secrets: also point those agents at the Toolport gateway"),
        ],
        positionals: (0, 0),
        examples: &["toolport fix --dry-run", "toolport fix --only skills --yes", "toolport fix --only secrets"],
        about: "Skills and rules fixes use the same checked repairs as the Skills page. Secrets moves plaintext credentials from agent configs into Toolport's registry and your OS keychain. Agent configs are only rewired with --connect and a gateway install.",
    },
    Command {
        name: "init",
        summary: "Write a secret-free toolport.toml for this project",
        usage: "toolport init [--out FILE] [--force]",
        flags: &[
            valued("out", "FILE", "Where to write it (default toolport.toml, - for stdout)"),
            switch("force", "Replace an existing file"),
        ],
        positionals: (0, 0),
        examples: &["toolport init", "toolport init --out -"],
        about: "Reads the project's rules (AGENTS.md, CLAUDE.md), skills (.agents/skills, .claude/skills) and MCP configs (.mcp.json, .cursor/mcp.json, .vscode/mcp.json). Credentials become required env names, never values.",
    },
    Command {
        name: "up",
        summary: "Set up this project's agents from toolport.toml",
        usage: "toolport up [--file FILE] [--agents LIST] [--dry-run] [--yes]",
        flags: &[
            valued("file", "FILE", "Manifest to read (default toolport.toml in the project)"),
            valued("agents", "LIST", "Comma-separated agent ids (default: agents found here)"),
            switch("dry-run", "Show the plan without changing anything"),
            Flag { long: "yes", short: Some('y'), value: None, help: "Apply without asking" },
        ],
        positionals: (0, 0),
        examples: &["toolport up --dry-run", "toolport up --yes", "toolport up --agents claude-code,codex"],
        about: "Refuses a manifest that contains a literal secret. Links rules and skills so each agent reads them, adds servers to Toolport, asks for missing credentials without echoing them and stores them in your OS keychain.",
    },
    Command {
        name: "export",
        summary: "Export your Toolport servers as a secret-free setup file",
        usage: "toolport export [--out FILE|-] [--name NAME] [--description TEXT]",
        flags: &[
            valued("out", "FILE", "Where to write it (default - for stdout)"),
            valued("name", "NAME", "A name for the setup"),
            valued("description", "TEXT", "A description for the setup"),
        ],
        positionals: (0, 0),
        examples: &["toolport export --out team-setup.json", "toolport export | gh gist create -"],
        about: "Credentials, URL secrets and secret arguments are stripped. Commit the file or share it as a gist; others bring it in with toolport import.",
    },
    Command {
        name: "import",
        summary: "Import servers from a setup file",
        usage: "toolport import <FILE|-> [--only NAMES] [--dry-run] [--yes]",
        flags: &[
            valued("only", "NAMES", "Comma-separated server names to import"),
            switch("dry-run", "Show what would be added"),
            Flag { long: "yes", short: Some('y'), value: None, help: "Import without asking" },
        ],
        positionals: (1, 1),
        examples: &["toolport import team-setup.json --dry-run", "curl -s URL | toolport import - --yes"],
        about: "Imported servers start disabled. Each one shows what it runs or connects to before you decide.",
    },
    Command {
        name: "hook",
        summary: "Install a quiet drift check that runs when an agent session starts",
        usage: "toolport hook install|uninstall|check [--agent claude-code|codex] [--yes] [--dry-run]",
        flags: &[
            valued("agent", "ID", "claude-code or codex (default: both when present)"),
            switch("dry-run", "Show which settings files would change"),
            Flag { long: "yes", short: Some('y'), value: None, help: "Change settings files without asking" },
        ],
        positionals: (1, 1),
        examples: &["toolport hook install", "toolport hook check", "toolport hook uninstall --agent codex --yes"],
        about: "The check prints nothing when everything is healthy and one line when something drifted. Uninstall removes only Toolport's own entry and keeps a backup.",
    },
    Command {
        name: "report",
        summary: "Write a static HTML report card of scan and usage",
        usage: "toolport report [--out FILE] [--days N]",
        flags: &[
            valued("out", "FILE", "Where to write it (default toolport-report.html)"),
            valued("days", "N", "Usage window in days (default 30)"),
        ],
        positionals: (0, 0),
        examples: &["toolport report", "toolport report --out ~/report.html --days 7"],
        about: "One self-contained file with no scripts and no external requests. It holds counts, names and paths, never secret values.",
    },
];

/// Parsed command line.
#[derive(Debug, Default)]
pub struct Args {
    pub command: String,
    pub positionals: Vec<String>,
    pub values: BTreeMap<String, String>,
    pub switches: BTreeSet<String>,
}

impl Args {
    pub fn has(&self, name: &str) -> bool {
        self.switches.contains(name)
    }
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }
    pub fn number(&self, name: &str, default: u32, max: u32) -> Result<u32, CliError> {
        match self.value(name) {
            None => Ok(default),
            Some(raw) => match raw.parse::<u32>() {
                Ok(n) if n >= 1 && n <= max => Ok(n),
                _ => Err(CliError::new(format!(
                    "--{name} needs a whole number from 1 to {max}, not '{raw}'"
                ))),
            },
        }
    }
    pub fn list(&self, name: &str) -> Vec<String> {
        self.value(name)
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn find_command(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = if ca == *cb {
                prev
            } else {
                1 + prev.min(row[j]).min(cur)
            };
            prev = cur;
        }
    }
    row[b.len()]
}

fn suggest<'a>(input: &str, options: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    options
        .map(|o| (edit_distance(input, o), o))
        .filter(|(d, o)| *d <= 2.max(o.len() / 3))
        .min_by_key(|(d, _)| *d)
        .map(|(_, o)| o)
}

/// Parse argv (without the program name). `toolport --fix` is `toolport fix`.
pub fn parse(argv: &[String]) -> Result<Args, CliError> {
    let mut args = Args::default();
    let mut rest: Vec<&str> = Vec::new();
    let mut command: Option<&'static Command> = None;
    // The command is the first bare word; flags may come before or after it.
    let mut iter = argv.iter().map(String::as_str).peekable();
    let mut only_positionals = false;
    while let Some(token) = iter.next() {
        if only_positionals || token == "-" || !token.starts_with('-') {
            if command.is_none() && !only_positionals {
                command = Some(find_command(token).ok_or_else(|| {
                    let mut err = CliError::new(format!("unknown command '{token}'"));
                    if let Some(s) = suggest(token, COMMANDS.iter().map(|c| c.name)) {
                        err = err.hint(format!("did you mean 'toolport {s}'?"));
                    } else {
                        err = err.hint("run 'toolport --help' to see the commands");
                    }
                    err
                })?);
            } else {
                rest.push(token);
            }
            continue;
        }
        if token == "--" {
            only_positionals = true;
            continue;
        }
        if token == "--fix" && command.is_none() {
            command = find_command("fix");
            continue;
        }
        // Before a command word, the default command's options are accepted too
        // (`toolport --check`); they are checked against the final command below.
        let flags: Vec<Flag> = GLOBAL_FLAGS
            .iter()
            .chain(
                command
                    .unwrap_or_else(|| find_command("scan").unwrap())
                    .flags,
            )
            .copied()
            .collect();
        let (name, inline) = if let Some(long) = token.strip_prefix("--") {
            match long.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (long.to_string(), None),
            }
        } else {
            let mut chars = token[1..].chars();
            let short = chars.next().unwrap_or('-');
            if chars.next().is_some() {
                return Err(CliError::new(format!("unknown option '{token}'"))
                    .hint("short options can't be combined; pass them separately"));
            }
            match flags.iter().find(|f| f.short == Some(short)) {
                Some(f) => (f.long.to_string(), None),
                None => return Err(unknown_option(token, &flags, command)),
            }
        };
        let Some(flag) = flags.iter().find(|f| f.long == name) else {
            return Err(unknown_option(token, &flags, command));
        };
        match flag.value {
            Some(value_name) => {
                let value = match inline {
                    Some(v) => v,
                    None => match iter.next() {
                        Some(v) if v == "-" || !v.starts_with('-') => v.to_string(),
                        _ => {
                            return Err(CliError::new(format!(
                                "--{} needs a value: --{} <{value_name}>",
                                flag.long, flag.long
                            )))
                        }
                    },
                };
                args.values.insert(flag.long.to_string(), value);
            }
            None => {
                if inline.is_some() {
                    return Err(CliError::new(format!("--{} takes no value", flag.long)));
                }
                args.switches.insert(flag.long.to_string());
            }
        }
    }
    let command = command.unwrap_or_else(|| find_command("scan").unwrap());
    args.command = command.name.to_string();
    for name in args.switches.iter().chain(args.values.keys()) {
        let known = GLOBAL_FLAGS
            .iter()
            .chain(command.flags)
            .any(|f| f.long == name);
        if !known {
            return Err(CliError::new(format!(
                "'toolport {}' has no --{name} option",
                command.name
            ))
            .hint(format!(
                "run 'toolport {} --help' for its options",
                command.name
            )));
        }
    }
    if !args.has("help") && !args.has("version") {
        let (min, max) = command.positionals;
        if rest.len() < min {
            return Err(
                CliError::new(format!("missing argument for '{}'", command.name))
                    .hint(format!("usage: {}", command.usage)),
            );
        }
        if rest.len() > max {
            return Err(
                CliError::new(format!("unexpected argument '{}'", rest[max]))
                    .hint(format!("usage: {}", command.usage)),
            );
        }
    }
    args.positionals = rest.into_iter().map(String::from).collect();
    Ok(args)
}

fn unknown_option(token: &str, flags: &[Flag], command: Option<&Command>) -> CliError {
    let mut err = CliError::new(format!("unknown option '{token}'"));
    let bare = token.trim_start_matches('-');
    let bare = bare.split('=').next().unwrap_or(bare);
    if let Some(s) = suggest(bare, flags.iter().map(|f| f.long)) {
        err = err.hint(format!("did you mean '--{s}'?"));
    } else if let Some(c) = command {
        err = err.hint(format!("run 'toolport {} --help' for its options", c.name));
    } else if COMMANDS
        .iter()
        .any(|c| c.flags.iter().any(|f| f.long == bare))
    {
        let owner = COMMANDS
            .iter()
            .find(|c| c.flags.iter().any(|f| f.long == bare))
            .unwrap();
        err = err.hint(format!("--{bare} belongs to 'toolport {}'", owner.name));
    }
    err
}

// ---------------------------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------------------------

fn flag_rows(style: &Style, flags: &[Flag]) -> Vec<Vec<String>> {
    flags
        .iter()
        .map(|f| {
            let mut name = match f.short {
                Some(s) => format!("-{s}, --{}", f.long),
                None => format!("    --{}", f.long),
            };
            if let Some(v) = f.value {
                name.push_str(&format!(" <{v}>"));
            }
            vec![style.cyan(&name), f.help.to_string()]
        })
        .collect()
}

pub fn main_help(style: &Style) -> String {
    use format::{table, Align};
    let mut out = String::new();
    out.push_str(&format!(
        "{} {}\n",
        style.bold("toolport"),
        style.dim(&format!("{VERSION}  One setup for every coding agent."))
    ));
    out.push_str(&format!(
        "\n{} toolport [COMMAND] [OPTIONS]\n",
        style.bold("Usage:")
    ));
    out.push_str(&format!("\n{}\n", style.bold("Commands:")));
    let rows: Vec<Vec<String>> = COMMANDS
        .iter()
        .map(|c| vec![style.cyan(c.name), c.summary.to_string()])
        .collect();
    out.push_str(&table(&rows, &[Align::Left, Align::Left], 2));
    out.push_str(&format!("\n{}\n", style.bold("Options:")));
    out.push_str(&table(
        &flag_rows(style, GLOBAL_FLAGS),
        &[Align::Left, Align::Left],
        2,
    ));
    out.push_str(&format!(
        "\n{}\n  {}  check every agent\n  {}  see where your tokens go\n  {}  preview safe fixes\n  {}  bootstrap this repo for every agent\n",
        style.bold("Start here:"),
        style.cyan("toolport              "),
        style.cyan("toolport usage        "),
        style.cyan("toolport fix --dry-run"),
        style.cyan("toolport init && toolport up"),
    ));
    out.push_str(&format!(
        "\n{}\n",
        style.dim("Exit codes: 0 ok, 1 error, 2 findings (with --check). Docs: https://github.com/btsouth/toolport/blob/main/docs/cli.md")
    ));
    out
}

pub fn command_help(style: &Style, command: &Command) -> String {
    use format::{table, Align};
    let mut out = String::new();
    out.push_str(&format!("{}\n", command.summary));
    out.push_str(&format!("\n{}\n", wrap(command.about, 88)));
    out.push_str(&format!("\n{} {}\n", style.bold("Usage:"), command.usage));
    let mut flags: Vec<Flag> = command.flags.to_vec();
    flags.extend(GLOBAL_FLAGS.iter().copied());
    out.push_str(&format!("\n{}\n", style.bold("Options:")));
    out.push_str(&table(
        &flag_rows(style, &flags),
        &[Align::Left, Align::Left],
        2,
    ));
    if !command.examples.is_empty() {
        out.push_str(&format!("\n{}\n", style.bold("Examples:")));
        for e in command.examples {
            out.push_str(&format!("  {}\n", style.dim(e)));
        }
    }
    out
}

fn wrap(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            out.push_str(&line);
            out.push('\n');
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    out.push_str(&line);
    out
}

// ---------------------------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------------------------

/// Run the CLI and return the process exit code.
pub fn run(host: &Host, argv: Vec<String>) -> i32 {
    let no_color_early = argv.iter().any(|a| a == "--no-color");
    let style = Style::detect(no_color_early);
    let args = match parse(&argv) {
        Ok(args) => args,
        Err(error) => return report_error(&style, &error),
    };
    if args.has("version") {
        if args.has("json") {
            let mut value =
                serde_json::json!({"name": "toolport", "version": VERSION, "binary": host.binary});
            if let Some(detail) = &host.version_detail {
                value["detail"] = detail.as_str().into();
            }
            out(&value.to_string());
        } else {
            out(&format!("toolport {VERSION}"));
            if let Some(detail) = &host.version_detail {
                out(detail);
            }
        }
        return EXIT_OK;
    }
    let command = find_command(&args.command).expect("parsed command exists");
    if args.has("help") {
        let explicit = argv.iter().any(|a| a == &args.command);
        out(&if explicit {
            command_help(&style, command)
        } else {
            let mut help = main_help(&style);
            if let Some(footer) = host.help_footer {
                help.push('\n');
                help.push_str(&footer(&style));
            }
            help
        });
        return EXIT_OK;
    }
    let project = match args.value("project") {
        Some(dir) => {
            let path = PathBuf::from(dir);
            if !path.is_dir() {
                return report_error(
                    &style,
                    &CliError::new(format!("project folder '{dir}' doesn't exist")),
                );
            }
            Some(std::fs::canonicalize(&path).unwrap_or(path))
        }
        None => std::env::current_dir().ok(),
    };
    let ctx = Ctx {
        json: args.has("json"),
        style,
        project,
        interactive: std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
    };
    let result = match command.name {
        "scan" => scan::command(&ctx, &args),
        "usage" => usage::command(&ctx, &args),
        "fix" => fix::command(&ctx, &args),
        "init" => manifest::init_command(&ctx, &args),
        "up" => manifest::up_command(&ctx, &args),
        "export" => portability::export_command(&ctx, &args),
        "import" => portability::import_command(&ctx, &args),
        "hook" => hook::command(&ctx, &args),
        "report" => report::command(&ctx, &args),
        _ => unreachable!("every command is dispatched"),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            if ctx.json {
                let _ = ctx.emit_json(&serde_json::json!({
                    "error": error.message,
                    "hint": error.hint,
                }));
                error.code
            } else {
                report_error(&ctx.style, &error)
            }
        }
    }
}

fn report_error(style: &Style, error: &CliError) -> i32 {
    eprintln!("{} {}", style.red(&style.bold("error:")), error.message);
    if let Some(hint) = &error.hint {
        eprintln!("  {}", style.hint(hint));
    }
    error.code
}

/// Read a line without echoing it, for credentials. Falls back to a visible read when the
/// terminal can't be switched (the caller only asks on a terminal).
pub fn read_secret(prompt: &str) -> Option<String> {
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let restore = echo_off();
    let mut line = String::new();
    let ok = std::io::stdin().read_line(&mut line).is_ok();
    drop(restore);
    eprintln!();
    let value = line.trim_end_matches(['\r', '\n']).to_string();
    (ok && !value.is_empty()).then_some(value)
}

struct EchoGuard {
    #[cfg(unix)]
    saved: Option<libc::termios>,
    #[cfg(windows)]
    saved: Option<(windows_sys::Win32::Foundation::HANDLE, u32)>,
}

impl Drop for EchoGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(saved) = self.saved {
            // SAFETY: restores the attributes read from the same descriptor.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved);
            }
        }
        #[cfg(windows)]
        if let Some((handle, mode)) = self.saved {
            // SAFETY: restores the mode read from the same console handle.
            unsafe {
                windows_sys::Win32::System::Console::SetConsoleMode(handle, mode);
            }
        }
    }
}

fn echo_off() -> EchoGuard {
    #[cfg(unix)]
    {
        // SAFETY: termios is plain data; tcgetattr fills it or fails without side effects.
        unsafe {
            let mut term: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut term) != 0 {
                return EchoGuard { saved: None };
            }
            let saved = term;
            term.c_lflag &= !libc::ECHO;
            term.c_lflag |= libc::ECHONL;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &term) != 0 {
                return EchoGuard { saved: None };
            }
            EchoGuard { saved: Some(saved) }
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_ECHO_INPUT, STD_INPUT_HANDLE,
        };
        // SAFETY: console mode calls on the process's own stdin handle.
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode = 0u32;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return EchoGuard { saved: None };
            }
            if SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT) == 0 {
                return EchoGuard { saved: None };
            }
            EchoGuard {
                saved: Some((handle, mode)),
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        EchoGuard {}
    }
}

/// Toolport's own data folder, created on demand. Errors name what to set.
pub fn data_dir() -> Result<PathBuf, CliError> {
    crate::registry::conduit_dir().ok_or_else(|| {
        CliError::new("couldn't find Toolport's data folder")
            .hint("set TOOLPORT_DATA_DIR to a writable folder")
    })
}

/// Milliseconds since the epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn cli_invocations_are_told_apart_from_desktop_launches() {
        let os =
            |s: &str| -> Vec<std::ffi::OsString> { s.split_whitespace().map(Into::into).collect() };
        for command in COMMANDS {
            assert!(is_cli_invocation(&os(command.name)), "{}", command.name);
            assert!(is_cli_invocation(&os(&format!("{} --json", command.name))));
        }
        for line in [
            "-h",
            "--help",
            "-V",
            "--version",
            "--version --json",
            "--fix",
            "--fix --dry-run",
            "--check",
            "--all",
            "--json",
            "--no-color",
            "--project /tmp",
            "--project=/tmp scan",
            "--",
            "-",
            "scna",
        ] {
            assert!(is_cli_invocation(&os(line)), "{line}");
        }
        for line in [
            "",
            "--hidden",
            "--hidden --version",
            "toolport://open/servers",
            "conduit://pair?code=1",
            "--gapplication-service",
            "--display=:1",
            "-x",
        ] {
            assert!(!is_cli_invocation(&os(line)), "{line}");
        }
    }

    #[test]
    fn default_command_is_scan_and_flags_go_anywhere() {
        let a = parse(&argv("--json")).unwrap();
        assert_eq!(a.command, "scan");
        assert!(a.has("json"));
        let a = parse(&argv("--project /tmp usage --days 7 --json")).unwrap();
        assert_eq!(a.command, "usage");
        assert_eq!(a.value("project"), Some("/tmp"));
        assert_eq!(a.value("days"), Some("7"));
        let a = parse(&argv("usage --days=3")).unwrap();
        assert_eq!(a.number("days", 30, 365).unwrap(), 3);
    }

    #[test]
    fn fix_alias_and_positionals() {
        assert_eq!(parse(&argv("--fix --dry-run")).unwrap().command, "fix");
        let a = parse(&argv("import - --yes")).unwrap();
        assert_eq!(a.positionals, vec!["-"]);
        assert!(a.has("yes"));
        let a = parse(&argv("fix -y")).unwrap();
        assert!(a.has("yes"));
        let a = parse(&argv("hook check")).unwrap();
        assert_eq!(a.positionals, vec!["check"]);
    }

    #[test]
    fn helpful_errors() {
        let e = parse(&argv("scna")).unwrap_err();
        assert_eq!(e.hint.as_deref(), Some("did you mean 'toolport scan'?"));
        let e = parse(&argv("usage --dyas 3")).unwrap_err();
        assert_eq!(e.hint.as_deref(), Some("did you mean '--days'?"));
        let e = parse(&argv("--days 3")).unwrap_err();
        assert!(e.hint.unwrap().contains("toolport usage"));
        assert_eq!(parse(&argv("--check")).unwrap().command, "scan");
        assert!(parse(&argv("--check usage")).is_err());
        assert!(parse(&argv("usage --days")).is_err());
        assert!(parse(&argv("import")).is_err());
        assert!(parse(&argv("scan extra")).is_err());
        assert!(parse(&argv("--json=1")).is_err());
        let a = parse(&argv("usage --days 0")).unwrap();
        assert!(a.number("days", 30, 365).is_err());
    }

    #[test]
    fn help_mentions_every_command() {
        let help = main_help(&Style::plain());
        for c in COMMANDS {
            assert!(help.contains(c.name), "{}", c.name);
            let h = command_help(&Style::plain(), c);
            assert!(h.contains(c.usage));
        }
        assert!(!help.contains('\u{2014}') && !help.contains('\u{2013}'));
    }
}
