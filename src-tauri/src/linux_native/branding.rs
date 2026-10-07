use adw::prelude::*;

// Card text can wrap into several lines when tiled. Keep the badge's
// allocation independent of the row height, including transport fallbacks.
fn centered_logo(image: gtk::Image) -> gtk::Image {
    image.set_halign(gtk::Align::Center);
    image.set_valign(gtk::Align::Center);
    image
}

fn png_image(bytes: &'static [u8], css_class: &str) -> gtk::Image {
    let image = gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from_static(bytes))
        .map(|texture| gtk::Image::from_paintable(Some(&texture)))
        .unwrap_or_default();
    image.add_css_class(css_class);
    centered_logo(image)
}

pub(super) fn toolport_mark() -> gtk::Image {
    png_image(
        include_bytes!("../../icons/32x32.png"),
        "toolport-brand-mark",
    )
}

pub(super) fn client_logo(id: &str) -> gtk::Image {
    let bytes: Option<&'static [u8]> = match id {
        "claude-desktop" => Some(include_bytes!("../../icons/client-logos/claude.png")),
        "claude-code" => Some(include_bytes!("../../icons/client-logos/claude-code.png")),
        "cursor" => Some(include_bytes!("../../icons/client-logos/cursor.png")),
        "vscode" => Some(include_bytes!("../../icons/client-logos/vscode.png")),
        "codex" => Some(include_bytes!("../../icons/client-logos/codex.png")),
        "antigravity" => Some(include_bytes!("../../icons/client-logos/antigravity.png")),
        "gemini-cli" => Some(include_bytes!("../../icons/client-logos/gemini-cli.png")),
        "cline" => Some(include_bytes!("../../icons/client-logos/cline.png")),
        "roo-code" => Some(include_bytes!("../../icons/client-logos/roo-code.png")),
        "kiro" => Some(include_bytes!("../../icons/client-logos/kiro.png")),
        "lm-studio" => Some(include_bytes!("../../icons/client-logos/lm-studio.png")),
        "goose" => Some(include_bytes!("../../icons/client-logos/goose.png")),
        "hermes" => Some(include_bytes!("../../icons/client-logos/hermes.png")),
        "windsurf" | "devin-cli" => Some(include_bytes!("../../icons/client-logos/devin.png")),
        "warp" => Some(include_bytes!("../../icons/client-logos/warp.png")),
        "zed" => Some(include_bytes!("../../icons/client-logos/zed.png")),
        "amazon-q" => Some(include_bytes!("../../icons/client-logos/amazon-q.png")),
        "grok" => Some(include_bytes!("../../icons/client-logos/grok.png")),
        "opencode" => Some(include_bytes!("../../icons/client-logos/opencode.png")),
        "qwen-code" => Some(include_bytes!("../../icons/client-logos/qwen-code.png")),
        "kimi-code" => Some(include_bytes!("../../icons/client-logos/kimi-code.png")),
        "junie" => Some(include_bytes!("../../icons/client-logos/junie.png")),
        "kilo-code" => Some(include_bytes!("../../icons/client-logos/kilo-code.png")),
        "github-copilot-cli" => Some(include_bytes!(
            "../../icons/client-logos/github-copilot-cli.png"
        )),
        "amp" => Some(include_bytes!("../../icons/client-logos/amp.png")),
        "pi" => Some(include_bytes!("../../icons/client-logos/pi.png")),
        "omp" => Some(include_bytes!("../../icons/client-logos/omp.png")),
        "droid" => Some(include_bytes!("../../icons/client-logos/droid.png")),
        "boltai" => Some(include_bytes!("../../icons/client-logos/boltai.png")),
        "anythingllm" => Some(include_bytes!("../../icons/client-logos/anythingllm.png")),
        "continue" => Some(include_bytes!("../../icons/client-logos/continue.png")),
        _ => None,
    };
    bytes.map_or_else(
        || {
            let image = gtk::Image::from_icon_name("computer-symbolic");
            image.add_css_class("toolport-card-icon");
            centered_logo(image)
        },
        |bytes| png_image(bytes, "toolport-client-logo"),
    )
}

/// True when `needle` appears in `name` as a whole word rather than as a
/// substring of a longer one.
fn name_has_word(name: &str, needle: &str) -> bool {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| word == needle)
}

fn server_logo_key(name: &str) -> Option<&'static str> {
    let name = name.to_lowercase();
    [
        ("stripe", "stripe"),
        ("github", "github"),
        ("vercel", "vercel"),
        ("sentry", "sentry"),
        ("cloudflare", "cloudflare"),
        ("clerk", "clerk"),
        ("amazon web services", "amazonwebservices"),
        ("aws", "amazonwebservices"),
        ("kubernetes", "kubernetes"),
        ("chrome devtools", "googlechrome"),
        ("supabase", "supabase"),
        ("neon", "neon"),
        ("postgres", "postgresql"),
        ("mongo", "mongodb"),
        ("elastic", "elasticsearch"),
        ("qdrant", "qdrant"),
        ("notion", "notion"),
        ("linear", "linear"),
        ("trello", "trello"),
        ("revenuecat", "revenuecat"),
        ("revenue cat", "revenuecat"),
        ("redis", "redis"),
        ("postman", "postman"),
        ("atlassian", "atlassian"),
        ("jira", "jira"),
        ("asana", "asana"),
        ("airtable", "airtable"),
        ("todoist", "todoist"),
        ("slack", "slack"),
        ("twilio", "twilio"),
        ("postiz", "postiz"),
        ("hugging face", "huggingface"),
        ("openrouter", "openrouter"),
        ("brave", "brave"),
        ("perplexity", "perplexity"),
        ("figma", "figma"),
        ("resend", "resend"),
        ("n8n", "n8n"),
    ]
    .into_iter()
    .find_map(|(needle, key)| name.contains(needle).then_some(key))
    // `git` is matched last and on a word boundary, because plain `contains`
    // finds it inside unrelated names: "digitalocean" carries "git" at offset 2
    // and would otherwise wear the Git logo. `github` is matched above, by name.
    .or_else(|| name_has_word(&name, "git").then_some("git"))
}

pub(super) fn server_logo(name: &str, transport: &str) -> gtk::Image {
    let bytes: Option<&'static [u8]> = match server_logo_key(name) {
        Some("stripe") => Some(include_bytes!("../../icons/server-logos/stripe.png")),
        Some("github") => Some(include_bytes!("../../icons/server-logos/github.png")),
        Some("vercel") => Some(include_bytes!("../../icons/server-logos/vercel.png")),
        Some("sentry") => Some(include_bytes!("../../icons/server-logos/sentry.png")),
        Some("cloudflare") => Some(include_bytes!("../../icons/server-logos/cloudflare.png")),
        Some("clerk") => Some(include_bytes!("../../icons/server-logos/clerk.png")),
        Some("amazonwebservices") => Some(include_bytes!(
            "../../icons/server-logos/amazonwebservices.png"
        )),
        Some("kubernetes") => Some(include_bytes!("../../icons/server-logos/kubernetes.png")),
        Some("googlechrome") => Some(include_bytes!("../../icons/server-logos/googlechrome.png")),
        Some("supabase") => Some(include_bytes!("../../icons/server-logos/supabase.png")),
        Some("neon") => Some(include_bytes!("../../icons/server-logos/neon.png")),
        Some("postgresql") => Some(include_bytes!("../../icons/server-logos/postgresql.png")),
        Some("mongodb") => Some(include_bytes!("../../icons/server-logos/mongodb.png")),
        Some("elasticsearch") => Some(include_bytes!("../../icons/server-logos/elasticsearch.png")),
        Some("qdrant") => Some(include_bytes!("../../icons/server-logos/qdrant.png")),
        Some("notion") => Some(include_bytes!("../../icons/server-logos/notion.png")),
        Some("linear") => Some(include_bytes!("../../icons/server-logos/linear.png")),
        Some("trello") => Some(include_bytes!("../../icons/server-logos/trello.png")),
        Some("revenuecat") => Some(include_bytes!("../../icons/server-logos/revenuecat.png")),
        Some("redis") => Some(include_bytes!("../../icons/server-logos/redis.png")),
        Some("postman") => Some(include_bytes!("../../icons/server-logos/postman.png")),
        Some("atlassian") => Some(include_bytes!("../../icons/server-logos/atlassian.png")),
        Some("jira") => Some(include_bytes!("../../icons/server-logos/jira.png")),
        Some("asana") => Some(include_bytes!("../../icons/server-logos/asana.png")),
        Some("airtable") => Some(include_bytes!("../../icons/server-logos/airtable.png")),
        Some("todoist") => Some(include_bytes!("../../icons/server-logos/todoist.png")),
        Some("slack") => Some(include_bytes!("../../icons/server-logos/slack.png")),
        Some("twilio") => Some(include_bytes!("../../icons/server-logos/twilio.png")),
        Some("postiz") => Some(include_bytes!("../../icons/server-logos/postiz.png")),
        Some("huggingface") => Some(include_bytes!("../../icons/server-logos/huggingface.png")),
        Some("openrouter") => Some(include_bytes!("../../icons/server-logos/openrouter.png")),
        Some("brave") => Some(include_bytes!("../../icons/server-logos/brave.png")),
        Some("perplexity") => Some(include_bytes!("../../icons/server-logos/perplexity.png")),
        Some("figma") => Some(include_bytes!("../../icons/server-logos/figma.png")),
        Some("resend") => Some(include_bytes!("../../icons/server-logos/resend.png")),
        Some("n8n") => Some(include_bytes!("../../icons/server-logos/n8n.png")),
        Some("git") => Some(include_bytes!("../../icons/server-logos/git.png")),
        _ => None,
    };
    bytes.map_or_else(
        || {
            let icon =
                gtk::Image::from_icon_name(if transport == "Local stdio" || transport == "stdio" {
                    "utilities-terminal-symbolic"
                } else {
                    "network-server-symbolic"
                });
            icon.add_css_class("toolport-card-icon");
            centered_logo(icon)
        },
        |bytes| png_image(bytes, "toolport-server-logo"),
    )
}

#[cfg(test)]
mod tests {

    /// Exercises GTK allocation, not just alignment properties. A taller card
    /// must not turn provider marks or neutral fallback badges into rectangles.
    #[test]
    #[ignore = "requires a GTK display; run inside isolated omabox"]
    fn logo_badges_stay_square_in_tall_cards() {
        use adw::prelude::*;
        gtk::init().unwrap();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let images = [
            super::server_logo("Trello", "http"),
            super::server_logo("RevenueCat", "http"),
            super::server_logo("private MCP", "http"),
            super::server_logo("private tool", "stdio"),
            super::client_logo("cursor"),
            super::client_logo("unknown client"),
        ];
        for image in &images {
            row.append(image);
        }
        let window = gtk::Window::builder().child(&row).build();
        for height in [120, 220] {
            row.set_size_request(-1, height);
            window.present();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let context = gtk::glib::MainContext::default();
            while row.height() < height && std::time::Instant::now() < deadline {
                while context.pending() {
                    context.iteration(false);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(
                row.height() >= height,
                "card must receive its tall allocation"
            );
            for image in &images {
                assert!(image.width() > 0 && image.width() <= 32);
                assert_eq!(image.width(), image.height(), "badge must stay square");
            }
        }
        window.close();
    }

    /// Ordered substring matching found "git" inside unrelated names:
    /// "digitalocean" carries it at offset 2 and wore the Git logo.
    #[test]
    fn a_name_that_merely_contains_git_does_not_get_the_git_logo() {
        assert_eq!(server_logo_key("DigitalOcean"), None);
        assert_eq!(server_logo_key("Digital Ocean MCP"), None);
        assert_eq!(server_logo_key("git"), Some("git"));
        assert_eq!(server_logo_key("Git MCP"), Some("git"));
        assert_eq!(server_logo_key("GitHub"), Some("github"));
    }
    use super::server_logo_key;

    #[test]
    fn curated_variants_map_to_provider_marks() {
        assert_eq!(server_logo_key("Stripe (Full API)"), Some("stripe"));
        assert_eq!(server_logo_key("Cloudflare Docs"), Some("cloudflare"));
        assert_eq!(server_logo_key("Jira Production"), Some("jira"));
        assert_eq!(server_logo_key("Trello (work)"), Some("trello"));
        assert_eq!(server_logo_key("RevenueCat"), Some("revenuecat"));
        assert_eq!(
            server_logo_key("Revenue Cat (personal)"),
            Some("revenuecat")
        );
        assert_eq!(server_logo_key("Redis"), Some("redis"));
        assert_eq!(server_logo_key("Postman"), Some("postman"));
        assert_eq!(server_logo_key("private MCP"), None);
    }
}
