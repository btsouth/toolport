//! Pasted `claude mcp add` commands.
//!
//! Matches `claude mcp add [options] <name> <commandOrUrl> [args...]` from
//! Claude Code's CLI help. Input is only split into words: nothing is run,
//! and no variable, glob or command substitution is expanded. Shell syntax
//! that would run something else (`;`, `&&`, pipes, redirects, backticks,
//! `$(...)`) is refused rather than guessed at.

use super::{json_server_with_values, ParsedSnippetServer};

/// One shell word. `expands` marks a word a shell would have changed before
/// Claude saw it: an unquoted or double-quoted `$NAME` or `${NAME}`, or an
/// unquoted leading `~`. The text keeps them as written.
struct Word {
    text: String,
    expands: bool,
}

/// Whether `$` followed by `next` starts a parameter expansion.
fn expansion_start(next: Option<&char>) -> bool {
    next.is_some_and(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, '_' | '{' | '@' | '*' | '#' | '?' | '!' | '-' | '$')
    })
}

/// Split one pasted shell command into words.
///
/// Supports single and double quotes, backslash escapes and backslash line
/// continuations. A bare newline ends the command, so anything after it is a
/// second command and is refused.
fn words(input: &str) -> Result<Vec<Word>, PasteError> {
    words_up_to(input, usize::MAX)
}

/// [`words`], stopping once `limit` words are complete. The rest of the input
/// is not read, so it cannot fail.
fn words_up_to(input: &str, limit: usize) -> Result<Vec<Word>, PasteError> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut expands = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\0' {
            return Err(PasteError::Nul);
        }
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    word.push(c);
                }
            }
            Some(_) => match c {
                '"' => quote = None,
                '\\' => match chars.next() {
                    None => return Err(PasteError::UnclosedQuote),
                    Some('\n') => {}
                    Some('\r') if chars.peek() == Some(&'\n') => {
                        chars.next();
                    }
                    Some(next @ ('$' | '`' | '"' | '\\')) => word.push(next),
                    Some(next) => {
                        word.push('\\');
                        word.push(next);
                    }
                },
                '`' => return Err(PasteError::Substitution),
                '$' if chars.peek() == Some(&'(') => return Err(PasteError::Substitution),
                '$' => {
                    expands |= expansion_start(chars.peek());
                    word.push(c);
                }
                _ => word.push(c),
            },
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    started = true;
                }
                '\\' => match chars.next() {
                    None => return Err(PasteError::TrailingBackslash),
                    Some('\n') => {}
                    Some('\r') if chars.peek() == Some(&'\n') => {
                        chars.next();
                    }
                    Some(next) => {
                        word.push(next);
                        started = true;
                    }
                },
                '\n' => {
                    if chars.any(|c| !c.is_whitespace()) {
                        return Err(PasteError::SecondCommand);
                    }
                    break;
                }
                '#' if !started => {
                    // A comment runs to the end of the line, which also ends
                    // the command.
                    if chars.by_ref().find(|c| *c == '\n').is_some()
                        && chars.any(|c| !c.is_whitespace())
                    {
                        return Err(PasteError::SecondCommand);
                    }
                    break;
                }
                c if c.is_whitespace() => {
                    if started {
                        words.push(Word {
                            text: std::mem::take(&mut word),
                            expands: std::mem::take(&mut expands),
                        });
                        started = false;
                        if words.len() == limit {
                            return Ok(words);
                        }
                    }
                }
                '~' if !started => {
                    expands = true;
                    word.push(c);
                    started = true;
                }
                ';' | '&' | '|' | '<' | '>' | '(' | ')' => {
                    return Err(PasteError::Operator(c));
                }
                '`' => return Err(PasteError::Substitution),
                '$' if matches!(chars.peek(), Some('(')) => return Err(PasteError::Substitution),
                '$' if matches!(chars.peek(), Some('\'' | '"')) => {
                    return Err(PasteError::AnsiQuote);
                }
                '$' => {
                    expands |= expansion_start(chars.peek());
                    word.push(c);
                    started = true;
                }
                _ => {
                    word.push(c);
                    started = true;
                }
            },
        }
    }
    if quote.is_some() {
        return Err(PasteError::UnclosedQuote);
    }
    if started {
        words.push(Word {
            text: word,
            expands,
        });
    }
    Ok(words)
}

/// `KEY=value` for a repeated `-e` value. Claude's `-e` takes several values
/// in a row, so a following word belongs to it only when it has this shape.
fn env_pair(value: &str) -> Option<(&str, &str)> {
    let (key, value) = value.split_once('=')?;
    let valid = !key.is_empty()
        && !key.starts_with(|c: char| c.is_ascii_digit())
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then_some((key, value))
}

/// `Name: value` for a header, with an HTTP token as the name. A URL such as
/// `https://host/mcp` is not a header even though it contains a colon.
fn header_pair(value: &str) -> Option<(&str, &str)> {
    let (name, rest) = value.split_once(':')?;
    let token = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c);
    if name.is_empty() || !name.chars().all(token) || rest.starts_with("//") {
        return None;
    }
    Some((name, rest.trim()))
}

/// Why a pasted command was refused. It carries no pasted text: the message is
/// shown to the user and copied from toasts, and a paste is full of secrets. An
/// operator is one of a fixed set of characters and an option name is Toolport's
/// own spelling, so neither can hold a pasted value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PasteError {
    Nul,
    UnclosedQuote,
    TrailingBackslash,
    SecondCommand,
    Operator(char),
    Substitution,
    AnsiQuote,
    UnsupportedOption,
    OAuthClient,
    MissingValue(&'static str),
    EnvFormat,
    HeaderFormat,
    HeaderLineBreak,
    MissingName,
    MissingTarget,
    ShellVariable,
    HeadersOnStdio,
    ExtraRemoteArgs,
    EnvOnRemote,
    RemoteUrl,
    Transport,
    CustomHeader,
}

impl PasteError {
    /// The message in the user's language. Every source is a literal so the
    /// locale checker sees it.
    pub(super) fn message(self) -> String {
        use crate::i18n::{tr, trf};
        match self {
            Self::Nul => tr("The pasted command contains a hidden NUL character. Copy it again from its source."),
            Self::UnclosedQuote => tr("A quote in the pasted command is never closed. Add the closing quote."),
            Self::TrailingBackslash => tr("The pasted command ends with a backslash. Remove it or paste the rest of the command."),
            Self::SecondCommand => tr("Paste one command at a time. Remove everything after the claude mcp add command."),
            Self::Operator(c) => trf("Remove {value0} from the pasted command or put it in quotes. Toolport reads one command and never runs it.", &[("value0", &format!("'{c}'"))]),
            Self::Substitution => tr("Replace $(...) and backticks with the value itself. Toolport never runs commands from a paste."),
            Self::AnsiQuote => tr("Replace $'...' and $\"...\" quoting with plain single or double quotes."),
            Self::UnsupportedOption => tr("Toolport reads only -t, -s, -e, -H and --callback-port before --. Move the server's own arguments after --."),
            Self::OAuthClient => tr("Remove --client-id and --client-secret. Toolport signs in to the server itself and cannot use a pre-registered OAuth client yet."),
            Self::MissingValue(option) => trf("Add a value after {value0}.", &[("value0", option)]),
            Self::EnvFormat => tr("Write each -e value as KEY=value."),
            Self::HeaderFormat => tr("Write each -H value as Name: value."),
            Self::HeaderLineBreak => tr("Remove the line break from the -H value."),
            Self::MissingName => tr("Add a server name after claude mcp add."),
            Self::MissingTarget => tr("Add the server's command or URL after its name."),
            Self::ShellVariable => tr("Replace the shell variable or ~ in the server name, command or URL with the value itself."),
            Self::HeadersOnStdio => tr("Headers (-H) apply only to http and sse servers. Add -t http or -t sse, or remove the headers."),
            Self::ExtraRemoteArgs => tr("A remote server takes one URL. Remove the words after it."),
            Self::EnvOnRemote => tr("Environment variables apply only to stdio servers. Remove them for an http or sse server."),
            Self::RemoteUrl => tr("Use an http:// or https:// URL for a remote server."),
            Self::Transport => tr("Use stdio, http or sse after -t."),
            Self::CustomHeader => tr("Toolport can send only an Authorization: Bearer header for now. Remove the other headers, or keep this server in its own client."),
        }
    }
}

enum Flag {
    Transport,
    Scope,
    Env,
    Header,
    ClientId,
    CallbackPort,
    ClientSecret,
}

impl Flag {
    /// Toolport's own spelling of the option, for messages.
    fn name(&self) -> &'static str {
        match self {
            Flag::Transport => "-t",
            Flag::Scope => "-s",
            Flag::Env => "-e",
            Flag::Header => "-H",
            Flag::ClientId => "--client-id",
            Flag::CallbackPort => "--callback-port",
            Flag::ClientSecret => "--client-secret",
        }
    }
}

fn long_flag(name: &str) -> Option<Flag> {
    Some(match name {
        "--transport" => Flag::Transport,
        "--scope" => Flag::Scope,
        "--env" => Flag::Env,
        "--header" => Flag::Header,
        "--client-id" => Flag::ClientId,
        "--callback-port" => Flag::CallbackPort,
        "--client-secret" => Flag::ClientSecret,
        _ => return None,
    })
}

fn short_flag(letter: char) -> Option<Flag> {
    Some(match letter {
        't' => Flag::Transport,
        's' => Flag::Scope,
        'e' => Flag::Env,
        'H' => Flag::Header,
        _ => return None,
    })
}

/// Parse a `claude mcp add ...` command. `Ok(None)` means the input is some
/// other format (including `claude mcp add-json`).
pub(super) fn parse(input: &str) -> Result<Option<ParsedSnippetServer>, PasteError> {
    // Read the prefix with the same word rules as the rest, so quoting and
    // line continuations in it still match.
    let is_add = match words_up_to(input, 3) {
        Ok(head) => head
            .iter()
            .map(|w| w.text.as_str())
            .eq(["claude", "mcp", "add"]),
        // Plainly this command, with a problem the full read below names.
        Err(_) => input
            .split_whitespace()
            .take(3)
            .eq(["claude", "mcp", "add"]),
    };
    if !is_add {
        return Ok(None);
    }
    let words = words(input)?;
    let mut transport = "stdio".to_string();
    let mut env = serde_json::Map::new();
    let mut headers = serde_json::Map::new();
    let mut positional: Vec<&Word> = Vec::new();
    let mut i = 3;
    while i < words.len() {
        let token = words[i].text.as_str();
        i += 1;
        if token == "--" {
            positional.extend(&words[i..]);
            break;
        }
        let (flag, attached) = if let Some(long) = token.strip_prefix("--") {
            let (name, value) = long
                .split_once('=')
                .map_or((long, None), |(n, v)| (n, Some(v)));
            let name = format!("--{name}");
            let flag = long_flag(&name).ok_or(PasteError::UnsupportedOption)?;
            (flag, value)
        } else if token.len() > 1 && token.starts_with('-') {
            let mut chars = token[1..].chars();
            let letter = chars.next().unwrap_or('-');
            let flag = short_flag(letter).ok_or(PasteError::UnsupportedOption)?;
            let rest = chars.as_str();
            (flag, (!rest.is_empty()).then_some(rest))
        } else {
            positional.push(&words[i - 1]);
            continue;
        };
        if matches!(flag, Flag::ClientId | Flag::ClientSecret) {
            // Toolport signs in with its own OAuth client. Dropping a
            // pre-registered one would import a server that cannot sign in.
            return Err(PasteError::OAuthClient);
        }
        // `expands` of the word the value came from: an attached value shares
        // the flag's word.
        let (value, expands) = match attached {
            Some(value) => (value, words[i - 1].expands),
            None => {
                let word = words.get(i).ok_or(PasteError::MissingValue(flag.name()))?;
                i += 1;
                (word.text.as_str(), word.expands)
            }
        };
        match flag {
            Flag::Transport => transport = value.to_string(),
            // Scope picks Claude's config file and the callback port only
            // matters with a pre-registered client, so neither is part of a
            // Toolport server definition.
            Flag::Scope | Flag::CallbackPort | Flag::ClientId | Flag::ClientSecret => {}
            Flag::Env => {
                let (mut value, mut expands) = (value, expands);
                loop {
                    let (key, val) = env_pair(value).ok_or(PasteError::EnvFormat)?;
                    env.insert(key.to_string(), shell_value(val, expands));
                    match words.get(i).filter(|w| env_pair(&w.text).is_some()) {
                        Some(next) => {
                            (value, expands) = (next.text.as_str(), next.expands);
                            i += 1;
                        }
                        None => break,
                    }
                }
            }
            Flag::Header => {
                let (mut value, mut expands) = (value, expands);
                loop {
                    let (name, val) = header_pair(value).ok_or(PasteError::HeaderFormat)?;
                    if val.contains(['\r', '\n']) {
                        return Err(PasteError::HeaderLineBreak);
                    }
                    // Header names are case-insensitive; the last one wins.
                    headers.retain(|existing: &String, _| !existing.eq_ignore_ascii_case(name));
                    headers.insert(name.to_string(), shell_value(val, expands));
                    match words.get(i).filter(|w| header_pair(&w.text).is_some()) {
                        Some(next) => {
                            (value, expands) = (next.text.as_str(), next.expands);
                            i += 1;
                        }
                        None => break,
                    }
                }
            }
        }
    }

    // A shell would have replaced `$NAME` or `~` in the name, command or
    // arguments with something only the pasting machine knows.
    if positional.iter().any(|w| w.expands) {
        return Err(PasteError::ShellVariable);
    }
    let mut positional = positional.into_iter().map(|w| w.text.clone());
    let name = positional
        .next()
        .filter(|n| !n.trim().is_empty())
        .ok_or(PasteError::MissingName)?;
    let target = positional
        .next()
        .filter(|t| !t.trim().is_empty())
        .ok_or(PasteError::MissingTarget)?;
    let rest: Vec<String> = positional.collect();
    let definition = match transport.as_str() {
        "stdio" => {
            if !headers.is_empty() {
                return Err(PasteError::HeadersOnStdio);
            }
            serde_json::json!({"command": target, "args": rest, "env": env})
        }
        "http" | "sse" => {
            if !rest.is_empty() {
                return Err(PasteError::ExtraRemoteArgs);
            }
            if !env.is_empty() {
                return Err(PasteError::EnvOnRemote);
            }
            let lower = target.to_ascii_lowercase();
            if !lower.starts_with("https://") && !lower.starts_with("http://") {
                return Err(PasteError::RemoteUrl);
            }
            if headers
                .iter()
                .any(|(name, value)| !super::header_supported(name, value.as_str()))
            {
                return Err(PasteError::CustomHeader);
            }
            serde_json::json!({"type": transport, "url": target, "headers": headers})
        }
        _ => return Err(PasteError::Transport),
    };
    Ok(Some(json_server_with_values(&name, &definition)))
}

/// A value a shell would have expanded is unknown here. Keep the key with no
/// value so the import asks for it, rather than storing `$TOKEN` as a secret.
fn shell_value(value: &str, expands: bool) -> serde_json::Value {
    if expands {
        serde_json::Value::Null
    } else {
        serde_json::json!(value)
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse_snippet;
    use super::super::ParsedSnippetServer;

    fn one(input: &str) -> ParsedSnippetServer {
        let mut servers = parse_snippet(input).unwrap_or_else(|e| panic!("{input}: {e}"));
        assert_eq!(servers.len(), 1, "{input}");
        servers.remove(0)
    }

    fn env(server: &ParsedSnippetServer, key: &str) -> Option<String> {
        server
            .env
            .iter()
            .find(|v| v.key == key)
            .and_then(|v| v.value.clone())
    }

    fn refused(input: &str) -> String {
        match parse_snippet(input) {
            Ok(servers) => panic!("{input} parsed as {servers:?}"),
            Err(error) => error,
        }
    }

    // Examples from `claude mcp add --help` (Claude Code 2.1.325) and
    // https://code.claude.com/docs/en/mcp.
    #[test]
    fn documented_commands() {
        let sentry = one("claude mcp add --transport http sentry https://mcp.sentry.dev/mcp");
        assert_eq!(sentry.name, "sentry");
        assert_eq!(sentry.transport, "http");
        assert_eq!(sentry.url.as_deref(), Some("https://mcp.sentry.dev/mcp"));
        assert!(sentry.command.is_none());

        let corridor = one(
            r#"claude mcp add --transport http corridor https://app.corridor.dev/api/mcp --header "Authorization: Bearer abc""#,
        );
        assert_eq!(
            env(&corridor, "Authorization").as_deref(),
            Some("Bearer abc")
        );

        let local = one("claude mcp add my-server -e API_KEY=xxx -- node ./my-server.js");
        assert_eq!(local.transport, "stdio");
        assert_eq!(local.command.as_deref(), Some("node"));
        assert_eq!(local.args, ["./my-server.js"]);
        assert_eq!(env(&local, "API_KEY").as_deref(), Some("xxx"));

        let flags = one("claude mcp add my-server -- my-command --some-flag arg1");
        assert_eq!(flags.command.as_deref(), Some("my-command"));
        assert_eq!(flags.args, ["--some-flag", "arg1"]);

        let airtable = one("claude mcp add --env AIRTABLE_API_KEY=YOUR_KEY --transport stdio airtable \\\n  -- npx -y airtable-mcp-server");
        assert_eq!(airtable.name, "airtable");
        assert_eq!(airtable.command.as_deref(), Some("npx"));
        assert_eq!(airtable.args, ["-y", "airtable-mcp-server"]);
        assert_eq!(
            env(&airtable, "AIRTABLE_API_KEY").as_deref(),
            Some("YOUR_KEY")
        );

        let sse = one("claude mcp add --transport sse -s user asana https://mcp.asana.com/sse");
        assert_eq!(sse.transport, "sse");
        assert_eq!(sse.url.as_deref(), Some("https://mcp.asana.com/sse"));

        let plain = one("claude mcp add files npx @modelcontextprotocol/server-filesystem /tmp");
        assert_eq!(plain.command.as_deref(), Some("npx"));
        assert_eq!(
            plain.args,
            ["@modelcontextprotocol/server-filesystem", "/tmp"]
        );
    }

    #[test]
    fn quoting_and_option_boundary() {
        let server = one(
            r#"claude mcp add -e FOO='two words' --env=EMPTY= --scope project local -- '/some path/server' --env server-option '' "a \"q\" \$x \n""#,
        );
        assert_eq!(server.command.as_deref(), Some("/some path/server"));
        // Options after `--` belong to the server, not to Claude.
        assert_eq!(
            server.args,
            ["--env", "server-option", "", r#"a "q" $x \n"#]
        );
        assert_eq!(env(&server, "FOO").as_deref(), Some("two words"));
        assert_eq!(env(&server, "EMPTY").as_deref(), Some(""));

        let mixed = one(r#"claude mcp add "my server" -- py"thon"3 a\ b 'it'\''s'"#);
        assert_eq!(mixed.name, "my server");
        assert_eq!(mixed.command.as_deref(), Some("python3"));
        assert_eq!(mixed.args, ["a b", "it's"]);
    }

    #[test]
    fn line_continuations() {
        let server = one("claude mcp add \\\r\n  --transport stdio \\\n  -e A=1 \\\n  demo \\\n  -- \\\n  node \"multi\\\nline\" server.js\n\n");
        assert_eq!(server.name, "demo");
        assert_eq!(server.command.as_deref(), Some("node"));
        assert_eq!(server.args, ["multiline", "server.js"]);
        assert_eq!(env(&server, "A").as_deref(), Some("1"));
        // A newline inside single quotes is part of the word.
        let quoted = one("claude mcp add demo -- node 'a\nb'");
        assert_eq!(quoted.args, ["a\nb"]);
    }

    #[test]
    fn repeated_and_variadic_env_and_header_flags() {
        let server = one("claude mcp add -e A=1 -e B=2 --env C=3 -e D=4 E=5 -eF=6 demo -- cmd");
        for (key, value) in [
            ("A", "1"),
            ("B", "2"),
            ("C", "3"),
            ("D", "4"),
            ("E", "5"),
            ("F", "6"),
        ] {
            assert_eq!(env(&server, key).as_deref(), Some(value), "{key}");
        }
        assert_eq!(server.name, "demo");
        // Repeating a key keeps the last value, like Claude.
        let again = one("claude mcp add -e A=1 -e A=2 demo -- cmd");
        assert_eq!(env(&again, "A").as_deref(), Some("2"));
        // A value that is itself KEY=value still parses: only the first `=` splits.
        let eq = one("claude mcp add -e URL=https://x/?a=b demo -- cmd");
        assert_eq!(env(&eq, "URL").as_deref(), Some("https://x/?a=b"));

        for spelling in [
            r#"-H "Authorization: Bearer 1" -H 'Authorization:Bearer 2'"#,
            r#"--header "Authorization: Bearer 1" "Authorization: Bearer 2""#,
            r#"-H "Authorization: Bearer 1" --header="Authorization: Bearer 2""#,
        ] {
            let remote = one(&format!(
                "claude mcp add -t http {spelling} api https://api.example.com/mcp"
            ));
            assert_eq!(
                env(&remote, "Authorization").as_deref(),
                Some("Bearer 2"),
                "{spelling}"
            );
            assert_eq!(remote.url.as_deref(), Some("https://api.example.com/mcp"));
        }
        // The URL after a header is the URL, not another header.
        let url_after = one(
            r#"claude mcp add --transport sse api -H "Authorization: Bearer 1" https://api.example.com/sse"#,
        );
        assert_eq!(
            url_after.url.as_deref(),
            Some("https://api.example.com/sse")
        );

        // The callback port only matters with a pre-registered client.
        let port =
            one("claude mcp add --transport http --callback-port 8080 api https://x.example/mcp");
        assert_eq!(port.url.as_deref(), Some("https://x.example/mcp"));

        // Header names are case-insensitive: the last one wins.
        let case = one(
            r#"claude mcp add -t http -H "Authorization: Bearer a" -H "authorization: bearer b" api https://x.example/mcp"#,
        );
        assert_eq!(case.env.len(), 1, "{case:?}");
        assert_eq!(case.env[0].key, "authorization");
        assert_eq!(case.env[0].value.as_deref(), Some("bearer b"));
    }

    /// Toolport sends only `Authorization: Bearer` for a pasted server. Any
    /// other header is refused, never saved as a credential that is not sent
    /// (or sent as the bearer token instead).
    #[test]
    fn headers_other_than_bearer_authorization_are_refused() {
        for bad in [
            "claude mcp add -t http demo https://x.example/mcp -H 'X-API-Key: abc'",
            "claude mcp add -t sse demo https://x.example/sse -H 'X-Team: t1'",
            "claude mcp add -t http demo https://x.example/mcp -H 'Authorization: Basic abc'",
            "claude mcp add -t http demo https://x.example/mcp -H 'Authorization: abc'",
            "claude mcp add -t http -H 'Authorization: Bearer a' -H 'X-Team: t' demo https://x.example/mcp",
            "claude mcp add -t http demo https://x.example/mcp -H 'X-Key: $KEY'",
        ] {
            assert_eq!(
                super::parse(bad).err(),
                Some(super::PasteError::CustomHeader),
                "{bad}"
            );
        }
        // A shell variable is asked for, so its scheme is not known yet.
        let asked =
            one(r#"claude mcp add -t http demo https://x.example/mcp -H "Authorization: $TOKEN""#);
        assert_eq!(env(&asked, "Authorization"), None);
    }

    /// The prefix follows the same word rules as the rest of the command.
    #[test]
    fn quoted_or_continued_prefix_is_recognised() {
        for input in [
            "claude \\\nmcp add demo -- node server.js",
            "claude \\\r\nmcp \\\n  add demo -- node server.js",
            "'claude' mcp \"add\" demo -- node server.js",
            "cl\\aude mcp add demo -- node server.js",
        ] {
            let server = one(input);
            assert_eq!(server.name, "demo", "{input:?}");
            assert_eq!(server.args, ["server.js"], "{input:?}");
        }
        // Errors in the rest still reach the user, even a bare line break.
        assert_eq!(
            super::parse("claude mcp add\n demo -- node s.js").err(),
            Some(super::PasteError::SecondCommand)
        );
        assert_eq!(
            super::parse("claude \\\nmcp add demo -- node a; b").err(),
            Some(super::PasteError::Operator(';'))
        );
        for other in [
            "claude mcp add-json",
            "claude mcpadd x",
            "'claude mcp add' x -- y",
        ] {
            assert!(super::parse(other).unwrap().is_none(), "{other}");
        }
    }

    #[test]
    fn add_json_is_left_to_its_own_parser() {
        let server = one(r#"claude mcp add-json demo '{"command":"node","args":["a; $(x) `y`"]}'"#);
        assert_eq!(server.command.as_deref(), Some("node"));
        assert!(super::parse("claude mcp add-json x '{}'")
            .unwrap()
            .is_none());
        assert!(super::parse("claude mcp list").unwrap().is_none());
    }

    #[test]
    fn invalid_commands_are_refused() {
        for bad in [
            "claude mcp add",
            "claude mcp add name",
            "claude mcp add --env",
            "claude mcp add --env broken name -- cmd",
            "claude mcp add -e =1 name -- cmd",
            "claude mcp add --transport ftp name url",
            "claude mcp add -t http name ftp://host/mcp",
            "claude mcp add -t http name https://one https://two",
            "claude mcp add -t http -e A=1 name https://one",
            "claude mcp add -H 'X: 1' name -- cmd",
            "claude mcp add -H nocolon -t http name https://one",
            "claude mcp add name npx -y pkg",
            "claude mcp add --unknown name -- cmd",
            "claude mcp add --client-secret=x -t http name https://one",
            "claude mcp add '' -- cmd",
            "claude mcp add name -- ''",
            "claude mcp add -t http name https://one -H \"X: a\nInjected: b\"",
            // Toolport signs in with its own client; dropping one would break sign-in.
            "claude mcp add -t http --client-id abc api https://x.example/mcp",
            "claude mcp add -t http --client-secret api https://x.example/mcp",
            "claude mcp add -t http --client-id=abc api https://x.example/mcp",
        ] {
            refused(bad);
        }
    }

    #[test]
    fn hostile_input_is_refused_or_kept_literal() {
        let marker =
            std::env::temp_dir().join(format!("toolport-paste-ran-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let m = marker.display();
        for bad in [
            "claude mcp add name -- 'oops".to_string(),
            "claude mcp add name -- \"oops".to_string(),
            "claude mcp add name -- cmd \\".to_string(),
            format!("claude mcp add name -- cmd; touch {m}"),
            format!("claude mcp add name -- cmd && touch {m}"),
            format!("claude mcp add name -- cmd || touch {m}"),
            format!("claude mcp add name -- cmd | touch {m}"),
            format!("claude mcp add name -- cmd & touch {m}"),
            format!("claude mcp add name -- cmd > {m}"),
            format!("claude mcp add name -- cmd < {m}"),
            format!("claude mcp add name -- cmd `touch {m}`"),
            format!("claude mcp add name -- cmd $(touch {m})"),
            format!("claude mcp add name -- cmd \"$(touch {m})\""),
            format!("claude mcp add name -- cmd \"`touch {m}`\""),
            format!("claude mcp add -e K=$(touch {m}) name -- cmd"),
            format!("claude mcp add name -- cmd\ntouch {m}"),
            format!("claude mcp add name -- cmd # note\ntouch {m}"),
            format!("claude mcp add name -- (touch {m})"),
            "claude mcp add name -- cmd $'\\xff'".to_string(),
            "claude mcp add name -- cmd \0arg".to_string(),
        ] {
            refused(&bad);
        }
        assert!(!marker.exists(), "parsing must never run anything");

        // Quoted operators and substitutions are plain text, never expanded.
        let literal = one(&format!(
            "claude mcp add -e HOME_REF=$HOME -e B='${{X}}' name -- cmd 'a; b && c' '$(touch {m})' \"x|y\" '`id`'"
        ));
        assert_eq!(
            literal.args,
            ["a; b && c", &format!("$(touch {m})"), "x|y", "`id`"]
        );
        // An unexpanded variable is unknown here, so the import asks for it.
        let home = literal.env.iter().find(|v| v.key == "HOME_REF").unwrap();
        assert_eq!(home.value, None);
        assert_eq!(env(&literal, "B").as_deref(), Some("${X}"));
        assert!(!marker.exists(), "parsing must never run anything");

        // A trailing comment is ignored, as a shell would.
        let commented = one("claude mcp add name -- cmd arg # comment; rm -rf /\n");
        assert_eq!(commented.args, ["arg"]);
    }

    #[test]
    fn shell_variables_are_never_stored_as_values() {
        // The documented pattern: Claude would receive the real token.
        let remote = one(
            r#"claude mcp add -t http gh https://api.githubcopilot.com/mcp -H "Authorization: Bearer $GITHUB_TOKEN""#,
        );
        let auth = remote
            .env
            .iter()
            .find(|v| v.key == "Authorization")
            .unwrap();
        assert_eq!(auth.value, None);
        let local = one(
            r#"claude mcp add -e API_KEY=$KEY -e B="${KEY}" -e C='$KEY' -e D=price$ demo -- cmd"#,
        );
        assert_eq!(
            local.env.iter().find(|v| v.key == "API_KEY").unwrap().value,
            None
        );
        assert_eq!(local.env.iter().find(|v| v.key == "B").unwrap().value, None);
        assert_eq!(env(&local, "C").as_deref(), Some("$KEY"));
        assert_eq!(env(&local, "D").as_deref(), Some("price$"));
        // In the name, command or arguments there is nothing to ask for.
        for bad in [
            "claude mcp add demo -- node ~/srv/index.js",
            "claude mcp add demo -- ~/bin/server",
            "claude mcp add demo -- node $HOME/srv.js",
            r#"claude mcp add demo -- node "${HOME}/srv.js""#,
            "claude mcp add $NAME -- node srv.js",
            "claude mcp add -t http demo https://$HOST/mcp",
        ] {
            refused(bad);
        }
        // `$$` is the shell's process id.
        let pid = one("claude mcp add -e INSTANCE_ID=$$ -e LIT='$$' demo -- cmd");
        assert_eq!(
            pid.env
                .iter()
                .find(|v| v.key == "INSTANCE_ID")
                .unwrap()
                .value,
            None
        );
        assert_eq!(env(&pid, "LIT").as_deref(), Some("$$"));
        assert_eq!(
            super::parse("claude mcp add demo -- node /tmp/server-$$.js").err(),
            Some(super::PasteError::ShellVariable)
        );
        let quoted = one("claude mcp add demo -- node '~/srv.js' '$HOME' a~b cost$");
        assert_eq!(quoted.args, ["~/srv.js", "$HOME", "a~b", "cost$"]);
    }

    /// A paste is full of secrets and a message lands in toasts and their copy
    /// button. Put a secret-looking value in every position of every refused
    /// shape and check that no message repeats it, and that every kind of
    /// refusal is covered.
    #[test]
    fn refusal_messages_never_hold_pasted_text() {
        const S: &str = "sk-live-SECRET42";
        let shapes = [
            "claude mcp add {S} -- {S} '{S}",
            "claude mcp add {S} -- {S} \"{S}",
            "claude mcp add {S} -- {S} \\",
            "claude mcp add {S} -- {S}\n{S}",
            "claude mcp add {S} -- {S} # {S}\n{S}",
            "claude mcp add {S} -- {S}; {S}",
            "claude mcp add {S} -- {S} && {S}",
            "claude mcp add {S} -- {S} | {S}",
            "claude mcp add {S} -- {S} > {S}",
            "claude mcp add {S} -- {S} < {S}",
            "claude mcp add {S} -- ({S})",
            "claude mcp add {S} -- {S} $({S})",
            "claude mcp add {S} -- {S} `{S}`",
            "claude mcp add {S} -- {S} \"$({S})\"",
            "claude mcp add -e K=$({S}) {S} -- {S}",
            "claude mcp add {S} -- {S} $'{S}'",
            "claude mcp add --{S} {S} -- {S}",
            "claude mcp add --{S}={S} {S} -- {S}",
            "claude mcp add -x{S} {S} -- {S}",
            "claude mcp add -t http --client-id {S} {S} https://{S}.example",
            "claude mcp add -t http --client-id={S} {S} https://{S}.example",
            "claude mcp add -t http --client-secret {S} https://{S}.example",
            "claude mcp add -t http {S} https://{S}.example -e",
            "claude mcp add -e K={S} {S} -e",
            "claude mcp add -e {S} {S} -- {S}",
            "claude mcp add -e ={S} {S} -- {S}",
            "claude mcp add -t http -H {S} {S} https://{S}.example",
            "claude mcp add -t http -H \"X-{S}: {S}\n{S}\" {S} https://{S}.example",
            "claude mcp add -e K={S}",
            "claude mcp add -e K={S} {S}",
            "claude mcp add -e K={S} {S} -- ${S}",
            "claude mcp add -e K={S} {S} -- ~/{S}",
            "claude mcp add -t http {S} https://${S}.example",
            "claude mcp add -H 'X-{S}: {S}' {S} -- {S}",
            "claude mcp add -t http {S} https://{S}.example {S}",
            "claude mcp add -t http -e K={S} {S} https://{S}.example",
            "claude mcp add -t http {S} ftp://{S}.example",
            "claude mcp add -t {S} {S} {S}",
            "claude mcp add {S} -- {S}\0{S}",
            "claude mcp add -t http {S} https://{S}.example -H 'X-{S}: {S}'",
            "claude mcp add -t http {S} https://{S}.example -H 'Authorization: Basic {S}'",
        ];
        let mut kinds = std::collections::HashSet::new();
        for shape in shapes {
            let input = shape.replace("{S}", S);
            let error = match super::parse(&input) {
                Err(error) => error,
                Ok(parsed) => panic!("{input:?} parsed as {parsed:?}"),
            };
            kinds.insert(std::mem::discriminant(&error));
            for message in [
                error.message(),
                refused(&input),
                super::super::parse_snippet_for_user(&input)
                    .unwrap_err()
                    .message(),
            ] {
                assert!(!message.contains("SECRET42"), "{input:?}: {message}");
                assert!(!message.is_empty(), "{input:?}");
            }
        }
        assert_eq!(kinds.len(), 22, "every refusal kind is covered");
    }

    /// The other formats keep one opaque message: their parser errors can quote
    /// the pasted line.
    #[test]
    fn other_formats_keep_an_opaque_message() {
        for input in [
            r#"{"mcpServers": {"x": {"command": "sk-live-SECRET42", }"#,
            "[mcp_servers.x]\ncommand = \"sk-live-SECRET42",
            "mcp_servers:\n  x: [sk-live-SECRET42",
        ] {
            let message = super::super::parse_snippet_for_user(input)
                .unwrap_err()
                .message();
            assert_eq!(
                message,
                "Could not read the pasted config. Check its syntax and retry."
            );
        }
    }

    /// JSON, TOML and YAML share the header rule, and say why with fixed text.
    #[test]
    fn other_formats_refuse_headers_toolport_cannot_send() {
        let custom = super::PasteError::CustomHeader.message();
        for bad in [
            r#"{"mcpServers":{"d":{"type":"http","url":"https://x.example/mcp","headers":{"X-API-Key":"sk-live-SECRET42"}}}}"#,
            r#"{"mcpServers":{"d":{"type":"http","url":"https://x.example/mcp","headers":{"Authorization":"Basic sk-live-SECRET42"}}}}"#,
            r#"{"mcpServers":{"d":{"url":"https://x.example/mcp","requestInit":{"headers":{"X-Key":"sk-live-SECRET42"}}}}}"#,
            r#"{"mcp":{"d":{"type":"remote","url":"https://x.example/mcp","headers":{"X-Key":"sk-live-SECRET42"}}}}"#,
            "[mcp_servers.d]\nurl = \"https://x.example/mcp\"\nhttp_headers = { X-Key = \"sk-live-SECRET42\" }",
            "[mcp_servers.d]\nurl = \"https://x.example/mcp\"\nenv_http_headers = { Authorization = \"TOKEN\" }",
            "mcp_servers:\n  d:\n    url: https://x.example/mcp\n    headers:\n      X-Key: sk-live-SECRET42",
        ] {
            let error = super::super::parse_snippet_for_user(bad).unwrap_err().message();
            assert_eq!(error, custom, "{bad}");
            assert_eq!(super::super::parse_snippet(bad).unwrap_err(), custom, "{bad}");
        }
        // A bearer Authorization is carried, from every format that has headers.
        for good in [
            r#"{"mcpServers":{"d":{"type":"http","url":"https://x.example/mcp","headers":{"Authorization":"Bearer abc"}}}}"#,
            "[mcp_servers.d]\nurl = \"https://x.example/mcp\"\nhttp_headers = { Authorization = \"Bearer abc\" }",
            "mcp_servers:\n  d:\n    url: https://x.example/mcp\n    headers:\n      Authorization: Bearer abc",
        ] {
            let server = one(good);
            assert_eq!(env(&server, "Authorization").as_deref(), Some("Bearer abc"), "{good}");
        }
        // Environment variables on a remote server would go out as its bearer
        // token. An empty Authorization is asked for, as a shell variable is.
        let env_message = super::PasteError::EnvOnRemote.message();
        for bad in [
            r#"{"mcpServers":{"d":{"type":"http","url":"https://x.example/mcp","env":{"API_KEY":"sk-live-SECRET42"}}}}"#,
            "[mcp_servers.d]\nurl = \"https://x.example/mcp\"\n[mcp_servers.d.env]\nAPI_KEY = \"sk-live-SECRET42\"",
            r#"{"mcp":{"d":{"type":"remote","url":"https://x.example/mcp","environment":{"API_KEY":"sk-live-SECRET42"}}}}"#,
        ] {
            let error = super::super::parse_snippet_for_user(bad).unwrap_err().message();
            assert_eq!(error, env_message, "{bad}");
        }
        let blank = one(
            r#"{"mcpServers":{"d":{"type":"http","url":"https://x.example/mcp","headers":{"Authorization":""}}}}"#,
        );
        assert!(
            blank.env.iter().any(|v| v.key == "Authorization"),
            "{blank:?}"
        );
        // A YAML key JSON cannot hold refuses the headers rather than dropping them.
        let null_key = "mcp_servers:\n  d:\n    url: https://x.example/mcp\n    ~: x\n    headers:\n      Authorization: Bearer abc";
        assert_eq!(
            super::super::parse_snippet_for_user(null_key)
                .unwrap_err()
                .message(),
            custom
        );
        // Headers on a stdio server go to its environment, as before.
        let local = one(r#"{"mcpServers":{"d":{"command":"node","headers":{"X-Key":"v"}}}}"#);
        assert_eq!(env(&local, "X-Key").as_deref(), Some("v"));
    }

    #[test]
    fn escapes_that_look_like_bytes_stay_text() {
        let server = one(r#"claude mcp add name -- cmd \xff \377 "\xff" 'é' é 日本 "\😀""#);
        assert_eq!(
            server.args,
            ["xff", "377", r"\xff", r"é", "é", "日本", r"\😀"]
        );
    }

    #[test]
    fn very_long_input_parses_in_linear_time() {
        let long_arg = "a".repeat(200_000);
        let server = one(&format!("claude mcp add name -- cmd {long_arg}"));
        assert_eq!(server.args[0].len(), 200_000);

        let many = (0..20_000)
            .map(|n| format!("arg{n}"))
            .collect::<Vec<_>>()
            .join(" \\\n ");
        let started = std::time::Instant::now();
        let server = one(&format!("claude mcp add name -- cmd {many}"));
        assert_eq!(server.args.len(), 20_000);
        let envs = (0..5_000)
            .map(|n| format!("-e K{n}=v"))
            .collect::<Vec<_>>()
            .join(" ");
        let server = one(&format!("claude mcp add {envs} name -- cmd"));
        assert_eq!(server.env.len(), 5_000);
        refused(&format!("claude mcp add name -- '{}", "x".repeat(300_000)));
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }
}
