#[cfg(test)]
mod tests {
    use super::super::*;
    use super::{remove, DocumentMut};
    use crate::registry::EnvVar;

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new(label: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "toolport-marker-{label}-{}-{nonce}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.0) {
                eprintln!("fixture cleanup failed for {}: {error}", self.0.display());
                if !std::thread::panicking() {
                    panic!("fixture cleanup failed");
                }
            }
        }
    }

    const BLOCK: &str = "# BEGIN PRUNLET MCP\n[mcp_servers.prunlet]\ncommand = 'prunlet'\nargs = ['mcp']\nstartup_timeout_sec = 600\n# END PRUNLET MCP\n";

    fn assert_block(text: &str) {
        let normalized = text.replace("\r\n", "\n");
        assert!(normalized.contains(BLOCK), "{text}");
        assert_eq!(normalized.matches("# BEGIN PRUNLET MCP").count(), 1);
        assert_eq!(normalized.matches("# END PRUNLET MCP").count(), 1);
        let value: toml::Value = toml::from_str(text).unwrap();
        assert_eq!(
            value["mcp_servers"]["prunlet"]["startup_timeout_sec"].as_integer(),
            Some(600)
        );
    }

    #[test]
    fn boundary_comments_survive_gateway_update_and_remove() {
        for newline in ["\n", "\r\n"] {
            let fixture = Fixture::new("toml-marker-gateway");
            let path = fixture.0.join("config.toml");
            let input = format!("{BLOCK}\n[mcp_servers.toolport]\ncommand = 'old'\n\n[profiles.work]\nmodel = 'keep'\n").replace('\n', newline);
            std::fs::write(&path, &input).unwrap();
            let gateway = super::super::tests::sample_gateway(None, "codex");
            edit_toml_gateway(&path, Some(&gateway)).unwrap();
            let updated = std::fs::read_to_string(&path).unwrap();
            assert_block(&updated);
            edit_toml_gateway(&path, Some(&gateway)).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), updated);
            edit_toml_gateway(&path, None).unwrap();
            let removed = std::fs::read_to_string(&path).unwrap();
            assert_block(&removed);
            assert!(removed.contains("[profiles.work]"));
        }
    }

    #[test]
    fn append_after_eof_marker_and_nested_tables_keeps_boundaries() {
        let fixture = Fixture::new("toml-marker-eof");
        let path = fixture.0.join("config.toml");
        std::fs::write(&path, BLOCK).unwrap();
        let gateway = super::super::tests::sample_gateway(None, "codex");
        edit_toml_gateway(&path, Some(&gateway)).unwrap();
        let updated = std::fs::read_to_string(&path).unwrap();
        assert_block(&updated);
        assert!(
            updated.find("# END PRUNLET MCP").unwrap()
                < updated.find("[mcp_servers.toolport]").unwrap()
        );
        edit_toml_gateway(&path, None).unwrap();
        assert_block(&std::fs::read_to_string(&path).unwrap());
        let mut doc: DocumentMut = BLOCK.parse().unwrap();
        let generated = toml_value_to_item(&entry_to_toml(&gateway));
        super::put(&mut doc, "toolport", generated);
        let table = doc["mcp_servers"]["toolport"].as_table().unwrap();
        assert!(table.position().unwrap() < table["env"].as_table().unwrap().position().unwrap());
        assert_block(&doc.to_string());
    }

    #[test]
    fn deletion_prefixes_follow_document_order_outside_server_subtrees() {
        for tail in [
            "",
            "\n[profiles.work.nested]\nmodel = 'keep'\n",
            "\n[[profiles.work.items]]\nmodel = 'keep'\n",
        ] {
            let input = format!("{BLOCK}\n[mcp_servers.conduit]\ncommand = 'old'\n# alias boundary\n[mcp_servers.toolport]\ncommand = 'old'\n[mcp_servers.toolport.env]\nX = 'value'\n{tail}");
            let mut doc: DocumentMut = input.parse().unwrap();
            remove(&mut doc, &["toolport".into(), "conduit".into()]);
            let text = doc.to_string();
            assert_block(&text);
            assert_eq!(text.matches("# alias boundary").count(), 1);
            assert!(
                text.find("# END PRUNLET MCP").unwrap() < text.find("# alias boundary").unwrap()
            );
            if !tail.is_empty() {
                assert!(
                    text.find("# alias boundary").unwrap() < text.find("profiles.work").unwrap()
                );
            }
            remove(&mut doc, &["toolport".into(), "conduit".into()]);
            assert_eq!(doc.to_string(), text);
        }
    }

    #[test]
    fn inventory_keeps_unchanged_raw_entries_and_updates_only_requested_values() {
        let fixture = Fixture::new("toml-marker-inventory");
        let path = fixture.0.join("config.toml");
        let other = "[mcp_servers.'quoted-name'] # keep header\ncommand = 'other' # keep value\nstartup_timeout_sec = 123\n[mcp_servers.'quoted-name'.env]\nTOKEN = 'fixture' # keep env\n[mcp_servers.'quoted-name'.metadata]\nlabel = 'keep'\n";
        let input = format!("{BLOCK}\n[mcp_servers.toolport]\ncommand = 'old'\n\n{other}\n[profiles.work]\nmodel = 'keep'\n");
        std::fs::write(&path, &input).unwrap();
        let mut prunlet = super::super::tests::sample_gateway(None, "codex");
        prunlet.name = "prunlet".into();
        prunlet.command = Some("prunlet".into());
        prunlet.args = vec!["mcp".into()];
        prunlet.env.clear();
        let mut native = prunlet.clone();
        native.name = "quoted-name".into();
        native.command = Some("other".into());
        native.args.clear();
        native.env = vec![EnvVar {
            key: "TOKEN".into(),
            value: Some("fixture".into()),
            secret: false,
            unknown_fields: Default::default(),
        }];
        write_toml(&path, &[prunlet.clone(), native.clone()]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_block(&text);
        assert!(text.contains(other), "{text}");
        write_toml(&path, &[prunlet.clone(), native.clone()]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        native.command = Some("changed".into());
        write_toml(&path, &[prunlet, native]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_block(&text);
        let value: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(
            value["mcp_servers"]["quoted-name"]["command"].as_str(),
            Some("changed")
        );
        assert_eq!(value["profiles"]["work"]["model"].as_str(), Some("keep"));
    }

    #[test]
    fn inline_update_and_removal_keep_key_and_value_comments() {
        let fixture = Fixture::new("toml-inline-marker");
        let path = fixture.0.join("config.toml");
        let input = "[mcp_servers]\n# boundary\n'conduit' = { command = 'old' } # value suffix\n# second boundary\n'other' = { command = 'other', startup_timeout_sec = 123 } # keep other\n\n[profiles.work]\nmodel = 'keep'\n";
        std::fs::write(&path, input).unwrap();
        let gateway = super::super::tests::sample_gateway(None, "codex");
        edit_toml_gateway(&path, Some(&gateway)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# boundary\n"));
        assert!(text.contains("# value suffix"));
        assert!(text
            .contains("'other' = { command = 'other', startup_timeout_sec = 123 } # keep other"));
        assert!(!text.contains("[mcp_servers.toolport]"));
        edit_toml_gateway(&path, None).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("# boundary").count(), 1);
        assert!(text.find("# boundary").unwrap() < text.find("'other'").unwrap());
        let mut doc: DocumentMut = "[mcp_servers]\n# first boundary\na = {command = 'a'}\n# second boundary\nb = {command = 'b'}\nc = {command = 'c'}\n".parse().unwrap();
        remove(&mut doc, &["b".into(), "a".into()]);
        let text = doc.to_string();
        assert!(
            text.find("# first boundary").unwrap() < text.find("# second boundary").unwrap(),
            "{text}"
        );
        assert!(
            text.find("# second boundary").unwrap() < text.find("c =").unwrap(),
            "{text}"
        );
    }

    #[test]
    fn moved_raw_restore_preserves_boundary_once_and_rebases_nested_positions() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("moved");
        let dir = fixture.0.clone();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        for inline in [false, true] {
            let path = dir.join(if inline { "inline.toml" } else { "tables.toml" });
            let input = if inline {
                "[mcp_servers]\n# moved boundary\n'native' = { command = 'native', env = { X = 'fixture' }, metadata = { keep = true } }\n".to_string()
            } else {
                "[mcp_servers.native]\ncommand = 'native'\n[mcp_servers.native.env]\nX = 'fixture'\n# moved boundary\n[mcp_servers.other]\ncommand = 'other'\n".to_string()
            };
            std::fs::write(&path, &input).unwrap();
            moved::record("fixture", Format::TomlMcpServers, &path).unwrap();
            moved::remove_selected(
                Format::TomlMcpServers,
                &path,
                &["native".into(), "other".into()],
            )
            .unwrap();
            let remaining = std::fs::read_to_string(&path).unwrap();
            std::fs::write(&path, format!("{remaining}\n[profiles.work.nested]\nmodel = 'keep'\n[mcp_servers.later]\ncommand = 'later'\n[mcp_servers.later.env]\nY = 'later'\n")).unwrap();
            moved::restore("fixture", Format::TomlMcpServers, &path).unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            assert_eq!(text.matches("# moved boundary").count(), 1, "{text}");
            if !inline {
                assert!(
                    text.find("[mcp_servers.later.env]").unwrap()
                        < text.find("[mcp_servers.native]").unwrap(),
                    "{text}"
                );
            }
            let parsed: toml::Value = toml::from_str(&text).unwrap();
            assert_eq!(
                parsed["mcp_servers"]["native"]["env"]["X"].as_str(),
                Some("fixture")
            );
            assert_eq!(
                parsed["profiles"]["work"]["nested"]["model"].as_str(),
                Some("keep")
            );
        }
    }

    #[test]
    fn restore_apply_merges_drift_without_losing_markers_or_native_raw_fields() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("apply");
        let dir = fixture.0.clone();
        let _data = crate::registry::DataDirOverride::set(dir.join("data"));
        for moved in [false, true] {
            let path = dir.join(if moved { "moved.toml" } else { "changed.toml" });
            let native = "[mcp_servers.'native']\ncommand = 'original'\nstartup_timeout_sec = 123\n[mcp_servers.'native'.env]\nTOKEN = 'fixture'\n";
            std::fs::write(
                &path,
                format!("{BLOCK}\n{native}\n[profiles.work]\nmodel = 'before'\n"),
            )
            .unwrap();
            let gateway = super::super::tests::sample_gateway(None, "codex");
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                if moved {
                    moved::record("fixture", Format::TomlMcpServers, &path)?;
                    moved::remove_selected(Format::TomlMcpServers, &path, &["native".into()])?;
                } else {
                    let mut doc = load_toml_document(&path)?;
                    let mut changed = gateway.clone();
                    changed.command = Some("changed".into());
                    changed.env.clear();
                    super::put(
                        &mut doc,
                        "native",
                        toml_value_to_item(&entry_to_toml(&changed)),
                    );
                    atomic_write(&path, &doc.to_string())?;
                }
                edit_toml_gateway(&path, Some(&gateway))
            })
            .unwrap();
            let edited = read_config_file(&path)
                .unwrap()
                .replace("model = 'before'", "model = 'user-edit'");
            std::fs::write(&path, edited).unwrap();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
                Ok(())
            })
            .unwrap();
            let text = read_config_file(&path).unwrap();
            assert_block(&text);
            let parsed: toml::Value = toml::from_str(&text).unwrap();
            assert!(parsed["mcp_servers"].get("toolport").is_none());
            assert_eq!(
                parsed["mcp_servers"]["native"]["command"].as_str(),
                Some("original")
            );
            assert_eq!(
                parsed["mcp_servers"]["native"]["startup_timeout_sec"].as_integer(),
                Some(123)
            );
            assert_eq!(
                parsed["mcp_servers"]["native"]["env"]["TOKEN"].as_str(),
                Some("fixture")
            );
            assert_eq!(
                parsed["profiles"]["work"]["model"].as_str(),
                Some("user-edit")
            );
        }
    }

    #[test]
    fn root_inline_map_preserves_boundary_keys_and_unrelated_settings() {
        for tail in ["", "\n[profiles.work]\nmodel = 'keep'\n"] {
            let fixture = Fixture::new("root-inline");
            let path = fixture.0.join("config.toml");
            let input = format!("theme = 'keep'\n# BEGIN PRUNLET MCP\n'mcp_servers' = {{ 'prunlet' = {{ command = 'prunlet', startup_timeout_sec = 600, metadata = {{ keep = true }} }} }} # END PRUNLET MCP\n# root setting comment\nmodel = 'root-model'\n{tail}");
            std::fs::write(&path, &input).unwrap();
            let gateway = super::super::tests::sample_gateway(None, "codex");
            edit_toml_gateway(&path, Some(&gateway)).unwrap();
            let updated = read_config_file(&path).unwrap();
            assert_eq!(updated.matches("# END PRUNLET MCP").count(), 1, "{updated}");
            assert!(
                updated.find("'prunlet'").unwrap() < updated.find("# END PRUNLET MCP").unwrap(),
                "{updated}"
            );
            assert!(
                updated.find("# END PRUNLET MCP").unwrap() < updated.find("toolport").unwrap(),
                "{updated}"
            );
            assert!(updated.contains("'prunlet' ="), "{updated}");
            let parsed: toml::Value = toml::from_str(&updated).unwrap();
            assert_eq!(parsed["theme"].as_str(), Some("keep"));
            assert_eq!(parsed["model"].as_str(), Some("root-model"));
            assert!(updated.contains("# root setting comment"));
            assert_eq!(
                parsed["mcp_servers"]["prunlet"]["metadata"]["keep"].as_bool(),
                Some(true)
            );
            edit_toml_gateway(&path, Some(&gateway)).unwrap();
            assert_eq!(read_config_file(&path).unwrap(), updated);
            edit_toml_gateway(&path, None).unwrap();
            let removed = read_config_file(&path).unwrap();
            assert_eq!(removed.matches("# END PRUNLET MCP").count(), 1);
            let mut prunlet = gateway;
            prunlet.name = "prunlet".into();
            prunlet.command = Some("prunlet".into());
            prunlet.env.clear();
            std::fs::write(&path, &input).unwrap();
            write_toml(&path, &[prunlet]).unwrap();
            let written = read_config_file(&path).unwrap();
            assert_eq!(written.matches("# END PRUNLET MCP").count(), 1);
            assert!(written.contains("metadata = { keep = true }"), "{written}");
        }
    }

    #[test]
    fn restore_missing_original_prefix_uses_transfer_evidence_and_preserves_user_edits() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("missing-original");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        for mode in ["retained", "legacy-drop", "user-prefix-edit"] {
            let path = fixture.0.join(format!("{mode}.toml"));
            std::fs::write(&path, format!("{BLOCK}\n[mcp_servers.native]\ncommand = 'native'\nstartup_timeout_sec = 123\n\n[profiles.work]\nmodel = 'before'\n")).unwrap();
            let mut prunlet = super::super::tests::sample_gateway(None, "codex");
            prunlet.name = "prunlet".into();
            prunlet.command = Some("prunlet".into());
            prunlet.args = vec!["mcp".into()];
            prunlet.env.clear();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                write_toml(&path, &[prunlet.clone()])?;
                if mode == "legacy-drop" {
                    let output = read_config_file(&path)?.replace("# END PRUNLET MCP\n", "");
                    atomic_write(&path, &output)?;
                }
                Ok(())
            })
            .unwrap();
            let mut edited = read_config_file(&path)
                .unwrap()
                .replace("model = 'before'", "model = 'user-edit'");
            if mode == "user-prefix-edit" {
                edited = edited.replace("# END PRUNLET MCP", "# user boundary");
            }
            if mode == "legacy-drop" {
                edited = edited.replace("[profiles.work]", "# user boundary\n[profiles.work]");
            }
            std::fs::write(&path, edited).unwrap();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
                Ok(())
            })
            .unwrap();
            let text = read_config_file(&path).unwrap();
            assert_eq!(
                text.matches("# BEGIN PRUNLET MCP").count(),
                1,
                "{mode}: {text}"
            );
            assert_eq!(
                text.matches("# END PRUNLET MCP").count(),
                1,
                "{mode}: {text}"
            );
            assert_eq!(
                text.matches("# user boundary").count(),
                usize::from(mode != "retained"),
                "{mode}: {text}"
            );
            let parsed: toml::Value = toml::from_str(&text).unwrap();
            assert_eq!(
                parsed["mcp_servers"]["native"]["startup_timeout_sec"].as_integer(),
                Some(123)
            );
            assert_eq!(
                parsed["profiles"]["work"]["model"].as_str(),
                Some("user-edit")
            );
        }
    }

    #[test]
    fn restore_missing_eof_original_after_gateway_append_keeps_single_end_marker() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("missing-eof-original");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        let path = fixture.0.join("config.toml");
        std::fs::write(&path, format!("model = 'before'\n{BLOCK}\n[mcp_servers.native]\ncommand = 'native'\nstartup_timeout_sec = 123\n")).unwrap();
        let gateway = super::super::tests::sample_gateway(None, "codex");
        let mut prunlet = gateway.clone();
        prunlet.name = "prunlet".into();
        prunlet.command = Some("prunlet".into());
        prunlet.args = vec!["mcp".into()];
        prunlet.env.clear();
        mutation::run("fixture", &path, Format::TomlMcpServers, || {
            write_toml(&path, &[prunlet.clone(), gateway.clone()])
        })
        .unwrap();
        let edited = read_config_file(&path)
            .unwrap()
            .replace("model = 'before'", "model = 'user-edit'");
        std::fs::write(&path, edited).unwrap();
        mutation::run("fixture", &path, Format::TomlMcpServers, || {
            assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
            Ok(())
        })
        .unwrap();
        let text = read_config_file(&path).unwrap();
        assert_eq!(text.matches("# BEGIN PRUNLET MCP").count(), 1, "{text}");
        assert_eq!(text.matches("# END PRUNLET MCP").count(), 1, "{text}");
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(parsed["model"].as_str(), Some("user-edit"));
        assert_eq!(
            parsed["mcp_servers"]["native"]["startup_timeout_sec"].as_integer(),
            Some(123)
        );
    }
}
use toml_edit::{DocumentMut, Item, Table};

// rootのinline mapを展開しても、境界のコメントは元のentriesの外側へ残す。
pub(super) fn expand_container(doc: &mut DocumentMut) {
    let Some(inline) = doc
        .get("mcp_servers")
        .and_then(Item::as_inline_table)
        .cloned()
    else {
        return;
    };
    let mut key = doc.as_table().key("mcp_servers").unwrap().clone();
    let prefix = key
        .leaf_decor()
        .prefix()
        .and_then(|raw| raw.as_str())
        .unwrap_or("")
        .to_string();
    let suffix = inline
        .decor()
        .suffix()
        .and_then(|raw| raw.as_str())
        .unwrap_or("")
        .to_string();
    key.leaf_decor_mut().clear();
    let mut table = Table::new();
    table.set_position(0);
    table.decor_mut().set_prefix(prefix);
    for (name, value) in inline.iter() {
        table.insert_formatted(inline.key(name).unwrap(), Item::Value(value.clone()));
    }
    doc.as_table_mut()
        .insert_formatted(&key, Item::Table(table));
    if suffix.contains('#') {
        let comment = format!("{}\n", suffix.trim_start());
        let mut next = None;
        visit(doc.as_table_mut(), &mut |table| {
            if !table.is_implicit() || !table.get_values().is_empty() {
                if let Some(position) = table.position().filter(|position| *position > 0) {
                    if next.is_none_or(|current| position < current) {
                        next = Some(position);
                    }
                }
            }
        });
        if let Some(next) = next {
            visit(doc.as_table_mut(), &mut |table| {
                if table.position() == Some(next) {
                    let prefix = table
                        .decor()
                        .prefix()
                        .and_then(|raw| raw.as_str())
                        .unwrap_or("")
                        .to_string();
                    table.decor_mut().set_prefix(format!("{comment}{prefix}"));
                }
            });
        } else {
            doc.set_trailing(format!(
                "{comment}{}",
                doc.trailing().as_str().unwrap_or("")
            ));
        }
    }
}

pub(super) fn put(doc: &mut DocumentMut, name: &str, mut item: Item) {
    if doc
        .get("mcp_servers")
        .and_then(|servers| servers.get(name))
        .is_none()
    {
        let mut maximum = 0;
        visit(doc.as_table_mut(), &mut |table| {
            maximum = maximum.max(table.position().unwrap_or(0));
        });
        if let Some(table) = item.as_table_mut() {
            let mut positions = Vec::new();
            visit(table, &mut |table| {
                positions.push(table.position().unwrap_or(usize::MAX));
            });
            let mut order: Vec<_> = (0..positions.len()).collect();
            order.sort_by_key(|index| (positions[*index], *index));
            let mut ranks = vec![0; positions.len()];
            for (rank, index) in order.into_iter().enumerate() {
                ranks[index] = rank;
            }
            let mut index = 0;
            visit(table, &mut |table| {
                table.set_position(maximum + 1 + ranks[index]);
                index += 1;
            });
            let trailing = doc.trailing().as_str().unwrap_or("").to_string();
            if !trailing.is_empty() {
                let prefix = table
                    .decor()
                    .prefix()
                    .and_then(|raw| raw.as_str())
                    .unwrap_or("")
                    .to_string();
                table.decor_mut().set_prefix(format!("{trailing}{prefix}"));
                doc.set_trailing("");
            }
        }
    }
    insert(super::toml_mcp_servers_mut(doc), name, item);
}

pub(super) fn rename(doc: &mut DocumentMut, old: &str, new: &str, item: Item) {
    let servers = super::toml_mcp_servers_mut(doc);
    insert(servers, old, item);
    let mut key = toml_edit::Key::new(new);
    *key.leaf_decor_mut() = servers.key(old).unwrap().leaf_decor().clone();
    let entries: Vec<_> = servers
        .iter()
        .map(|(name, item)| {
            let key = if name == old {
                key.clone()
            } else {
                servers.key(name).unwrap().clone()
            };
            (key, item.clone())
        })
        .collect();
    // clearは見出しのdecor・position・implicitを変えず、keyの元のslotを再現する。
    servers.clear();
    for (key, item) in entries {
        servers.insert_formatted(&key, item);
    }
}

// 同名の値だけを更新し、見出しの位置・コメント・引用されたkeyを引き継ぐ。
pub(super) fn insert(servers: &mut Table, name: &str, mut item: Item) {
    let key = servers.key(name).cloned();
    if let Some(old) = servers.get(name) {
        if let (Some(old), Some(new)) = (old.as_table(), item.as_table_mut()) {
            *new.decor_mut() = old.decor().clone();
            if let Some(position) = old.position() {
                new.set_position(position);
            }
        } else if let Some(old) = old.as_value() {
            if let Item::Table(table) = item {
                item = Item::Value(toml_edit::Value::InlineTable(table.into_inline_table()));
            }
            if let Some(new) = item.as_value_mut() {
                *new.decor_mut() = old.decor().clone();
            }
        }
    }
    if let Some(key) = key {
        servers.insert_formatted(&key, item);
    } else {
        servers.insert(name, item);
    }
}

fn visit(table: &mut Table, f: &mut impl FnMut(&mut Table)) {
    f(table);
    for (_, item) in table.iter_mut() {
        if let Some(table) = item.as_table_mut() {
            visit(table, f);
        } else if let Some(tables) = item.as_array_of_tables_mut() {
            for table in tables.iter_mut() {
                visit(table, f);
            }
        }
    }
}

#[derive(Clone)]
enum PrefixDestination {
    Table(Vec<(String, Option<usize>)>),
    InlineKey(String),
    Eof,
}

pub(super) struct PrefixTransfer {
    source_name: String,
    prefix: String,
    destination: PrefixDestination,
    expected_destination_prefix: String,
}

fn table_path(table: &Table, position: usize) -> Option<Vec<(String, Option<usize>)>> {
    for (name, item) in table.iter() {
        if let Some(child) = item.as_table() {
            let step = (name.to_string(), None);
            if child.position() == Some(position) {
                return Some(vec![step]);
            }
            if let Some(mut rest) = table_path(child, position) {
                rest.insert(0, step);
                return Some(rest);
            }
        } else if let Some(children) = item.as_array_of_tables() {
            for (index, child) in children.iter().enumerate() {
                let step = (name.to_string(), Some(index));
                if child.position() == Some(position) {
                    return Some(vec![step]);
                }
                if let Some(mut rest) = table_path(child, position) {
                    rest.insert(0, step);
                    return Some(rest);
                }
            }
        }
    }
    None
}

fn destination_prefix<'a>(
    doc: &'a DocumentMut,
    destination: &PrefixDestination,
) -> Option<&'a str> {
    let raw = match destination {
        PrefixDestination::Eof => return doc.trailing().as_str(),
        PrefixDestination::InlineKey(name) => doc
            .get("mcp_servers")?
            .as_table()?
            .key(name)?
            .leaf_decor()
            .prefix(),
        PrefixDestination::Table(path) => {
            let mut table = doc.as_table();
            for (name, index) in path {
                let item = table.get(name)?;
                table = if let Some(index) = index {
                    item.as_array_of_tables()?.get(*index)?
                } else {
                    item.as_table()?
                };
            }
            table.decor().prefix()
        }
    };
    Some(raw.and_then(|raw| raw.as_str()).unwrap_or(""))
}

// 原本から実際に転送されたprefixが同じ移送先へ残る証拠がある場合だけ重複を省く。
pub(super) fn retained_prefix(
    original: &DocumentMut,
    written: &DocumentMut,
    current: &DocumentMut,
) -> Vec<String> {
    let missing: Vec<_> = original
        .get("mcp_servers")
        .and_then(Item::as_table_like)
        .map(|servers| {
            servers
                .iter()
                .filter(|(name, _)| {
                    written
                        .get("mcp_servers")
                        .and_then(|servers| servers.get(name))
                        .is_none()
                })
                .map(|(name, _)| name.to_string())
                .collect()
        })
        .unwrap_or_default();
    let mut replay = original.clone();
    remove(&mut replay, &missing)
        .into_iter()
        .filter(|transfer| {
            if transfer.prefix.is_empty() {
                return false;
            }
            let mut destination = transfer.destination.clone();
            if destination_prefix(written, &destination)
                != Some(transfer.expected_destination_prefix.as_str())
            {
                if !matches!(destination, PrefixDestination::Eof) {
                    return false;
                }
                // appendがEOFから最初の新規server見出しへ移した場合だけ、その構造位置を照合する。
                let first = written
                    .get("mcp_servers")
                    .and_then(Item::as_table_like)
                    .and_then(|servers| {
                        servers
                            .iter()
                            .filter(|(name, _)| {
                                original
                                    .get("mcp_servers")
                                    .and_then(|servers| servers.get(name))
                                    .is_none()
                            })
                            .filter_map(|(name, item)| {
                                item.as_table()
                                    .filter(|table| {
                                        !table.is_implicit() || !table.get_values().is_empty()
                                    })
                                    .and_then(|table| {
                                        table
                                            .position()
                                            .map(|position| (position, name.to_string()))
                                    })
                            })
                            .min_by_key(|(position, _)| *position)
                    });
                let Some((_, name)) = first else { return false };
                destination =
                    PrefixDestination::Table(vec![("mcp_servers".into(), None), (name, None)]);
                if !destination_prefix(written, &destination)
                    .is_some_and(|prefix| prefix.starts_with(&transfer.expected_destination_prefix))
                {
                    return false;
                }
            }
            destination_prefix(current, &destination)
                .is_some_and(|prefix| prefix.contains(&transfer.prefix))
        })
        .map(|transfer| transfer.source_name)
        .collect()
}

// 削除対象の外側にあるprefixだけを、文書上の次の見出し（なければ末尾）へ残す。
pub(super) fn remove(doc: &mut DocumentMut, names: &[String]) -> Vec<PrefixTransfer> {
    let mut prefixes = Vec::new();
    let mut transfers = Vec::new();
    {
        let servers = super::toml_mcp_servers_mut(doc);
        let order: Vec<_> = servers.iter().map(|(name, _)| name.to_string()).collect();
        let ordered: Vec<_> = order.iter().filter(|name| names.contains(name)).collect();
        let mut inline_prefixes = std::collections::BTreeMap::<String, String>::new();
        for name in ordered {
            let Some(item) = servers.get(name) else {
                continue;
            };
            if let Some(table) = item.as_table() {
                if let Some(prefix) = table.decor().prefix().and_then(|raw| raw.as_str()) {
                    prefixes.push((
                        table.position().unwrap_or(usize::MAX),
                        name.clone(),
                        prefix.to_string(),
                    ));
                }
            } else {
                let prefix = servers
                    .key(name)
                    .and_then(|key| key.leaf_decor().prefix())
                    .and_then(|raw| raw.as_str())
                    .unwrap_or("")
                    .to_string();
                let next = order
                    .iter()
                    .skip_while(|key| *key != name)
                    .skip(1)
                    .find(|key| {
                        !names.contains(key) && servers.get(key).is_some_and(Item::is_value)
                    });
                if let Some(next) = next {
                    transfers.push(PrefixTransfer {
                        source_name: name.clone(),
                        prefix: prefix.clone(),
                        destination: PrefixDestination::InlineKey(next.clone()),
                        expected_destination_prefix: String::new(),
                    });
                    inline_prefixes
                        .entry(next.clone())
                        .or_default()
                        .push_str(&prefix);
                } else {
                    prefixes.push((
                        servers.position().unwrap_or(usize::MAX),
                        name.clone(),
                        prefix,
                    ));
                }
            }
        }
        for (next, prefix) in inline_prefixes {
            let mut key = servers.key_mut(&next).unwrap();
            let old = key
                .leaf_decor()
                .prefix()
                .and_then(|raw| raw.as_str())
                .unwrap_or("")
                .to_string();
            key.leaf_decor_mut().set_prefix(format!("{prefix}{old}"));
        }
        for name in names {
            servers.remove(name);
        }
    }
    prefixes.sort_by_key(|(position, _, _)| *position);
    let mut destinations = std::collections::BTreeMap::<Option<usize>, String>::new();
    for (position, name, prefix) in prefixes {
        let mut next = None;
        visit(doc.as_table_mut(), &mut |table| {
            if !table.is_implicit() || !table.get_values().is_empty() {
                if let Some(candidate) = table.position().filter(|candidate| *candidate > position)
                {
                    if next.is_none_or(|current| candidate < current) {
                        next = Some(candidate);
                    }
                }
            }
        });
        let destination = next
            .and_then(|position| table_path(doc.as_table(), position))
            .map(PrefixDestination::Table)
            .unwrap_or(PrefixDestination::Eof);
        transfers.push(PrefixTransfer {
            source_name: name,
            prefix: prefix.clone(),
            destination,
            expected_destination_prefix: String::new(),
        });
        destinations.entry(next).or_default().push_str(&prefix);
    }
    for (position, prefix) in destinations {
        if let Some(position) = position {
            visit(doc.as_table_mut(), &mut |table| {
                if table.position() == Some(position)
                    && (!table.is_implicit() || !table.get_values().is_empty())
                {
                    let old = table
                        .decor()
                        .prefix()
                        .and_then(|raw| raw.as_str())
                        .unwrap_or("")
                        .to_string();
                    table.decor_mut().set_prefix(format!("{prefix}{old}"));
                }
            });
        } else {
            doc.set_trailing(format!("{prefix}{}", doc.trailing().as_str().unwrap_or("")));
        }
    }
    for transfer in &mut transfers {
        transfer.expected_destination_prefix = destination_prefix(doc, &transfer.destination)
            .unwrap_or("")
            .to_string();
    }
    transfers
}
