//! Curated "stacks": role-based bundles of catalog servers with guided setup.
//!
//! A stack is just a named, ordered list of catalog entries (referenced by name),
//! resolved against [`catalog::curated`] so the UI gets each server's command,
//! env keys, and credential hints in one call. Applying a stack reuses the
//! existing add-server / profile / install primitives; nothing here writes state.

use serde::Serialize;

use crate::catalog::{self, CatalogEntry};

/// One curated stack: a use-case bundle the user can set up in one flow.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stack {
    /// Stable id (kebab-case), e.g. "fullstack-web".
    pub id: String,
    pub name: String,
    pub description: String,
    /// The stack's servers, resolved to full catalog entries (with cred hints).
    /// A name that doesn't resolve is dropped, so the list is always usable.
    pub servers: Vec<CatalogEntry>,
}

/// Raw stack definition before catalog resolution. Kept as a separate list so
/// tests can assert every referenced name resolves without a magic server total.
struct StackDef {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    /// Exact `catalog::curated()` entry names, in display order.
    server_names: &'static [&'static str],
}

fn stack_defs() -> Vec<StackDef> {
    vec![
        StackDef {
            id: "fullstack-web",
            name: "Full-stack web dev",
            description: "Ship and run a web app: repo, deploys, database, and error tracking.",
            server_names: &["GitHub", "Vercel", "PostgreSQL", "Sentry", "Filesystem"],
        },
        StackDef {
            id: "backend-data",
            name: "Backend & data",
            description: "Work across your databases and code from one agent.",
            server_names: &["PostgreSQL", "MongoDB", "GitHub", "Fetch", "Filesystem"],
        },
        StackDef {
            id: "infra-devops",
            name: "Infra & DevOps",
            description: "Manage cloud infrastructure from your editor: Linode, Kubernetes, and AWS.",
            server_names: &["Linode", "Kubernetes", "AWS", "GitHub", "Sentry"],
        },
        StackDef {
            id: "research-docs",
            name: "Research & docs",
            description: "Search the web, pull up-to-date library docs, and write into Notion.",
            server_names: &["Context7", "Exa", "Perplexity", "Notion", "Fetch"],
        },
        StackDef {
            id: "ai-ml",
            name: "AI & ML",
            description: "Build with models and retrieval: model catalogs, a vector store, and up-to-date docs.",
            server_names: &["Hugging Face", "OpenRouter", "Qdrant", "Context7", "Exa"],
        },
        StackDef {
            id: "product-design",
            name: "Product & design",
            description: "Run product work from one place: issues, docs, designs, and team chat.",
            server_names: &["Linear", "Notion", "Figma", "Slack"],
        },
        StackDef {
            id: "founder",
            name: "Founder / indie SaaS",
            description: "Ship and run a small SaaS: payments, deploys, code, email, and issues.",
            server_names: &["Stripe", "Vercel", "GitHub", "Resend", "Linear"],
        },
        StackDef {
            id: "web-automation",
            name: "Web scraping & automation",
            description: "Pull data from any site and drive real browsers: search, scrape, extract, and automate.",
            server_names: &["Firecrawl", "Tavily", "Playwright", "Browserbase", "Apify"],
        },
    ]
}

/// The curated set of stacks. Each references catalog entries by name; we resolve
/// them here so a typo surfaces as a missing server in tests, not at runtime.
pub fn stacks() -> Vec<Stack> {
    let catalog = catalog::curated();
    let by_name: std::collections::HashMap<&str, &CatalogEntry> =
        catalog.iter().map(|e| (e.name.as_str(), e)).collect();

    stack_defs()
        .into_iter()
        .map(|def| Stack {
            id: def.id.to_string(),
            name: def.name.to_string(),
            description: def.description.to_string(),
            servers: def
                .server_names
                .iter()
                .filter_map(|n| by_name.get(n).map(|e| (*e).clone()))
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stack_resolves_all_its_servers() {
        let catalog = catalog::curated();
        let by_name: std::collections::HashMap<&str, &CatalogEntry> =
            catalog.iter().map(|e| (e.name.as_str(), e)).collect();

        for def in stack_defs() {
            for server_name in def.server_names {
                assert!(
                    by_name.contains_key(server_name),
                    "stack `{}` references server name `{}` not in the catalog",
                    def.id,
                    server_name
                );
            }
        }

        // Resolved stacks stay non-empty and carry stable id + name.
        for s in stacks() {
            assert!(!s.servers.is_empty(), "stack {} is empty", s.id);
            assert!(!s.id.is_empty() && !s.name.is_empty());
            let def = stack_defs().into_iter().find(|d| d.id == s.id).unwrap();
            assert_eq!(
                s.servers.len(),
                def.server_names.len(),
                "stack `{}` dropped a server during resolution",
                s.id
            );
        }
    }

    #[test]
    fn stack_servers_carry_credential_hints_where_expected() {
        let infra = stacks()
            .into_iter()
            .find(|s| s.id == "infra-devops")
            .unwrap();
        let linode = infra.servers.iter().find(|e| e.name == "Linode").unwrap();
        // Linode is token-based: it should carry a creds URL + a setup hint.
        assert!(linode.credentials_url.is_some());
        assert!(linode.setup_hint.is_some());
        assert!(linode.env_keys.iter().any(|k| k == "LINODE_API_TOKEN"));
    }
}
