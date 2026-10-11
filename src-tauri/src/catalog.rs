//! MCP server catalog.
//!
//! Two layers, so users can add servers without hand-typing commands/URLs:
//!   1. A small hand-verified "popular" seed, bundled and offline.
//!   2. Live search against the official MCP Registry for the long tail.
//!
//! Both produce the same [`CatalogEntry`] shape, which the UI turns into a
//! registry server with one click - the existing auth flow then handles creds.
use crate::http_client::ResponseResultExt as _;

use crate::registry::{ArgBinding, ArgPart, LaunchConfig, LaunchInput};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const REGISTRY_SEARCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
const REGISTRY_URL: &str = "https://registry.modelcontextprotocol.io/v0.1/servers";

/// One addable server: enough to create a registry entry, plus display metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub name: String,
    pub description: String,
    /// "stdio" | "http" | "sse"
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<LaunchConfig>,
    pub url: Option<String>,
    /// Env-var names the server needs (treated as secrets when added).
    pub env_keys: Vec<String>,
    /// "curated" | "registry"
    pub source: String,
    pub homepage: Option<String>,
    /// Publishing namespace from the official registry id, e.g. `io.github.acme`
    /// for `io.github.acme/widget`. A provenance signal (who published it), not a
    /// cryptographic guarantee. `None` for curated/user entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    /// Curated grouping for the browse view (e.g. "Databases"). Empty for registry
    /// and user entries, which surface flat in search results.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub category: String,
    /// Direct link to where the user creates this server's credential (e.g. the
    /// provider's API-token page). Powers the guided "go get your creds" step in
    /// Collections (and the normal add flow). Curated entries only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_url: Option<String>,
    /// One-line hint on what credential to create (scopes, what to paste).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_hint: Option<String>,
    /// Placeholder text for the URL field when the server is self-hosted or
    /// needs a user-specific endpoint. When present, the catalog UI opens
    /// ServerDialog instead of immediate-add so the user can enter their URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_hint: Option<String>,
}

/// Browse-view grouping for a curated server, keyed by name. Keeps the verified
/// entry list itself untouched; the UI orders the sections, not the arm order here.
fn category_for(name: &str) -> &'static str {
    match name {
        "GitHub"
        | "Vercel"
        | "Sentry"
        | "Cloudflare Docs"
        | "AWS"
        | "Kubernetes"
        | "Linode"
        | "Vercel (Full API)"
        | "Cloudflare (Full API)"
        | "Clerk (Full API)" => "Code & infrastructure",
        "Supabase" | "Neon" | "PostgreSQL" | "MongoDB" | "Elasticsearch" | "Qdrant" | "Redis" => {
            "Databases"
        }
        "Context7" | "DeepWiki" | "Microsoft Learn" | "Hugging Face" | "OpenRouter"
        | "Parallel Search" | "Brave Search" | "Exa" | "Tavily" | "Perplexity" | "DataForSEO" => {
            "Search & knowledge"
        }
        "Firecrawl" | "Apify" | "Browserbase" => "Web & automation",
        "Stripe" | "Stripe (Full API)" | "Notion" | "Composio" | "Linear" | "Atlassian"
        | "Airtable" | "Todoist" | "Slack" | "Resend" | "Figma" | "Postiz" | "Twilio" | "n8n"
        | "Langfuse" | "Postman" | "Trello" => "Apps & productivity",
        "Filesystem"
        | "Fetch"
        | "Git"
        | "Playwright"
        | "Sequential Thinking"
        | "Memory"
        | "Time"
        | "Chrome DevTools" => "Local tools",
        _ => "",
    }
}

/// Where to create the credential for a curated server, and a one-line hint, for
/// the guided "go get your creds" step in Collections. Returns `(url, hint)`; an empty
/// `url` means there's no single page (a connection string you supply, OAuth, or
/// no auth) so only the hint shows. `None` = unknown / no guidance.
fn credentials_for(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        // Toolport Full-API overlays: a provider API key the overlay uses locally.
        "Stripe (Full API)" => (
            "https://dashboard.stripe.com/apikeys",
            "Create a secret or restricted API key with the access the agent needs.",
        ),
        "Vercel (Full API)" => (
            "https://vercel.com/account/tokens",
            "Create an access token (Account Settings > Tokens).",
        ),
        "Cloudflare (Full API)" => (
            "https://dash.cloudflare.com/profile/api-tokens",
            "Create an API token scoped to the zones and services the agent should touch.",
        ),
        "Clerk (Full API)" => (
            "https://dashboard.clerk.com/last-active?path=api-keys",
            "Copy your Secret Key (Clerk Dashboard > API Keys).",
        ),
        // Token-based: the agent gets an API key Toolport vaults.
        "Linode" => (
            "https://cloud.linode.com/profile/tokens",
            "Create a Personal Access Token with read/write on the resources you need (Linodes, Volumes, Databases).",
        ),
        "AWS" => (
            "https://console.aws.amazon.com/iam/home#/security_credentials",
            "Create an access key (ID + secret) for an IAM user scoped to what the agent should touch.",
        ),
        "MongoDB" => (
            "https://cloud.mongodb.com",
            "Paste your MongoDB connection string (Atlas: Database > Connect > Drivers).",
        ),
        "Exa" => ("https://dashboard.exa.ai/api-keys", "Create an API key."),
        "Perplexity" => (
            "https://www.perplexity.ai/settings/api",
            "Create an API key (needs a small credit balance).",
        ),
        "DataForSEO" => (
            "https://app.dataforseo.com/api-access",
            "Copy your API login and password from the API Access dashboard.",
        ),
        "OpenRouter" => ("https://openrouter.ai/keys", "Create an API key."),
        "Qdrant" => (
            "https://cloud.qdrant.io",
            "Create a free cluster, then copy its URL and an API key (Cluster > API Keys).",
        ),
        "Redis" => (
            "",
            "Enter a redis:// or rediss:// connection URL in Launch setup. It is vaulted and passed as one argument.",
        ),
        "Hugging Face" => (
            "https://huggingface.co/settings/tokens",
            "Authenticate when prompted, or paste a read token.",
        ),
        "Resend" => ("https://resend.com/api-keys", "Create an API key with send access."),
        "Figma" => (
            "https://www.figma.com/settings",
            "Create a personal access token (Settings > Security > Personal access tokens).",
        ),
        "Slack" => (
            "https://api.slack.com/apps",
            "Create a Slack app, add a bot token (xoxb-...), and grab your team id.",
        ),
        "Twilio" => (
            "https://console.twilio.com",
            "Copy your Account SID, API Key SID, and API Secret from the Twilio Console.",
        ),
        "Postiz" => (
            "https://postiz.pro/settings/developers",
            "Create an API key in Settings > Developers > Public API, then paste it as the server's auth token.",
        ),
        "Langfuse" => (
            "https://langfuse.com/docs/api-and-data-platform/features/mcp-server",
            "Use your instance's /api/public/mcp URL. Paste Basic followed by base64(public-key:secret-key) as the server's auth token.",
        ),
        // Config you supply (no single token page).
        "PostgreSQL" => (
            "",
            "Enter your Postgres connection URL in Launch setup. It is vaulted and passed as one argument.",
        ),
        "Kubernetes" => ("", "Uses your local kubeconfig (~/.kube/config); nothing to paste."),
        "Filesystem" => (
            "",
            "Enter one allowed directory in Launch setup. Add more directories as literal arguments if needed.",
        ),
        // OAuth: authorize in the browser, no manual token.
        "GitHub" | "Vercel" | "Sentry" | "Notion" | "Linear" | "Stripe" | "Postman" | "Trello" => (
            "",
            "OAuth: click Authenticate when prompted; no manual token needed.",
        ),
        // No auth at all.
        "Fetch" | "Context7" | "Microsoft Learn" => ("", "No credential needed."),
        _ => return None,
    })
}

/// The hand-verified popular set. Hosted (URL) servers are favored here because
/// their endpoints are far more stable than package names. The live registry
/// covers the long tail; this set is what most people reach for first.
pub fn curated() -> Vec<CatalogEntry> {
    // Remote servers, keyed by transport. Most hosted MCPs are streamable-http;
    // a few still use SSE endpoints.
    let http = |name: &str, desc: &str, url: &str, home: &str| CatalogEntry {
        name: name.to_string(),
        description: desc.to_string(),
        transport: "http".to_string(),
        command: None,
        args: vec![],
        launch: None,
        url: Some(url.to_string()),
        env_keys: vec![],
        source: "curated".to_string(),
        homepage: Some(home.to_string()),
        publisher: None,
        category: String::new(),
        credentials_url: None,
        setup_hint: None,
        url_hint: None,
    };
    // Self-hosted server: the user supplies the URL (shown as placeholder).
    // transport is http because the server speaks MCP over HTTP, but the URL
    // is None — the catalog UI opens ServerDialog so the user enters their
    // instance endpoint before the server is created.
    let self_hosted = |name: &str, desc: &str, url_hint: &str, home: &str| CatalogEntry {
        name: name.to_string(),
        description: desc.to_string(),
        transport: "http".to_string(),
        command: None,
        args: vec![],
        launch: None,
        url: None,
        env_keys: vec![],
        source: "curated".to_string(),
        homepage: Some(home.to_string()),
        publisher: None,
        category: String::new(),
        credentials_url: None,
        setup_hint: None,
        url_hint: Some(url_hint.to_string()),
    };
    // Local (stdio) servers: `command` + args, with any required secret env keys.
    let cmd = |name: &str, desc: &str, command: &str, args: &[&str], env: &[&str], home: &str| {
        CatalogEntry {
            name: name.to_string(),
            description: desc.to_string(),
            transport: "stdio".to_string(),
            command: Some(command.to_string()),
            args: args.iter().map(|s| s.to_string()).collect(),
            launch: None,
            url: None,
            env_keys: env.iter().map(|s| s.to_string()).collect(),
            source: "curated".to_string(),
            homepage: Some(home.to_string()),
            publisher: None,
            category: String::new(),
            credentials_url: None,
            setup_hint: None,
            url_hint: None,
        }
    };

    let mut list = vec![
        // --- Payments & commerce ---
        http("Stripe", "Payments, customers, charges, and balances.", "https://mcp.stripe.com", "https://docs.stripe.com/mcp"),
        cmd("Stripe (Full API)", "Toolport overlay: all 587 Stripe endpoints as intent-named tools, with the full write coverage the official MCP lacks (your API key, runs locally).", "npx", &["-y", "toolport-mcp-servers@0.3.0", "stripe"], &["STRIPE_API_KEY"], "https://github.com/btsouth/toolport-mcp-servers"),
        // --- Code, deploy & infra ---
        http("GitHub", "Repos, issues, PRs, and code search.", "https://api.githubcopilot.com/mcp/", "https://github.com/github/github-mcp-server"),
        http("Vercel", "Projects, deployments, and logs on Vercel.", "https://mcp.vercel.com", "https://vercel.com/docs/mcp/vercel-mcp"),
        cmd("Vercel (Full API)", "Toolport overlay: 333 Vercel endpoints including the writes the official MCP omits (env vars, domains/DNS, the deploy lifecycle).", "npx", &["-y", "toolport-mcp-servers@0.3.0", "vercel"], &["VERCEL_TOKEN"], "https://github.com/btsouth/toolport-mcp-servers"),
        http("Sentry", "Errors, issues, and releases from Sentry.", "https://mcp.sentry.dev/mcp", "https://docs.sentry.io"),
        http("Cloudflare Docs", "Search Cloudflare's documentation.", "https://docs.mcp.cloudflare.com/mcp", "https://developers.cloudflare.com/agents/model-context-protocol/"),
        cmd("Cloudflare (Full API)", "Toolport overlay: 357 Cloudflare control-plane endpoints as named tools (DNS, email routing, zones, WAF, SSL, cache, R2, Access) for per-tool approval.", "npx", &["-y", "toolport-mcp-servers@0.3.0", "cloudflare"], &["CLOUDFLARE_API_TOKEN"], "https://github.com/btsouth/toolport-mcp-servers"),
        cmd("Clerk (Full API)", "Toolport overlay: 224 Clerk Backend API endpoints (users, orgs, sessions, invitations), vs the official 2-tool docs server.", "npx", &["-y", "toolport-mcp-servers@0.3.0", "clerk"], &["CLERK_SECRET_KEY"], "https://github.com/btsouth/toolport-mcp-servers"),
        cmd("AWS", "AWS service APIs through the AWS Labs API MCP server.", "uvx", &["awslabs.aws-api-mcp-server@1.5.6"], &["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"], "https://github.com/awslabs/mcp"),
        cmd("Kubernetes", "Inspect and manage Kubernetes clusters via your kubeconfig.", "npx", &["-y", "mcp-server-kubernetes@4.1.9"], &[], "https://github.com/Flux159/mcp-server-kubernetes"),
        cmd("Linode", "Manage Linode (Akamai) cloud: instances, volumes, NodeBalancers, databases, and networking.", "npx", &["-y", "@takashito/linode-mcp-server@0.4.0"], &["LINODE_API_TOKEN"], "https://github.com/takashito/linode-mcp-server"),
        cmd("Chrome DevTools", "Control and inspect a live Chrome browser: traces, screenshots, network, console.", "npx", &["-y", "chrome-devtools-mcp@1.10.1"], &[], "https://github.com/ChromeDevTools/chrome-devtools-mcp"),
        // --- Databases ---
        http("Supabase", "Query and manage your Supabase projects.", "https://mcp.supabase.com/mcp", "https://supabase.com/docs/guides/getting-started/mcp"),
        http("Neon", "Serverless Postgres: branches, queries, projects.", "https://mcp.neon.tech/mcp", "https://neon.tech/docs/ai/neon-mcp-server"),
        cmd("PostgreSQL", "Query a Postgres database (requires a connection URL).", "npx", &["-y", "@modelcontextprotocol/server-postgres@0.6.2", "<launch-input>"], &[], "https://github.com/modelcontextprotocol/servers-archived"),
        cmd("MongoDB", "Query and manage MongoDB databases.", "npx", &["-y", "mongodb-mcp-server@3.0.5"], &["MDB_MCP_CONNECTION_STRING"], "https://github.com/mongodb-js/mongodb-mcp-server"),
        cmd("Elasticsearch", "Search and analytics over your Elasticsearch cluster.", "npx", &["-y", "@elastic/mcp-server-elasticsearch@0.3.1"], &["ES_URL", "ES_API_KEY"], "https://github.com/elastic/mcp-server-elasticsearch"),
        cmd("Qdrant", "Vector search and memory for RAG: store and query embeddings in Qdrant.", "uvx", &["mcp-server-qdrant@0.8.1"], &["QDRANT_URL", "QDRANT_API_KEY", "COLLECTION_NAME"], "https://github.com/qdrant/mcp-server-qdrant"),
        cmd("Redis", "Inspect and manage a Redis database.", "uvx", &["--from", "redis-mcp-server==0.5.1", "redis-mcp-server", "--url", "<launch-input>"], &[], "https://github.com/redis/mcp-redis"),
        // --- Project management & docs ---
        http("Notion", "Search and edit Notion pages and databases.", "https://mcp.notion.com/mcp", "https://developers.notion.com"),
        http("Postman", "Manage Postman workspaces, collections, and environments.", "https://mcp.postman.com/minimal", "https://github.com/postmanlabs/postman-mcp-server"),
        http("Composio", "Connect AI agents to 1,000+ apps (Gmail, Slack, GitHub, Notion, Linear, and more).", "https://connect.composio.dev/mcp", "https://composio.dev"),
        http("Trello", "Boards, lists, cards, checklists, and workspace search.", "https://mcp.trello.com/v1", "https://github.com/atlassian/trello-mcp-server"),
        http("Linear", "Issues, projects, and cycles in Linear.", "https://mcp.linear.app/mcp", "https://linear.app/docs"),
        http("Atlassian", "Jira issues and Confluence pages.", "https://mcp.atlassian.com/v2/mcp?tools=all", "https://support.atlassian.com/atlassian-ai-gateway/docs/get-started-with-the-atlassian-remote-mcp-server/"),
        cmd("Airtable", "Read and write records in your Airtable bases.", "npx", &["-y", "airtable-mcp-server@1.14.0"], &["AIRTABLE_API_KEY"], "https://github.com/domdomegg/airtable-mcp-server"),
        cmd("Todoist", "Manage Todoist tasks and projects.", "npx", &["-y", "@abhiz123/todoist-mcp-server@0.1.0"], &["TODOIST_API_TOKEN"], "https://github.com/abhiz123/todoist-mcp-server"),
        // --- Communication ---
        cmd("Slack", "Read and send Slack messages and manage channels.", "npx", &["-y", "@modelcontextprotocol/server-slack@2025.4.25"], &["SLACK_BOT_TOKEN", "SLACK_TEAM_ID"], "https://github.com/modelcontextprotocol/servers"),
        cmd("Twilio", "Send SMS, make calls, and manage Twilio messaging and voice.", "npx", &["-y", "@twilio-alpha/mcp@0.7.0", "<launch-input>"], &[], "https://github.com/twilio-labs/mcp"),
        http("Postiz", "Schedule and publish social media posts across platforms.", "https://mcp.postiz.com/mcp", "https://docs.postiz.com/mcp/setup"),
        // --- Knowledge & search ---
        http("Context7", "Up-to-date docs and code examples for libraries.", "https://mcp.context7.com/mcp", "https://github.com/upstash/context7"),
        http("DeepWiki", "Ask questions about any public GitHub repo. No auth.", "https://mcp.deepwiki.com/mcp", "https://deepwiki.com"),
        http("Microsoft Learn", "Search official Microsoft and Azure documentation and code samples. No auth.", "https://learn.microsoft.com/api/mcp", "https://learn.microsoft.com/en-us/training/support/mcp"),
        http("Hugging Face", "Models, datasets, and Spaces on Hugging Face.", "https://huggingface.co/mcp", "https://huggingface.co/settings/mcp"),
        http("OpenRouter", "Live model intelligence: list and compare models, prices, and your credits.", "https://mcp.openrouter.ai/mcp", "https://openrouter.ai/blog/announcements/openrouter-mcp-server/"),
        http("Parallel Search", "Live web search and clean content from URLs. No account or API key required.", "https://search.parallel.ai/mcp", "https://docs.parallel.ai/integrations/mcp/search-mcp"),
        cmd("Brave Search", "Web search via the Brave Search API.", "npx", &["-y", "@brave/brave-search-mcp-server@2.1.4"], &["BRAVE_API_KEY"], "https://github.com/brave/brave-search-mcp-server"),
        cmd("Exa", "AI-native web search built for agents.", "npx", &["-y", "exa-mcp-server@3.4.2"], &["EXA_API_KEY"], "https://github.com/exa-labs/exa-mcp-server"),
        cmd("Tavily", "Web search and content extraction built for LLMs.", "npx", &["-y", "tavily-mcp@0.2.22"], &["TAVILY_API_KEY"], "https://github.com/tavily-ai/tavily-mcp"),
        cmd("Perplexity", "Ask Perplexity for cited, up-to-date answers.", "npx", &["-y", "@perplexity-ai/mcp-server@1.3.0"], &["PERPLEXITY_API_KEY"], "https://github.com/perplexityai/modelcontextprotocol"),
        cmd("DataForSEO", "SEO data: SERP tracking, keyword research, and competitor analysis.", "npx", &["-y", "dataforseo-mcp-server@3.1.3"], &["DATAFORSEO_USERNAME", "DATAFORSEO_PASSWORD"], "https://dataforseo.com"),
        cmd("Firecrawl", "Web scraping and data extraction from websites.", "npx", &["-y", "firecrawl-mcp@3.28.2"], &["FIRECRAWL_API_KEY"], "https://github.com/firecrawl/firecrawl-mcp-server"),
        cmd("Apify", "Run Apify actors for web scraping and automation.", "npx", &["-y", "@apify/actors-mcp-server@0.17.4"], &["APIFY_TOKEN"], "https://github.com/apify/actors-mcp-server"),
        cmd("Browserbase", "Cloud headless browsers agents can drive.", "npx", &["-y", "@browserbasehq/mcp@3.0.0"], &["BROWSERBASE_API_KEY", "BROWSERBASE_PROJECT_ID", "GEMINI_API_KEY"], "https://github.com/browserbase/mcp-server-browserbase"),
        // --- Email & comms already above; Design ---
        cmd("Figma", "Turn Figma designs into code (Framelink).", "npx", &["-y", "figma-developer-mcp@0.13.2", "--stdio"], &["FIGMA_API_KEY"], "https://github.com/GLips/Figma-Context-MCP"),
        // --- Email ---
        cmd("Resend", "Send transactional email through Resend.", "npx", &["-y", "resend-mcp@2.25.0"], &["RESEND_API_KEY"], "https://resend.com/docs"),
        // --- Self-hosted (user supplies URL) ---
        self_hosted("n8n", "Trigger, manage, and edit n8n workflows via MCP.", "https://your-instance.com/mcp-server/http", "https://n8n.io"),
        self_hosted("Langfuse", "Prompt management and observability. Use /api/public/mcp with Basic auth; see setup docs.", "https://your-langfuse.com/api/public/mcp", "https://langfuse.com/docs/api-and-data-platform/features/mcp-server"),
        // --- Local utilities (no account needed) ---
        cmd("Filesystem", "Read and write files in directories you allow.", "npx", &["-y", "@modelcontextprotocol/server-filesystem@2026.8.31", "<launch-input>"], &[], "https://github.com/modelcontextprotocol/servers"),
        cmd("Fetch", "Fetch a URL and return its content as markdown.", "uvx", &["mcp-server-fetch@2026.8.18"], &[], "https://github.com/modelcontextprotocol/servers"),
        cmd("Git", "Read, search, and manipulate a local Git repo.", "uvx", &["mcp-server-git@2026.8.18"], &[], "https://github.com/modelcontextprotocol/servers"),
        cmd("Playwright", "Drive a real browser for testing and scraping.", "npx", &["-y", "@playwright/mcp@0.0.83"], &[], "https://github.com/microsoft/playwright-mcp"),
        cmd("Sequential Thinking", "Structured step-by-step reasoning for hard problems.", "npx", &["-y", "@modelcontextprotocol/server-sequential-thinking@2026.8.31"], &[], "https://github.com/modelcontextprotocol/servers"),
        cmd("Memory", "A knowledge graph the agent reads and writes across sessions.", "npx", &["-y", "@modelcontextprotocol/server-memory@2026.8.31"], &[], "https://github.com/modelcontextprotocol/servers"),
        cmd("Time", "Current time and timezone conversions.", "uvx", &["mcp-server-time@2026.8.18"], &[], "https://github.com/modelcontextprotocol/servers"),
    ];
    for e in &mut list {
        let template = e
            .name
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>();
        let mut launch = LaunchConfig {
            template: Some(template),
            revision: Some(1),
            ..Default::default()
        };
        let mut add_input = |key: &str, label: &str, secret: bool| {
            launch.inputs.push(LaunchInput {
                key: key.into(),
                label: label.into(),
                secret,
                required: true,
                value: None,
                unknown_fields: Default::default(),
            });
        };
        match e.name.as_str() {
            "Twilio" => {
                add_input("TWILIO_ACCOUNT_SID", "Account SID", false);
                add_input("TWILIO_API_KEY", "API Key SID", true);
                add_input("TWILIO_API_SECRET", "API Secret", true);
                launch.bindings.push(ArgBinding {
                    index: 2,
                    parts: vec![
                        ArgPart::Input {
                            key: "TWILIO_ACCOUNT_SID".into(),
                            unknown_fields: Default::default(),
                        },
                        ArgPart::Literal {
                            value: "/".into(),
                            unknown_fields: Default::default(),
                        },
                        ArgPart::Input {
                            key: "TWILIO_API_KEY".into(),
                            unknown_fields: Default::default(),
                        },
                        ArgPart::Literal {
                            value: ":".into(),
                            unknown_fields: Default::default(),
                        },
                        ArgPart::Input {
                            key: "TWILIO_API_SECRET".into(),
                            unknown_fields: Default::default(),
                        },
                    ],
                    unknown_fields: Default::default(),
                });
                launch.revision = Some(2);
            }
            "PostgreSQL" => {
                add_input("POSTGRES_URL", "Postgres connection URL", true);
                launch.bindings.push(ArgBinding {
                    index: 2,
                    parts: vec![ArgPart::Input {
                        key: "POSTGRES_URL".into(),
                        unknown_fields: Default::default(),
                    }],
                    unknown_fields: Default::default(),
                });
                launch.revision = Some(2);
            }
            "Redis" => {
                add_input("REDIS_URL", "Redis connection URL", true);
                launch.bindings.push(ArgBinding {
                    index: 4,
                    parts: vec![ArgPart::Input {
                        key: "REDIS_URL".into(),
                        unknown_fields: Default::default(),
                    }],
                    unknown_fields: Default::default(),
                });
            }
            "Filesystem" => {
                add_input("ALLOWED_DIRECTORY", "Allowed directory", false);
                launch.bindings.push(ArgBinding {
                    index: 2,
                    parts: vec![ArgPart::Input {
                        key: "ALLOWED_DIRECTORY".into(),
                        unknown_fields: Default::default(),
                    }],
                    unknown_fields: Default::default(),
                });
                launch.revision = Some(2);
            }
            "Browserbase" => {
                launch.required_env = vec![
                    "BROWSERBASE_API_KEY".into(),
                    "BROWSERBASE_PROJECT_ID".into(),
                    "GEMINI_API_KEY".into(),
                ];
                launch.revision = Some(2);
            }
            "Qdrant" => {
                // The tools accept a collection name per call when no default
                // is configured. QDRANT_API_KEY is also optional for local or
                // unsecured clusters; this preset uses a URL connection.
                launch.required_env = vec!["QDRANT_URL".into()];
                launch.revision = Some(2);
            }
            "AWS" => launch.revision = Some(2),
            "Atlassian" => launch.revision = Some(2),
            "Perplexity" | "Brave Search" => launch.revision = Some(2),
            _ => {}
        }
        // These stdio packages cannot authenticate or complete setup without
        // their declared keys. Optional/alternative credentials (AWS's provider
        // chain, MongoDB's connect-later flow, Qdrant's API key) stay optional.
        if matches!(
            e.name.as_str(),
            "Stripe (Full API)"
                | "Vercel (Full API)"
                | "Cloudflare (Full API)"
                | "Clerk (Full API)"
                | "Linode"
                | "Elasticsearch"
                | "Airtable"
                | "Todoist"
                | "Slack"
                | "Brave Search"
                | "Exa"
                | "Tavily"
                | "Perplexity"
                | "DataForSEO"
                | "Firecrawl"
                | "Apify"
                | "Figma"
                | "Resend"
        ) {
            launch.required_env = e.env_keys.clone();
        }
        e.launch = Some(launch);
        e.category = category_for(&e.name).to_string();
        if let Some((url, hint)) = credentials_for(&e.name) {
            e.credentials_url = (!url.is_empty()).then(|| url.to_string());
            e.setup_hint = Some(hint.to_string());
        }
    }
    list
}

/// The popular set shown by default: the curated catalog.
pub fn popular() -> Vec<CatalogEntry> {
    curated()
}

/// Filter a catalog list by a query (name, description, or category). Substring
/// match, plus an all-terms fallback so multi-word queries still hit. Empty query
/// = all.
fn filter_catalog(list: Vec<CatalogEntry>, query: &str) -> Vec<CatalogEntry> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return list;
    }
    let terms: Vec<&str> = q.split_whitespace().collect();
    list.into_iter()
        .filter(|e| {
            let hay = format!(
                "{} {} {}",
                e.name.to_lowercase(),
                e.description.to_lowercase(),
                e.category.to_lowercase()
            );
            hay.contains(&q) || terms.iter().all(|t| hay.contains(t))
        })
        .collect()
}

/// Popular entries (user + curated) matching a query.
pub fn search_curated(query: &str) -> Vec<CatalogEntry> {
    filter_catalog(popular(), query)
}

/// Live search is optional; its failure never discards bundled matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RegistryStatus {
    NotQueried,
    Available,
    Unavailable,
    TimedOut,
}

impl RegistryStatus {
    pub fn notice(self) -> Option<&'static str> {
        match self {
            Self::Unavailable => {
                Some("The live MCP Registry is unavailable. Showing curated matches only.")
            }
            Self::TimedOut => Some(
                "The live MCP Registry took too long to respond. Showing curated matches only.",
            ),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSearch {
    pub entries: Vec<CatalogEntry>,
    pub registry_status: RegistryStatus,
}

pub fn search(query: &str) -> CatalogSearch {
    search_with_registry(query, search_registry)
}

pub(crate) fn search_with_registry(
    query: &str,
    live: impl FnOnce(&str) -> Result<Vec<CatalogEntry>, RegistryStatus>,
) -> CatalogSearch {
    let mut entries = search_curated(query);
    let mut seen: std::collections::HashSet<String> =
        entries.iter().filter_map(entry_identity).collect();
    let registry_status = if query.trim().is_empty() {
        RegistryStatus::NotQueried
    } else {
        match live(query) {
            Ok(live_entries) => {
                for entry in live_entries {
                    // Labels are not identities: keep same-name different servers.
                    if entry_identity(&entry).is_none_or(|identity| seen.insert(identity)) {
                        entries.push(entry);
                    }
                }
                RegistryStatus::Available
            }
            Err(status) => status,
        }
    };
    rank_search_results(&mut entries, query);
    CatalogSearch {
        entries,
        registry_status,
    }
}

pub fn entry_identity(entry: &CatalogEntry) -> Option<String> {
    server_identity(
        &entry.transport,
        entry.command.as_deref(),
        &entry.args,
        entry.url.as_deref(),
    )
}

/// Ignore labels, credentials and package versions, retaining launch arguments
/// and endpoint selectors so different packages/instances do not collapse.
pub fn server_identity(
    transport: &str,
    command: Option<&str>,
    args: &[String],
    endpoint: Option<&str>,
) -> Option<String> {
    if transport != "stdio" {
        let mut url = url::Url::parse(endpoint?.trim()).ok()?;
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        url.set_username("").ok()?;
        url.set_password(None).ok()?;
        url.set_fragment(None);
        let mut query: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| !secret_query_key(key))
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        query.sort();
        url.set_query(None);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let path = url.path().trim_end_matches('/').to_string();
        url.set_path(if path.is_empty() { "/" } else { &path });
        return Some(format!("remote:{}", url));
    }
    let command = command?.trim();
    if command.is_empty() {
        return None;
    }
    let runner = command
        .rsplit(['/', '\\'])
        .next()?
        .trim_end_matches(".cmd")
        .trim_end_matches(".exe");
    let mut args = args.to_vec();
    let command = if matches!(runner, "npx" | "uvx") {
        if runner == "npx"
            && args
                .first()
                .is_some_and(|arg| matches!(arg.as_str(), "-y" | "--yes"))
        {
            args.remove(0);
        }
        let index = usize::from(runner == "uvx" && args.first().is_some_and(|arg| arg == "--from"));
        if let Some(spec) = args.get_mut(index) {
            if runner == "npx" {
                if let Some((name, _)) = spec.rsplit_once('@').filter(|(name, _)| !name.is_empty())
                {
                    *spec = name.to_string();
                }
            } else {
                *spec = spec
                    .split(['@', '='])
                    .next()
                    .unwrap_or(spec)
                    .replace('_', "-")
                    .to_ascii_lowercase();
            }
        }
        runner
    } else if runner == "docker" && args.first().is_some_and(|arg| arg == "run") {
        let value_flags = [
            "-e",
            "--env",
            "--env-file",
            "-v",
            "--volume",
            "-p",
            "--publish",
            "--name",
            "--network",
            "--entrypoint",
            "-w",
            "--workdir",
            "-u",
            "--user",
            "--mount",
        ];
        let mut index = 1;
        while args.get(index).is_some_and(|arg| arg.starts_with('-')) {
            index += if value_flags.contains(&args[index].as_str()) {
                2
            } else {
                1
            };
        }
        if let Some(image) = args.get_mut(index) {
            *image = image.split("@sha256:").next().unwrap().to_string();
            if let Some(colon) = image.rfind(':') {
                if image.rfind('/').is_none_or(|slash| colon > slash) {
                    image.truncate(colon);
                }
            }
        }
        runner
    } else {
        command
    };
    Some(serde_json::to_string(&(command, args)).expect("launch identity"))
}

pub fn installed_entry_identity(entry: &CatalogEntry) -> Option<String> {
    entry_identity(entry)
        .or_else(|| (entry.source == "curated").then(|| format!("curated:{}", entry.name)))
}

pub fn installed_server_identities(server: &crate::registry::ServerEntry) -> Vec<String> {
    let mut identities: Vec<_> = server_identity(
        &server.transport,
        server.command.as_deref(),
        &server.args,
        server.url.as_deref(),
    )
    .into_iter()
    .collect();
    if server.source.as_deref() == Some("catalog:curated") {
        identities.push(format!("curated:{}", server.name));
    }
    identities
}

fn secret_query_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "token"
            | "access_token"
            | "api_key"
            | "apikey"
            | "key"
            | "secret"
            | "client_secret"
            | "password"
            | "auth"
            | "authorization"
            | "sig"
            | "signature"
            | "x-api-key"
            | "credential"
            | "credentials"
            | "api-key"
    )
}

fn rank_search_results(entries: &mut [CatalogEntry], query: &str) {
    let query = query.trim().to_lowercase();
    entries.sort_by(|a, b| {
        catalog_match_score(b, &query)
            .cmp(&catalog_match_score(a, &query))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

fn catalog_match_score(entry: &CatalogEntry, query: &str) -> i32 {
    let name = entry.name.to_lowercase();
    let description = entry.description.to_lowercase();
    let category = entry.category.to_lowercase();
    let mut score = if entry.source == "curated" { 10_000 } else { 0 };
    if name == query {
        score += 5_000;
    } else if name.starts_with(query) {
        score += 3_000;
    } else if name.contains(query) {
        score += 2_000;
    } else if query.split_whitespace().all(|term| name.contains(term)) {
        score += 1_500;
    } else if category.contains(query) {
        score += 800;
    } else if description.contains(query) {
        score += 500;
    }
    if entry.url.is_some() {
        score += 100;
    }
    if entry.homepage.is_some() {
        score += 25;
    }
    score
}

/// npm/yarn/pnpm remote specs that `npx -y` will fetch outside the registry.
/// `:` itself stays allowed so docker `host:port/image` and OCI `image:tag` work.
fn is_remote_package_spec(spec: &str) -> bool {
    let lower = spec.to_ascii_lowercase();
    lower.contains("://")
        || lower.starts_with("github:")
        || lower.starts_with("gitlab:")
        || lower.starts_with("bitbucket:")
        || lower.starts_with("gist:")
        || lower.starts_with("git+")
        || lower.starts_with("npm:")
        || lower.starts_with("jsr:")
        || lower.starts_with("file:")
        || lower.starts_with("http:")
        || lower.starts_with("https:")
}

/// npm-only: specs that make `npx -y` fetch code from somewhere other than the named
/// registry, in the spellings npm accepts BEYOND a leading protocol.
///
/// [`is_remote_package_spec`] only inspects the start of the combined
/// `identifier@version` string, so every form here reached `npx -y` from a malicious or
/// MITM'd registry entry: `attacker/payload` (npm reads an unscoped slash name as
/// GitHub shorthand), `lodash@github:attacker/payload` (a hosted-git alias as the
/// version range), `github.com/attacker/payload`, and `git@github.com:attacker/x.git`.
///
/// Applied on the npm path only. `user/image:tag` is a normal Docker reference, so the
/// slash rule cannot be global without dropping valid OCI entries.
fn npm_spec_escapes_registry(spec: &str) -> bool {
    let lower = spec.to_ascii_lowercase();
    // SCP-style git, which carries no `://` for is_remote_package_spec to catch.
    if lower.starts_with("git@") {
        return true;
    }
    // A hosted-git alias ANYWHERE, not just as a prefix: npm resolves it from the
    // version half too.
    if ["github:", "gitlab:", "bitbucket:", "gist:", "git+"]
        .iter()
        .any(|alias| lower.contains(alias))
    {
        return true;
    }
    // Hosted URLs with the scheme left off.
    if ["github.com/", "gitlab.com/", "bitbucket.org/"]
        .iter()
        .any(|host| lower.starts_with(host) || lower.contains(&format!("@{host}")))
    {
        return true;
    }
    // An unscoped `user/repo` is GitHub shorthand to npm, while `@scope/name` is a
    // real package. Judge the NAME half: a scoped name's own `@` must not be mistaken
    // for the version separator.
    let name = match lower.strip_prefix('@') {
        Some(rest) => match rest.find('@') {
            Some(i) => &lower[..=i],
            None => lower.as_str(),
        },
        None => lower.split('@').next().unwrap_or(&lower),
    };
    !name.starts_with('@') && name.contains('/')
}

/// A registry package spec safe to pass as an npx/uvx/docker argument: non-empty, no
/// leading dash (flag injection), bounded length, and only the characters real
/// package names use. Nothing that could become a separate flag, a shell token,
/// or a github:/URL install that bypasses the named registry.
fn is_safe_package_id(spec: &str) -> bool {
    !spec.is_empty()
        && !spec.starts_with('-')
        && spec.len() <= 200
        && spec.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '@' | '/' | '-' | '_' | '.' | '+' | ':')
        })
        && !is_remote_package_spec(spec)
}

/// Turn one registry `server` object into a catalog entry. Prefers a hosted
/// remote (simplest to connect), else the first installable package.
fn map_server(server: &Value) -> Option<CatalogEntry> {
    let id = server.get("name").and_then(|v| v.as_str()).unwrap_or("");
    // Friendly title when present; fall back to the namespaced id.
    let name = server
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(id)
        .to_string();
    if name.is_empty() {
        return None;
    }
    let description = server
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let homepage = server
        .get("repository")
        .and_then(|r| r.get("url"))
        .and_then(|v| v.as_str())
        // SECURITY: this flows to the OS opener in the UI. A registry entry could set
        // it to file:// (Windows SMB -> NTLM-hash leak) or a custom-scheme handler
        // URI; only hand real web links onward. Mirrors the remotes[].url guard below.
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .map(String::from);
    // Registry ids are namespaced (`io.github.acme/widget`); the namespace tells
    // you who published it - a provenance signal we surface in the catalog.
    let publisher = id.split_once('/').map(|(ns, _)| ns.to_string());

    if let Some(remote) = server
        .get("remotes")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
    {
        if let Some(url) = remote.get("url").and_then(|v| v.as_str()) {
            // SECURITY: only real HTTP(S) endpoints from a registry entry; a
            // javascript:/file:/data: URL has no business here.
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return None;
            }
            let ty = remote.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let transport = if ty.contains("sse") { "sse" } else { "http" };
            return Some(CatalogEntry {
                name,
                description,
                transport: transport.to_string(),
                command: None,
                args: vec![],
                launch: None,
                url: Some(url.to_string()),
                env_keys: vec![],
                source: "registry".to_string(),
                homepage,
                publisher,
                category: String::new(),
                credentials_url: None,
                setup_hint: None,
                url_hint: None,
            });
        }
    }

    if let Some(pkg) = server
        .get("packages")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
    {
        let registry_type = pkg
            .get("registryType")
            .or_else(|| pkg.get("registry_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("npm");
        let identifier = pkg.get("identifier").and_then(|v| v.as_str()).unwrap_or("");
        if identifier.is_empty() {
            return None;
        }
        let version = pkg.get("version").and_then(|v| v.as_str());
        let spec = match version {
            Some(v) if !v.is_empty() && v != "latest" => {
                // npm/PyPI pin with `@`. Docker treats `@` as a content digest
                // (`image@sha256:…`), so OCI tags must use `identifier:version` (SBS-784).
                if matches!(registry_type, "oci" | "docker") {
                    if v.contains(':') {
                        format!("{identifier}@{v}")
                    } else {
                        format!("{identifier}:{v}")
                    }
                } else {
                    format!("{identifier}@{v}")
                }
            }
            _ => identifier.to_string(),
        };
        // SECURITY: this spec becomes an argument to npx/uvx/docker for a one-click
        // install. Reject anything that isn't a plain package spec (a leading '-'
        // reads as a flag; shell metacharacters have no business in a package name).
        // Args are passed without a shell, but this is cheap defense in depth against
        // a malicious or MITM'd registry entry.
        if !is_safe_package_id(&spec) {
            return None;
        }
        let (command, args) = match registry_type {
            "pypi" => ("uvx".to_string(), vec![spec]),
            "oci" | "docker" => (
                "docker".to_string(),
                vec![
                    "run".to_string(),
                    "-i".to_string(),
                    "--rm".to_string(),
                    spec,
                ],
            ),
            _ => {
                // npm-only, and deliberately here rather than in is_safe_package_id:
                // `user/image:tag` is an ordinary Docker reference, so the slash rule
                // below would drop legitimate OCI entries if applied globally.
                if npm_spec_escapes_registry(&spec) {
                    return None;
                }
                ("npx".to_string(), vec!["-y".to_string(), spec])
            }
        };
        let env_keys = pkg
            .get("environmentVariables")
            .or_else(|| pkg.get("environment_variables"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.get("name").and_then(|n| n.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        return Some(CatalogEntry {
            name,
            description,
            transport: "stdio".to_string(),
            command: Some(command),
            args,
            launch: None,
            url: None,
            env_keys,
            source: "registry".to_string(),
            homepage,
            publisher,
            category: String::new(),
            credentials_url: None,
            setup_hint: None,
            url_hint: None,
        });
    }

    None
}

/// True if a registry list item is the latest published version of its server
/// (the API returns one item per version, so we dedupe on this).
fn is_latest(item: &Value) -> bool {
    item.get("_meta")
        .and_then(|m| m.get("io.modelcontextprotocol.registry/official"))
        .and_then(|o| o.get("isLatest"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

fn is_active(item: &Value) -> bool {
    item.get("_meta")
        .and_then(|m| m.get("io.modelcontextprotocol.registry/official"))
        .and_then(|o| o.get("status"))
        .and_then(|v| v.as_str())
        .is_none_or(|status| status == "active")
}

fn registry_search_url(query: &str) -> String {
    let q = query.trim();
    if q.is_empty() {
        format!("{REGISTRY_URL}?limit=50&version=latest")
    } else {
        format!(
            "{REGISTRY_URL}?limit=50&version=latest&search={}",
            urlencoding::encode(q)
        )
    }
}

fn registry_failure(error: &(dyn std::error::Error + 'static)) -> RegistryStatus {
    let mut current = Some(error);
    while let Some(error) = current {
        if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            )
        }) {
            return RegistryStatus::TimedOut;
        }
        current = error.source();
    }
    RegistryStatus::Unavailable
}

/// Search the official MCP Registry with a bounded deadline.
pub fn search_registry(query: &str) -> Result<Vec<CatalogEntry>, RegistryStatus> {
    let url = registry_search_url(query);
    bounded_registry_fetch(REGISTRY_SEARCH_BUDGET, move || fetch_registry(&url))
}

fn bounded_registry_fetch(
    budget: std::time::Duration,
    fetch: impl FnOnce() -> Result<Vec<CatalogEntry>, RegistryStatus> + Send + 'static,
) -> Result<Vec<CatalogEntry>, RegistryStatus> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    // The worker owns only request data. Expired receivers drop late results.
    std::thread::Builder::new()
        .name("catalog-registry".into())
        .spawn(move || {
            let _ = sender.send(fetch());
        })
        .map_err(|_| RegistryStatus::Unavailable)?;
    receiver.recv_timeout(budget).map_err(|error| match error {
        std::sync::mpsc::RecvTimeoutError::Timeout => RegistryStatus::TimedOut,
        std::sync::mpsc::RecvTimeoutError::Disconnected => RegistryStatus::Unavailable,
    })?
}

fn fetch_registry(url: &str) -> Result<Vec<CatalogEntry>, RegistryStatus> {
    use std::io::Read;
    let resp = crate::http_client::agent()
        .get(url)
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(5)))
        .build()
        .call()
        .retain_status_body()
        .map_err(|error| registry_failure(&error))?;
    // Cap the registry response (defense in depth against a huge or MITM'd body).
    let mut buf = Vec::new();
    resp.into_body()
        .into_reader()
        .take(8 * 1024 * 1024)
        .read_to_end(&mut buf)
        .map_err(|error| registry_failure(&error))?;
    let body: Value = serde_json::from_slice(&buf).map_err(|_| RegistryStatus::Unavailable)?;

    let items = body
        .get("servers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(items
        .iter()
        .filter(|item| is_latest(item) && is_active(item))
        .filter_map(|item| map_server(item.get("server").unwrap_or(item)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registry_deadline_bounds_a_stalled_fetch_and_drops_late_results() {
        let (release, stalled) = std::sync::mpsc::channel();
        let (finished, done) = std::sync::mpsc::channel();
        let budget = REGISTRY_SEARCH_BUDGET;
        let start = std::time::Instant::now();
        let result = bounded_registry_fetch(budget, move || {
            stalled.recv().unwrap();
            finished.send(()).unwrap();
            Ok(popular())
        });
        assert_eq!(result, Err(RegistryStatus::TimedOut));
        assert!(start.elapsed() >= budget);
        assert!(start.elapsed() < budget + std::time::Duration::from_secs(1));
        release.send(()).unwrap();
        done.recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(result, Err(RegistryStatus::TimedOut));
    }

    #[test]
    fn live_timeout_is_classified_without_message_matching() {
        assert_eq!(
            registry_failure(&std::io::Error::from(std::io::ErrorKind::TimedOut)),
            RegistryStatus::TimedOut
        );
        assert_eq!(
            registry_failure(&std::io::Error::from(std::io::ErrorKind::WouldBlock)),
            RegistryStatus::TimedOut
        );
        assert_eq!(
            registry_failure(&std::io::Error::from(std::io::ErrorKind::ConnectionRefused)),
            RegistryStatus::Unavailable
        );
    }

    #[test]
    fn launch_and_endpoint_identity_fixtures() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../tests/fixtures/catalog-identities.json"))
                .unwrap();
        for fixture in fixtures.as_array().unwrap() {
            let identity = |value: &Value| {
                server_identity(
                    value["transport"].as_str().unwrap(),
                    value["command"].as_str(),
                    &serde_json::from_value::<Vec<String>>(value["args"].clone()).unwrap(),
                    value["url"].as_str(),
                )
            };
            let a = identity(&fixture["catalog"]);
            assert_eq!(
                a.is_some() && a == identity(&fixture["server"]),
                fixture["equal"].as_bool().unwrap(),
                "{}",
                fixture["case"]
            );
            let mut catalog =
                json!({"description": "", "envKeys": [], "source": "", "homepage": null});
            catalog
                .as_object_mut()
                .unwrap()
                .extend(fixture["catalog"].as_object().unwrap().clone());
            let catalog: CatalogEntry = serde_json::from_value(catalog).unwrap();
            let server: crate::registry::ServerEntry =
                serde_json::from_value(fixture["server"].clone()).unwrap();
            assert_eq!(
                installed_entry_identity(&catalog).is_some_and(|identity| {
                    installed_server_identities(&server).contains(&identity)
                }),
                fixture
                    .get("installedEqual")
                    .unwrap_or(&fixture["equal"])
                    .as_bool()
                    .unwrap(),
                "installed: {}",
                fixture["case"]
            );
        }
    }

    #[test]
    fn live_failure_and_timeout_keep_curated_matches_and_status() {
        for status in [RegistryStatus::Unavailable, RegistryStatus::TimedOut] {
            let result = search_with_registry("github", |_| Err(status));
            assert!(result.entries.iter().any(|entry| entry.name == "GitHub"));
            assert!(result.entries.iter().all(|entry| entry.source == "curated"));
            assert_eq!(result.registry_status, status);
            assert!(status.notice().unwrap().contains("curated matches only"));
            let empty = search_with_registry("no-fixture-match-xyz", |_| Err(status));
            assert!(empty.entries.is_empty());
            assert_eq!(empty.registry_status, status);
        }
        assert_eq!(
            search_with_registry("", |_| panic!("browse must stay offline")).registry_status,
            RegistryStatus::NotQueried
        );
    }

    #[test]
    fn search_deduplicates_identity_not_labels() {
        let github = search_curated("github")
            .into_iter()
            .find(|entry| entry.name == "GitHub")
            .unwrap();
        let mut renamed = github.clone();
        renamed.name = "My repositories".into();
        let mut other = github.clone();
        other.url = Some("https://different.example/mcp".into());
        other.source = "registry".into();
        let result = search_with_registry("github", |_| Ok(vec![renamed, other.clone()]));
        assert_eq!(result.registry_status, RegistryStatus::Available);
        assert!(result.entries.contains(&github));
        assert!(result.entries.contains(&other));
        assert!(!result
            .entries
            .iter()
            .any(|entry| entry.name == "My repositories"));
    }

    #[test]
    fn curated_packages_have_reviewed_exact_pins() {
        let pins: Value = serde_json::from_str(include_str!("../catalog-pins.json")).unwrap();
        for entry in curated().into_iter().filter(|e| e.transport == "stdio") {
            let runner = entry.command.as_deref().unwrap();
            assert!(matches!(runner, "npx" | "uvx"));
            let spec = &entry.args[if matches!(entry.args[0].as_str(), "-y" | "--from") {
                1
            } else {
                0
            }];
            let (name, version) = spec
                .split_once("==")
                .or_else(|| spec.rsplit_once('@'))
                .unwrap();
            let exact = if runner == "npx" {
                r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$"
            } else {
                r"^[0-9]+(?:\.[0-9]+)*(?:(?:a|b|rc|\.post|\.dev)[0-9]+)?$"
            };
            assert!(
                regex::Regex::new(exact).unwrap().is_match(version),
                "floating pin: {spec}"
            );
            let pin = &pins[format!("{runner}:{name}")];
            assert_eq!(pin["version"], version);
            assert!(pin["integrity"]
                .as_str()
                .unwrap()
                .starts_with(if runner == "npx" {
                    "sha512-"
                } else {
                    "sha256-"
                }));
            assert!(pin["url"].as_str().unwrap().starts_with("https://"));
        }
    }

    #[test]
    fn curated_remote_setup_matches_supported_publisher_auth() {
        let entries = curated();
        // Asana v2 requires a preregistered authorization-code client, which
        // Toolport's CIMD/DCR flow cannot yet supply. Keep it out of curated add.
        assert!(!entries.iter().any(|e| e.name == "Asana"));
        let langfuse = entries.iter().find(|e| e.name == "Langfuse").unwrap();
        assert_eq!(
            langfuse.url_hint.as_deref(),
            Some("https://your-langfuse.com/api/public/mcp")
        );
        assert!(langfuse.setup_hint.as_deref().unwrap().contains("Basic"));
        let postiz = entries.iter().find(|e| e.name == "Postiz").unwrap();
        assert_eq!(postiz.url.as_deref(), Some("https://mcp.postiz.com/mcp"));
        assert!(postiz.setup_hint.as_deref().unwrap().contains("token"));
    }

    #[test]
    fn registry_search_requests_only_current_versions() {
        assert_eq!(
            registry_search_url(""),
            "https://registry.modelcontextprotocol.io/v0.1/servers?limit=50&version=latest"
        );
        assert_eq!(
            registry_search_url(" git hub "),
            "https://registry.modelcontextprotocol.io/v0.1/servers?limit=50&version=latest&search=git%20hub"
        );
    }

    #[test]
    fn curated_is_nonempty_and_well_formed() {
        let c = curated();
        assert!(c.len() >= 20);
        for e in &c {
            assert!(!e.name.is_empty());
            // Each entry has a target: a URL, a command, or a url_hint
            // (self-hosted servers where the user supplies the URL at add time).
            assert!(
                e.url.is_some() || e.command.is_some() || e.url_hint.is_some(),
                "{} has no target",
                e.name
            );
            // Every curated entry must land in a browse-view category.
            assert!(!e.category.is_empty(), "{} has no category", e.name);
        }
    }

    #[test]
    fn curated_search_finds_popular_picks() {
        // The reported bug: searching a curated vendor must still surface it,
        // even though the live registry wouldn't return it.
        let vercel = filter_catalog(curated(), "vercel");
        // The official Vercel and the Toolport (Full API) overlay both surface, official first.
        assert_eq!(vercel.len(), 2);
        assert_eq!(vercel[0].name, "Vercel");
        assert_eq!(vercel[1].name, "Vercel (Full API)");
        // Description matches too (Postgres -> Neon/Supabase).
        assert!(filter_catalog(curated(), "postgres")
            .iter()
            .any(|e| e.name == "Neon"));
        // Empty query returns the full set.
        assert_eq!(filter_catalog(curated(), "").len(), curated().len());
    }

    #[test]
    fn search_ranking_prefers_curated_and_exact_name_matches() {
        let stripe = curated()
            .into_iter()
            .find(|entry| entry.name == "Stripe")
            .unwrap();
        let mut registry_prefix = stripe.clone();
        registry_prefix.name = "Stripe helper".into();
        registry_prefix.source = "registry".into();
        let mut results = vec![registry_prefix, stripe];
        rank_search_results(&mut results, "stripe");
        assert_eq!(results[0].name, "Stripe");
    }

    #[test]
    fn registry_lifecycle_filter_excludes_inactive_entries() {
        let item = |status| {
            json!({
                "_meta": {
                    "io.modelcontextprotocol.registry/official": {
                        "status": status,
                        "isLatest": true
                    }
                }
            })
        };
        assert!(is_active(&item("active")));
        assert!(!is_active(&item("deprecated")));
        assert!(!is_active(&item("deleted")));
    }

    #[test]
    fn trello_is_a_hosted_oauth_catalog_entry() {
        let entries = curated();
        let trello = entries.iter().find(|entry| entry.name == "Trello").unwrap();
        assert_eq!(trello.transport, "http");
        assert_eq!(trello.url.as_deref(), Some("https://mcp.trello.com/v1"));
        assert!(trello.command.is_none());
        assert!(trello.env_keys.is_empty());
        assert_eq!(trello.category, "Apps & productivity");
        assert!(trello.setup_hint.as_deref().unwrap().contains("OAuth"));
        assert!(trello.credentials_url.is_none());
        assert_eq!(filter_catalog(entries, "trello").len(), 1);
    }

    #[test]
    fn atlassian_uses_the_current_v2_gateway_endpoint() {
        let atlassian = curated()
            .into_iter()
            .find(|entry| entry.name == "Atlassian")
            .expect("Atlassian must remain in the curated catalog");
        assert_eq!(
            atlassian.url.as_deref(),
            Some("https://mcp.atlassian.com/v2/mcp?tools=all")
        );
    }

    #[test]
    fn filter_catalog_matches_category_headings() {
        let c = curated();
        let databases = filter_catalog(c.clone(), "databases");
        for name in [
            "Supabase",
            "Neon",
            "Elasticsearch",
            "Qdrant",
            "MongoDB",
            "PostgreSQL",
            "Redis",
        ] {
            assert!(
                databases.iter().any(|e| e.name == name),
                "searching 'databases' should include {name}"
            );
        }

        let productivity = filter_catalog(c.clone(), "productivity");
        assert!(
            productivity.iter().any(|e| e.name == "Notion"),
            "searching 'productivity' should include Apps & productivity members"
        );
        assert!(
            productivity.iter().any(|e| e.name == "Linear"),
            "searching 'productivity' should include Apps & productivity members"
        );
        assert!(productivity.iter().any(|e| e.name == "Postman"));

        let infrastructure = filter_catalog(c, "infrastructure");
        assert!(
            infrastructure.iter().any(|e| e.name == "GitHub"),
            "searching 'infrastructure' should include Code & infrastructure members"
        );
        assert!(
            infrastructure.iter().any(|e| e.name == "AWS"),
            "searching 'infrastructure' should include Code & infrastructure members"
        );
    }

    #[test]
    fn filter_catalog_empty_category_does_not_match_everything() {
        let entry = CatalogEntry {
            name: "Registry Only".into(),
            description: "A live registry result with no browse category.".into(),
            transport: "http".into(),
            command: None,
            args: vec![],
            launch: None,
            url: Some("https://example.com/mcp".into()),
            env_keys: vec![],
            source: "registry".into(),
            homepage: None,
            publisher: None,
            category: String::new(),
            credentials_url: None,
            setup_hint: None,
            url_hint: None,
        };
        assert!(filter_catalog(vec![entry], "databases").is_empty());
    }

    #[test]
    fn filter_catalog_multiword_uses_all_terms_fallback() {
        let c = curated();
        // "serverless postgres" is not a contiguous substring of Neon's text
        // ("Serverless Postgres: ..."), but both terms appear, so the all-terms
        // fallback must still surface it.
        let hits = filter_catalog(c.clone(), "serverless postgres");
        assert!(
            hits.iter().any(|e| e.name == "Neon"),
            "all-terms fallback should match Neon"
        );
        // A term present nowhere yields nothing.
        assert!(filter_catalog(c, "zzzznotacatalogword").is_empty());
    }

    #[test]
    fn maps_a_remote_server() {
        // Shape taken from a real registry.modelcontextprotocol.io response.
        let server = json!({
            "name": "ac.inference.sh/mcp",
            "title": "inference.sh",
            "description": "Run 150+ AI apps.",
            "remotes": [{ "type": "streamable-http", "url": "https://api.inference.sh/mcp" }]
        });
        let e = map_server(&server).unwrap();
        assert_eq!(e.name, "inference.sh");
        assert_eq!(e.transport, "http");
        assert_eq!(e.url.as_deref(), Some("https://api.inference.sh/mcp"));
        assert_eq!(e.source, "registry");
    }

    #[test]
    fn maps_an_npm_package_with_env() {
        let server = json!({
            "name": "io.github.acme/widget",
            "description": "A widget server.",
            "packages": [{
                "registryType": "npm",
                "identifier": "@acme/widget-mcp",
                "version": "1.2.3",
                "environmentVariables": [{ "name": "ACME_API_KEY" }]
            }]
        });
        let e = map_server(&server).unwrap();
        // No title -> falls back to the namespaced id.
        assert_eq!(e.name, "io.github.acme/widget");
        assert_eq!(e.transport, "stdio");
        assert_eq!(e.command.as_deref(), Some("npx"));
        assert_eq!(e.args, vec!["-y", "@acme/widget-mcp@1.2.3"]);
        assert_eq!(e.env_keys, vec!["ACME_API_KEY"]);
    }

    #[test]
    fn maps_a_pypi_package_to_uvx() {
        let server = json!({
            "name": "io.github.acme/pytool",
            "description": "A python server.",
            "packages": [{
                "registryType": "pypi",
                "identifier": "acme-mcp",
                "version": "0.4.2"
            }]
        });
        let e = map_server(&server).unwrap();
        assert_eq!(e.transport, "stdio");
        assert_eq!(e.command.as_deref(), Some("uvx"));
        // uvx takes the spec alone — no `-y`-style prefix.
        assert_eq!(e.args, vec!["acme-mcp@0.4.2"]);

        // `latest` is a dist-tag, not a version, so the bare identifier is used.
        let latest = json!({ "name": "io.github.acme/pytool", "title": "Pytool",
            "packages": [{ "registryType": "pypi", "identifier": "acme-mcp", "version": "latest" }] });
        let e = map_server(&latest).unwrap();
        assert_eq!(e.command.as_deref(), Some("uvx"));
        assert_eq!(e.args, vec!["acme-mcp"]);

        // Same for a missing version.
        let bare = json!({ "name": "io.github.acme/pytool", "title": "Pytool",
            "packages": [{ "registryType": "pypi", "identifier": "acme-mcp" }] });
        assert_eq!(map_server(&bare).unwrap().args, vec!["acme-mcp"]);
    }

    #[test]
    fn maps_container_packages_to_a_docker_run() {
        // "oci" and "docker" are the two spellings the registry uses for the same thing.
        for registry_type in ["oci", "docker"] {
            let server = json!({
                "name": "io.github.acme/boxed",
                "description": "A containerized server.",
                "packages": [{
                    "registryType": registry_type,
                    "identifier": "ghcr.io/acme/boxed-mcp",
                    "version": "1.2.3",
                    "environmentVariables": [{ "name": "ACME_API_KEY" }]
                }]
            });
            let e = map_server(&server).unwrap();
            assert_eq!(e.transport, "stdio");
            assert_eq!(e.command.as_deref(), Some("docker"), "{registry_type}");
            // stdio needs the container's stdin held open and the container reaped,
            // so `-i --rm` must precede the image spec.
            assert_eq!(
                e.args,
                vec!["run", "-i", "--rm", "ghcr.io/acme/boxed-mcp:1.2.3"],
                "{registry_type}"
            );
            assert_eq!(e.env_keys, vec!["ACME_API_KEY"], "{registry_type}");
        }

        // No version: the image spec is the bare identifier.
        let bare = json!({ "name": "io.github.acme/boxed", "title": "Boxed",
            "packages": [{ "registryType": "oci", "identifier": "ghcr.io/acme/boxed-mcp" }] });
        assert_eq!(
            map_server(&bare).unwrap().args,
            vec!["run", "-i", "--rm", "ghcr.io/acme/boxed-mcp"]
        );

        // OCI digests use @; a colon is only the separator for a tag.
        let digest = json!({ "name": "io.github.acme/boxed", "title": "Boxed",
            "packages": [{
                "registryType": "oci",
                "identifier": "ghcr.io/acme/boxed-mcp",
                "version": "sha256:abcdef0123456789"
            }] });
        assert_eq!(
            map_server(&digest).unwrap().args,
            vec![
                "run",
                "-i",
                "--rm",
                "ghcr.io/acme/boxed-mcp@sha256:abcdef0123456789"
            ]
        );
    }

    #[test]
    fn map_server_rejects_unsafe_specs_for_every_registry_type() {
        // The safety gate runs before the registry_type match, so uvx and docker get
        // the same treatment npx does.
        for registry_type in ["pypi", "oci", "docker"] {
            // Leading-dash identifier (flag injection into uvx/docker) is dropped.
            let flag = json!({ "name": "io.x/y", "title": "Y",
                "packages": [{ "registryType": registry_type, "identifier": "--unsafe-flag" }] });
            assert!(
                map_server(&flag).is_none(),
                "{registry_type}: flag-injection identifier must be dropped"
            );
            // Shell metacharacters in the version are dropped.
            let meta = json!({ "name": "io.x/z", "title": "Z",
                "packages": [{ "registryType": registry_type, "identifier": "pkg", "version": "1; rm -rf /" }] });
            assert!(
                map_server(&meta).is_none(),
                "{registry_type}: shell metachars must be dropped"
            );
            // An empty identifier has nothing to install.
            let empty = json!({ "name": "io.x/w", "title": "W",
                "packages": [{ "registryType": registry_type, "identifier": "" }] });
            assert!(
                map_server(&empty).is_none(),
                "{registry_type}: empty identifier must be dropped"
            );
        }
    }

    /// npm accepts several spellings of "fetch this from a git host" that carry no
    /// `://` and no leading alias, so a prefix-only check let a malicious or MITM'd
    /// registry entry one-click-install code from outside the named registry.
    #[test]
    fn map_server_rejects_npm_specs_that_escape_the_registry() {
        let npm = |identifier: &str, version: Option<&str>| {
            let mut pkg = json!({ "registryType": "npm", "identifier": identifier });
            if let Some(v) = version {
                pkg["version"] = json!(v);
            }
            json!({ "name": "io.x/y", "title": "Y", "packages": [pkg] })
        };
        for (identifier, version) in [
            // Unscoped slash name: GitHub shorthand to npm.
            ("attacker/payload", None),
            // Hosted-git alias smuggled in as the version range.
            ("lodash", Some("github:attacker/payload")),
            ("lodash", Some("gitlab:attacker/payload")),
            // Hosted URL with the scheme left off.
            ("github.com/attacker/payload", None),
            // SCP-style git, which has no `://` to catch.
            ("git@github.com:attacker/payload.git", None),
        ] {
            assert!(
                map_server(&npm(identifier, version)).is_none(),
                "npm spec {identifier}@{version:?} must not reach npx -y"
            );
        }

        // Real npm packages still install, including scoped names whose own `@` must
        // not be read as the version separator.
        for (identifier, version) in [
            ("@scope/name", None),
            ("@scope/name", Some("1.2.3")),
            ("express", Some("4.18.0")),
        ] {
            let entry = map_server(&npm(identifier, version));
            assert!(
                entry.is_some(),
                "npm spec {identifier}@{version:?} is legitimate and must still map"
            );
        }

        // The slash rule is npm-only: a Docker reference is not GitHub shorthand.
        let docker = json!({ "name": "io.x/d", "title": "D", "packages": [
            { "registryType": "oci", "identifier": "user/image", "version": "tag" }] });
        assert!(
            map_server(&docker).is_some(),
            "a docker user/image:tag reference must still map"
        );
    }

    #[test]
    fn skips_isnt_latest() {
        let old = json!({ "_meta": { "io.modelcontextprotocol.registry/official": { "isLatest": false } } });
        let cur = json!({ "_meta": { "io.modelcontextprotocol.registry/official": { "isLatest": true } } });
        assert!(!is_latest(&old));
        assert!(is_latest(&cur));
    }

    #[test]
    fn map_server_rejects_unsafe_specs() {
        // Leading-dash identifier (flag injection into npx) is dropped.
        let flag = json!({ "name": "io.x/y", "title": "Y",
            "packages": [{ "registryType": "npm", "identifier": "--unsafe-flag" }] });
        assert!(
            map_server(&flag).is_none(),
            "flag-injection identifier must be dropped"
        );
        // Shell metacharacters in the version are dropped.
        let meta = json!({ "name": "io.x/z", "title": "Z",
            "packages": [{ "registryType": "npm", "identifier": "pkg", "version": "1; rm -rf /" }] });
        assert!(
            map_server(&meta).is_none(),
            "shell metachars must be dropped"
        );
        // A non-http(s) remote URL is dropped.
        let scheme = json!({ "name": "io.x/w", "title": "W",
            "remotes": [{ "type": "streamable-http", "url": "file:///etc/passwd" }] });
        assert!(
            map_server(&scheme).is_none(),
            "non-http remote must be dropped"
        );
        // github:/URL/git+ specs would make npx fetch outside npm. Colon stays
        // allowed for docker host:port and OCI tags (see maps_container_packages).
        for identifier in [
            "github:attacker/payload",
            "https://evil.example/pkg.tgz",
            "http://evil.example/pkg.tgz",
            "git+https://github.com/attacker/payload.git",
            "npm:evil",
        ] {
            let remote = json!({ "name": "io.x/r", "title": "R",
                "packages": [{ "registryType": "npm", "identifier": identifier }] });
            assert!(
                map_server(&remote).is_none(),
                "{identifier}: remote package spec must be dropped"
            );
        }
    }

    // Self-hosted catalog coverage (url_hint / setup_hint invariants), contributed by
    // @bradhallett (salvaged from PR #62 onto current main).
    #[test]
    fn self_hosted_entries_have_url_hint_not_url() {
        let c = curated();
        for e in &c {
            if e.url_hint.is_some() {
                // Self-hosted entries must NOT have a fixed URL (user supplies it).
                assert!(e.url.is_none(), "{} has both url and url_hint", e.name);
                // And must have transport http (they speak MCP over HTTP).
                assert_eq!(
                    e.transport, "http",
                    "{} has url_hint but transport is not http",
                    e.name
                );
            }
        }
    }

    #[test]
    fn url_hint_round_trips_through_serialization() {
        let e = curated()
            .into_iter()
            .find(|e| e.url_hint.is_some())
            .unwrap();
        let original_hint = e.url_hint.clone().unwrap();
        let json = serde_json::to_string(&e).unwrap();
        // url_hint is serialized (not skipped — it's present).
        assert!(json.contains("urlHint"), "url_hint should appear in JSON");
        let back: CatalogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.url_hint.as_deref(), Some(original_hint.as_str()));
    }

    #[test]
    fn n8n_and_langfuse_are_in_catalog() {
        let c = curated();
        assert!(
            c.iter().any(|e| e.name == "n8n"),
            "n8n missing from catalog"
        );
        assert!(
            c.iter().any(|e| e.name == "Langfuse"),
            "Langfuse missing from catalog"
        );
    }

    #[test]
    fn n8n_and_langfuse_have_categories() {
        let c = curated();
        for e in c.iter().filter(|e| e.name == "n8n" || e.name == "Langfuse") {
            assert!(!e.category.is_empty(), "{} has no category", e.name);
        }
    }

    #[test]
    fn self_hosted_entries_have_credential_hints() {
        // n8n and Langfuse should guide the user on their URL + credentials. On current
        // main that guidance lives in url_hint (adapted from Brad's setup_hint check).
        let c = curated();
        for name in ["n8n", "Langfuse"] {
            let e = c.iter().find(|e| e.name == name).unwrap();
            let hint = e.url_hint.as_deref().or(e.setup_hint.as_deref());
            assert!(
                hint.map(|h| !h.is_empty()).unwrap_or(false),
                "{name} should have a credential/setup hint"
            );
        }
    }

    #[test]
    fn registry_entries_have_no_url_hint() {
        // map_server (the MCP registry mapper) should never set url_hint.
        let server = json!({
            "name": "io.test/example",
            "title": "Example",
            "packages": [{ "registryType": "npm", "identifier": "example-mcp" }]
        });
        let entry = map_server(&server).unwrap();
        assert!(
            entry.url_hint.is_none(),
            "registry entries should not have url_hint"
        );
    }

    #[test]
    fn curated_launch_references_are_complete_and_templates_stable() {
        let entries = curated();
        let mut templates = std::collections::HashSet::new();
        for entry in &entries {
            let launch = entry.launch.as_ref().expect("curated template identity");
            assert!(templates.insert(launch.template.as_deref().unwrap()));
            launch.validate(&entry.args, true).unwrap();
            assert!(launch
                .required_env
                .iter()
                .all(|key| entry.env_keys.contains(key)));
        }
        let mut bad = entries.into_iter().find(|e| e.name == "Twilio").unwrap();
        bad.launch.as_mut().unwrap().bindings[0]
            .parts
            .push(ArgPart::Input {
                key: "MISSING".into(),
                unknown_fields: Default::default(),
            });
        assert!(bad.launch.unwrap().validate(&bad.args, true).is_err());
        let mut unused = curated().into_iter().find(|e| e.name == "Twilio").unwrap();
        unused.launch.as_mut().unwrap().inputs.push(LaunchInput {
            key: "UNUSED".into(),
            label: "Unused".into(),
            secret: false,
            required: true,
            value: None,
            unknown_fields: Default::default(),
        });
        assert!(unused.launch.unwrap().validate(&unused.args, true).is_err());
        let mut missing_binding = curated().into_iter().find(|e| e.name == "Twilio").unwrap();
        missing_binding.launch.as_mut().unwrap().bindings.clear();
        assert!(missing_binding
            .launch
            .unwrap()
            .validate(&missing_binding.args, true)
            .is_err());
    }

    #[test]
    fn redis_and_postman_have_documented_launch_shapes() {
        let entries = curated();
        let redis = entries.iter().find(|e| e.name == "Redis").unwrap();
        assert_eq!(redis.category, "Databases");
        assert_eq!(redis.command.as_deref(), Some("uvx"));
        assert_eq!(
            redis.args,
            [
                "--from",
                "redis-mcp-server==0.5.1",
                "redis-mcp-server",
                "--url",
                "<launch-input>",
            ]
        );
        let launch = redis.launch.as_ref().unwrap();
        assert_eq!(launch.template.as_deref(), Some("redis"));
        assert_eq!(launch.revision, Some(1));
        assert_eq!(launch.inputs.len(), 1);
        assert_eq!(launch.inputs[0].key, "REDIS_URL");
        assert!(launch.inputs[0].secret && launch.inputs[0].required);
        assert_eq!(launch.bindings[0].index, 4);

        let postman = entries.iter().find(|e| e.name == "Postman").unwrap();
        assert_eq!(postman.transport, "http");
        assert_eq!(
            postman.url.as_deref(),
            Some("https://mcp.postman.com/minimal")
        );
        assert!(postman.command.is_none());
        assert_eq!(postman.category, "Apps & productivity");
        assert!(postman.setup_hint.as_deref().unwrap().contains("OAuth"));
    }
}
