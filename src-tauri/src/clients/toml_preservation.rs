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
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                moved::record("fixture", Format::TomlMcpServers, &path)?;
                moved::remove_selected(
                    Format::TomlMcpServers,
                    &path,
                    &["native".into(), "other".into()],
                )?;
                edit_toml_gateway(
                    &path,
                    Some(&super::super::tests::sample_gateway(None, "codex")),
                )
            })
            .unwrap();
            let remaining = std::fs::read_to_string(&path).unwrap();
            std::fs::write(&path, format!("{remaining}\n[profiles.work.nested]\nmodel = 'keep'\n[mcp_servers.later]\ncommand = 'later'\n[mcp_servers.later.env]\nY = 'later'\n")).unwrap();
            moved::restore("fixture", Format::TomlMcpServers, &path).unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            assert_eq!(text.matches("# moved boundary").count(), 1, "{text}");
            if !inline {
                assert!(
                    text.find("[mcp_servers.native.env]").unwrap()
                        < text.find("[mcp_servers.other]").unwrap()
                        && text.find("[mcp_servers.other]").unwrap()
                            < text.find("[mcp_servers.toolport]").unwrap()
                        && text.find("[mcp_servers.toolport.env]").unwrap()
                            < text.find("[mcp_servers.later]").unwrap(),
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

    #[test]
    fn followup_eof_comment_terminates_before_appended_gateway_header() {
        let fixture = Fixture::new("eof-comment-line");
        let path = fixture.0.join("config.toml");
        let gateway = super::super::tests::sample_gateway(None, "codex");
        for newline in ["\n", "\r\n"] {
            for ending in ["", "\n", "\r\n"] {
                let input = BLOCK.trim_end_matches('\n').replace('\n', newline) + ending;
                std::fs::write(&path, input).unwrap();
                edit_toml_gateway(&path, Some(&gateway)).unwrap();
                let output = read_config_file(&path).unwrap();
                let parsed: toml::Value = toml::from_str(&output).unwrap();
                assert_block(&output);
                assert_eq!(
                    parsed["mcp_servers"]["toolport"]["command"].as_str(),
                    Some("toolport-gateway")
                );
                assert_eq!(
                    parsed["mcp_servers"]["prunlet"]["command"].as_str(),
                    Some("prunlet")
                );
            }
        }
        let mut doc: DocumentMut = "model = 'keep'\n \t".parse().unwrap();
        super::put(
            &mut doc,
            "toolport",
            toml_value_to_item(&entry_to_toml(&gateway)),
        );
        let output = doc.to_string();
        assert!(output.contains("\n \t[mcp_servers.toolport]"), "{output}");
        assert!(!output.contains("\n \t\n"), "{output}");
        let _: toml::Value = toml::from_str(&output).unwrap();
        for newline in ["\n", "\r\n"] {
            let mut doc: DocumentMut = BLOCK.trim_end_matches('\n').parse().unwrap();
            let mut item = toml_value_to_item(&entry_to_toml(&gateway));
            item.as_table_mut()
                .unwrap()
                .decor_mut()
                .set_prefix(format!("{newline}# incoming comment\n"));
            super::put(&mut doc, "toolport", item);
            let output = doc.to_string();
            assert!(
                output
                    .replace("\r\n", "\n")
                    .contains("# END PRUNLET MCP\n# incoming comment\n"),
                "{output}"
            );
            let _: toml::Value = toml::from_str(&output).unwrap();
        }
    }

    #[test]
    fn followup_whitespace_only_prefix_is_not_transferred_or_retained_as_evidence() {
        for inline in [false, true] {
            for prefix in ["\n\n", "\n# retained boundary\n"] {
                let input = if inline {
                    format!("[mcp_servers]\nprunlet = {{command = 'prunlet'}}\n{prefix}toolport = {{command = 'old'}}\nother = {{command = 'other'}}\n")
                } else {
                    format!("[mcp_servers.prunlet]\ncommand = 'prunlet'\n{prefix}[mcp_servers.toolport]\ncommand = 'old'\n[profiles.work]\nmodel = 'keep'\n")
                };
                let original: DocumentMut = input.parse().unwrap();
                let mut doc = original.clone();
                let transfers = remove(&mut doc, &["toolport".into()]);
                assert_eq!(transfers.is_empty(), !prefix.contains('#'));
                assert_eq!(
                    doc.to_string().matches("# retained boundary").count(),
                    usize::from(prefix.contains('#'))
                );
                if !prefix.contains('#') {
                    assert!(super::retained_prefix(&original, &doc, &doc).is_empty());
                }
                let stable = doc.to_string();
                for _ in 0..3 {
                    let gateway = super::super::tests::sample_gateway(None, "codex");
                    super::put(
                        &mut doc,
                        "toolport",
                        toml_value_to_item(&entry_to_toml(&gateway)),
                    );
                    doc = doc.to_string().parse().unwrap();
                    remove(&mut doc, &["toolport".into()]);
                    assert_eq!(doc.to_string(), stable);
                }
            }
        }
    }
    #[test]
    fn moved_comment_is_restored_after_user_removes_its_transfer_destination() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("moved-comment-destination-removed");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        let path = fixture.0.join("config.toml");
        std::fs::write(&path, "# native boundary\n[mcp_servers.native]\ncommand = 'native'\nstartup_timeout_sec = 123\n\n[profiles.work]\nmodel = 'before'\n").unwrap();
        let gateway = super::super::tests::sample_gateway(None, "codex");
        mutation::run("fixture", &path, Format::TomlMcpServers, || {
            moved::record("fixture", Format::TomlMcpServers, &path)?;
            write_toml(&path, std::slice::from_ref(&gateway))
        })
        .unwrap();
        let mut edited: DocumentMut = read_config_file(&path).unwrap().parse().unwrap();
        assert!(edited.to_string().contains("# native boundary"));
        edited.remove("profiles");
        assert!(!edited.to_string().contains("# native boundary"));
        std::fs::write(&path, edited.to_string()).unwrap();
        mutation::run("fixture", &path, Format::TomlMcpServers, || {
            assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
            Ok(())
        })
        .unwrap();
        let restored = read_config_file(&path).unwrap();
        assert_eq!(
            restored.matches("# native boundary").count(),
            1,
            "{restored}"
        );
        let parsed: toml::Value = toml::from_str(&restored).unwrap();
        assert_eq!(
            parsed["mcp_servers"]["native"]["command"].as_str(),
            Some("native")
        );
        assert_eq!(
            parsed["mcp_servers"]["native"]["startup_timeout_sec"].as_integer(),
            Some(123)
        );
        assert!(parsed.get("profiles").is_none());
    }

    #[test]
    fn latest_moved_raw_values_and_prefix_survive_added_and_repeated_moves() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("latest-moved-prefix");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        for inline in [false, true] {
            for added in [false, true] {
                for keep_destination in [false, true] {
                    for direct in [false, true] {
                        let path = fixture
                            .0
                            .join(format!("{inline}-{added}-{keep_destination}-{direct}.toml"));
                        let config = |prefix: &str, command: &str| {
                            let native = if inline {
                                format!("[mcp_servers]\n{prefix}\n'native' = {{command = '{command}', env = {{ TOKEN = '{command}' }}, metadata = {{ revision = '{command}' }}}}\n")
                            } else {
                                format!("{prefix}\n[mcp_servers.'native']\ncommand = '{command}'\n[mcp_servers.'native'.env]\nTOKEN = '{command}'\n[mcp_servers.'native'.metadata]\nrevision = '{command}'\n")
                            };
                            format!(
                                "model = 'before'\n{native}# END LATEST\n[profiles.work]\nmodel = 'profile'\n"
                            )
                        };
                        let initial = if added {
                            "model = 'before'\n[profiles.work]\nmodel = 'profile'\n".to_string()
                        } else {
                            config("# original boundary", "original")
                        };
                        std::fs::write(&path, initial).unwrap();
                        let gateway = super::super::tests::sample_gateway(None, "codex");
                        mutation::run("fixture", &path, Format::TomlMcpServers, || {
                            if !added {
                                moved::record("fixture", Format::TomlMcpServers, &path)?;
                            }
                            write_toml(&path, std::slice::from_ref(&gateway))
                        })
                        .unwrap();
                        let connected: DocumentMut =
                            read_config_file(&path).unwrap().parse().unwrap();
                        let mut edited: DocumentMut =
                            config("# BEGIN LATEST", "latest").parse().unwrap();
                        super::put(
                            &mut edited,
                            "toolport",
                            connected["mcp_servers"]["toolport"].clone(),
                        );
                        std::fs::write(&path, edited.to_string()).unwrap();
                        mutation::run("fixture", &path, Format::TomlMcpServers, || {
                            moved::record("fixture", Format::TomlMcpServers, &path)?;
                            write_toml(&path, std::slice::from_ref(&gateway))
                        })
                        .unwrap();
                        let mut edited: DocumentMut = read_config_file(&path)
                            .unwrap()
                            .replace("model = 'before'", "model = 'user-edit'")
                            .parse()
                            .unwrap();
                        if !keep_destination {
                            edited.remove("profiles");
                        }
                        std::fs::write(&path, edited.to_string()).unwrap();
                        mutation::run("fixture", &path, Format::TomlMcpServers, || {
                            if direct {
                                moved::restore("fixture", Format::TomlMcpServers, &path)?;
                            } else {
                                assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
                            }
                            Ok(())
                        })
                        .unwrap();
                        let restored = read_config_file(&path).unwrap();
                        assert_eq!(
                            restored.matches("# BEGIN LATEST").count(),
                            1,
                            "{inline}-{added}-{keep_destination}-{direct}: {restored}"
                        );
                        if keep_destination {
                            assert_eq!(restored.matches("# END LATEST").count(), 1);
                            let begin = restored.find("# BEGIN LATEST").unwrap();
                            let native = restored
                                .find(if inline {
                                    "'native' ="
                                } else {
                                    "[mcp_servers.'native']"
                                })
                                .unwrap();
                            let end = restored.find("# END LATEST").unwrap();
                            assert!(
                                begin < native && native < end,
                                "{inline}-{added}-{keep_destination}-{direct}: {restored}"
                            );
                        }
                        let parsed: toml::Value = toml::from_str(&restored).unwrap();
                        assert_eq!(
                            parsed["mcp_servers"]["native"]["command"].as_str(),
                            Some("latest")
                        );
                        assert_eq!(
                            parsed["mcp_servers"]["native"]["env"]["TOKEN"].as_str(),
                            Some("latest")
                        );
                        assert_eq!(
                            parsed["mcp_servers"]["native"]["metadata"]["revision"].as_str(),
                            Some("latest")
                        );
                        assert_eq!(parsed["model"].as_str(), Some("user-edit"));
                        assert_eq!(parsed.get("profiles").is_some(), keep_destination);
                    }
                }
            }
        }
    }

    #[test]
    fn latest_raw_prefix_witnesses_require_unique_structural_line_matches() {
        let original: DocumentMut = "[profiles.work]\nmodel = 'keep'\n".parse().unwrap();
        let written: DocumentMut =
            "# first boundary\n# second boundary\n[profiles.work]\nmodel = 'keep'\n"
                .parse()
                .unwrap();
        let raw = |name: &str, prefix: &str| {
            let doc: DocumentMut = format!("{prefix}[mcp_servers.'{name}']\ncommand = '{name}'\n")
                .parse()
                .unwrap();
            super::raw_entry(&doc, name).unwrap()
        };
        let mut restored = written.clone();
        for (name, prefix) in [
            ("one", "# first boundary\n"),
            ("two", "# second boundary\n"),
        ] {
            let (mut key, mut item) = raw(name, prefix);
            super::dedup_raw_prefix(&original, &written, &written, &mut key, &mut item);
            super::put_raw(&mut restored, key, item);
        }
        assert_eq!(restored.to_string().matches("# first boundary").count(), 1);
        assert_eq!(restored.to_string().matches("# second boundary").count(), 1);
        let repeated: DocumentMut =
            "# first boundary\n# first boundary\n[profiles.work]\nmodel = 'keep'\n"
                .parse()
                .unwrap();
        let multiple: DocumentMut = "# first boundary\n[profiles.work]\nmodel = 'keep'\n# first boundary\n[profiles.other]\nmodel = 'keep'\n".parse().unwrap();
        let mut missing = written.clone();
        missing.remove("profiles");
        for (before, after, current) in [
            (&original, &repeated, &repeated),
            (&original, &multiple, &multiple),
            (&written, &written, &written),
            (&original, &written, &missing),
        ] {
            let (mut key, mut item) = raw("one", "# first boundary\n");
            super::dedup_raw_prefix(before, after, current, &mut key, &mut item);
            assert_eq!(super::entry_prefix(&key, &item), "# first boundary\n");
        }
    }
    #[test]
    fn moved_entry_returns_inside_its_original_comment_boundaries_after_profile_edit() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("moved-entry-boundary-order");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        for inline in [false, true] {
            for eof in [false, true] {
                for direct in [false, true] {
                    let path = fixture.0.join(format!("{inline}-{eof}-{direct}.toml"));
                    let entries = if inline {
                        "[mcp_servers]\n# BEGIN NATIVE\n'zebra' = {command = 'zebra', env = {TOKEN = 'keep'}}\n'alpha' = {command = 'alpha'}\n# END NATIVE"
                    } else {
                        "# BEGIN NATIVE\n[mcp_servers.zebra]\ncommand = 'zebra'\n[mcp_servers.zebra.env]\nTOKEN = 'keep'\n[mcp_servers.alpha]\ncommand = 'alpha'\n# END NATIVE"
                    };
                    let tail = if eof {
                        ""
                    } else {
                        "\n[profiles.work]\nmodel = 'before'\n"
                    };
                    std::fs::write(&path, format!("model = 'before'\n{entries}{tail}")).unwrap();
                    let gateway = super::super::tests::sample_gateway(None, "codex");
                    mutation::run("fixture", &path, Format::TomlMcpServers, || {
                        moved::record("fixture", Format::TomlMcpServers, &path)?;
                        write_toml(&path, std::slice::from_ref(&gateway))
                    })
                    .unwrap();
                    let edited = read_config_file(&path)
                        .unwrap()
                        .replace("model = 'before'", "model = 'user-edit'");
                    std::fs::write(&path, edited).unwrap();
                    mutation::run("fixture", &path, Format::TomlMcpServers, || {
                        if direct {
                            moved::restore("fixture", Format::TomlMcpServers, &path)?;
                        } else {
                            assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
                        }
                        Ok(())
                    })
                    .unwrap();
                    let restored = read_config_file(&path).unwrap();
                    assert_eq!(restored.matches("# BEGIN NATIVE").count(), 1);
                    assert_eq!(restored.matches("# END NATIVE").count(), 1);
                    let begin = restored.find("# BEGIN NATIVE").unwrap();
                    let zebra = restored
                        .find(if inline {
                            "'zebra' ="
                        } else {
                            "[mcp_servers.zebra]"
                        })
                        .unwrap();
                    let alpha = restored
                        .find(if inline {
                            "'alpha' ="
                        } else {
                            "[mcp_servers.alpha]"
                        })
                        .unwrap();
                    let end = restored.find("# END NATIVE").unwrap();
                    assert!(
                        begin < zebra && zebra < alpha && alpha < end,
                        "{inline}-{eof}-{direct}: {restored}"
                    );
                    if !inline {
                        assert!(restored.find("[mcp_servers.zebra.env]").unwrap() < alpha);
                    }
                    let parsed: toml::Value = toml::from_str(&restored).unwrap();
                    assert_eq!(
                        parsed["mcp_servers"]["zebra"]["env"]["TOKEN"].as_str(),
                        Some("keep")
                    );
                    assert_eq!(
                        parsed["mcp_servers"]["alpha"]["command"].as_str(),
                        Some("alpha")
                    );
                    assert_eq!(parsed["model"].as_str(), Some("user-edit"));
                    if !eof {
                        assert_eq!(
                            parsed["profiles"]["work"]["model"].as_str(),
                            Some("user-edit")
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn inline_boundary_restore_preserves_surviving_siblings_and_unrelated_tables() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("inline-boundary-survivor");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        let survivor =
            "'survivor' = {command = 'survivor', metadata = {keep = true}} # survivor comment\n";
        for before in [false, true] {
            let path = fixture.0.join(format!("{before}.toml"));
            let head = if before { survivor } else { "" };
            let tail = if before { "" } else { survivor };
            std::fs::write(&path, format!("theme = 'keep'\n[mcp_servers]\n{head}# BEGIN NATIVE\n'native' = {{command = 'native', env = {{TOKEN = 'keep'}}}}\n# END NATIVE\n{tail}[profiles.work]\nmodel = 'before'\n[profiles.other]\nkeep = true\n")).unwrap();
            let gateway = super::super::tests::sample_gateway(None, "codex");
            let mut keep = gateway.clone();
            keep.name = "survivor".into();
            keep.command = Some("survivor".into());
            keep.env.clear();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                moved::record_selected(
                    "fixture",
                    Format::TomlMcpServers,
                    &path,
                    &["native".into()],
                )?;
                write_toml(&path, &[gateway.clone(), keep.clone()])
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
            let output = read_config_file(&path).unwrap();
            let begin = output.find("# BEGIN NATIVE").unwrap();
            let native = output.find("'native' =").unwrap();
            let end = output.find("# END NATIVE").unwrap();
            let sibling = output.find("'survivor' =").unwrap();
            assert!(begin < native && native < end, "{output}");
            assert!(
                if before {
                    sibling < begin
                } else {
                    end < sibling
                },
                "{output}"
            );
            assert!(output.contains(survivor), "{output}");
            let parsed: toml::Value = toml::from_str(&output).unwrap();
            assert_eq!(parsed["theme"].as_str(), Some("keep"));
            assert_eq!(
                parsed["mcp_servers"]["native"]["env"]["TOKEN"].as_str(),
                Some("keep")
            );
            assert_eq!(
                parsed["profiles"]["work"]["model"].as_str(),
                Some("user-edit")
            );
            assert_eq!(parsed["profiles"]["other"]["keep"].as_bool(), Some(true));
        }
    }
    #[test]
    fn security_legacy_move_restore_keeps_eof_comment_after_restored_entry() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("legacy-eof-boundary");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        let path = fixture.0.join("config.toml");
        std::fs::write(
            &path,
            "# BEGIN NATIVE\n[mcp_servers.native]\ncommand = 'native'\n# END NATIVE\n",
        )
        .unwrap();
        moved::record("fixture", Format::TomlMcpServers, &path).unwrap();
        assert!(restore::toml_prefix_provenance("fixture", &path)
            .unwrap()
            .is_none());
        std::fs::write(&path, "# END NATIVE").unwrap();
        moved::restore("fixture", Format::TomlMcpServers, &path).unwrap();
        let output = read_config_file(&path).unwrap();
        assert_eq!(output.matches("# BEGIN NATIVE").count(), 1);
        assert_eq!(output.matches("# END NATIVE").count(), 1);
        assert!(
            output.find("# BEGIN NATIVE").unwrap() < output.find("[mcp_servers.native]").unwrap(),
            "{output}"
        );
        assert!(
            output.find("[mcp_servers.native]").unwrap() < output.find("# END NATIVE").unwrap(),
            "{output}"
        );
        let parsed: toml::Value = toml::from_str(&output).unwrap();
        assert_eq!(
            parsed["mcp_servers"]["native"]["command"].as_str(),
            Some("native")
        );
    }

    #[test]
    fn security_renamed_gateway_keeps_native_transfer_evidence_and_boundary() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("renamed-gateway-boundary");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        let gateway = super::super::tests::sample_gateway(None, "codex");
        let mut owned = gateway.clone();
        owned.command = Some("conduit-gateway".into());
        owned.env.clear();
        let mut registry = crate::registry::Registry::default();
        registry.set_client_managed_entry("codex", ManagedEntry::from_gateway_entry(&owned));
        crate::registry::save(&registry).unwrap();
        for user_gateway_edit in [false, true] {
            let path = fixture.0.join(format!("{user_gateway_edit}.toml"));
            std::fs::write(&path, "# BEGIN NATIVE\n[mcp_servers.native]\ncommand = 'native'\nmetadata = {keep = true}\n# END NATIVE\n[mcp_servers.conduit]\ncommand = 'conduit-gateway'\n# second alias comment\n[mcp_servers.Conduit]\ncommand = 'conduit-gateway'\n[profiles.work]\nmodel = 'before'\nextra = 'keep'\n").unwrap();
            mutation::run("codex", &path, Format::TomlMcpServers, || {
                moved::record_selected("codex", Format::TomlMcpServers, &path, &["native".into()])?;
                moved::remove_selected(Format::TomlMcpServers, &path, &["native".into()])?;
                edit_toml_gateway(&path, Some(&gateway))
            })
            .unwrap();
            let observed: DocumentMut = read_config_file(&path).unwrap().parse().unwrap();
            let (key, item, _) = moved::toml_entry("codex", &path, "native")
                .unwrap()
                .unwrap();
            let meta = moved::toml_provenance("codex", &path, &[(key, item)]).unwrap();
            let capture = meta[0].capture.as_ref().unwrap();
            let live = meta[0]
                .live
                .iter()
                .find(|live| live.source_id == capture.id)
                .unwrap();
            let (_, gateway_item) = super::raw_entry(&observed, "toolport").unwrap();
            assert_eq!(
                live.destination,
                super::source_destination("toolport", &gateway_item)
            );
            assert_eq!(super::valid_live(&observed, live), Some("# BEGIN NATIVE\n"));
            let mut edited: DocumentMut = read_config_file(&path)
                .unwrap()
                .replace("model = 'before'", "model = 'user-edit'")
                .parse()
                .unwrap();
            if user_gateway_edit {
                let table = edited["mcp_servers"]["toolport"].as_table_mut().unwrap();
                table.insert(
                    "user_extra",
                    toml_edit::Item::Value(toml_edit::Value::from("keep")),
                );
                let prefix = table
                    .decor()
                    .prefix()
                    .and_then(|raw| raw.as_str())
                    .unwrap_or("")
                    .to_string();
                table
                    .decor_mut()
                    .set_prefix(format!("# user gateway comment\n{prefix}"));
            }
            std::fs::write(&path, edited.to_string()).unwrap();
            mutation::run("codex", &path, Format::TomlMcpServers, || {
                assert!(restore::apply("codex", Format::TomlMcpServers, &path)?);
                Ok(())
            })
            .unwrap();
            let output = read_config_file(&path).unwrap();
            assert_eq!(output.matches("# BEGIN NATIVE").count(), 1, "{output}");
            assert_eq!(output.matches("# END NATIVE").count(), 1, "{output}");
            assert_eq!(
                output.matches("# second alias comment").count(),
                1,
                "{output}"
            );
            assert_eq!(
                output.matches("# user gateway comment").count(),
                usize::from(user_gateway_edit),
                "{output}"
            );
            let native = output.find("[mcp_servers.native]").unwrap();
            assert!(
                output.find("# BEGIN NATIVE").unwrap() < native
                    && native < output.find("# END NATIVE").unwrap(),
                "{output}"
            );
            let parsed: toml::Value = toml::from_str(&output).unwrap();
            assert!(parsed["mcp_servers"].get("conduit").is_none());
            assert!(parsed["mcp_servers"].get("Conduit").is_none());
            if user_gateway_edit {
                assert_eq!(
                    parsed["mcp_servers"]["toolport"]["user_extra"].as_str(),
                    Some("keep")
                );
            } else {
                assert!(parsed["mcp_servers"].get("toolport").is_none());
            }
            assert_eq!(
                parsed["mcp_servers"]["native"]["metadata"]["keep"].as_bool(),
                Some(true)
            );
            assert_eq!(
                parsed["profiles"]["work"]["model"].as_str(),
                Some("user-edit")
            );
            assert_eq!(parsed["profiles"]["work"]["extra"].as_str(), Some("keep"));
        }
    }
    #[test]
    fn added_entries_with_proven_prefixes_restore_in_transfer_order() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("added-prefix-order");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        for inline in [false, true] {
            for direct in [false, true] {
                let path = fixture.0.join(format!("{inline}-{direct}.toml"));
                std::fs::write(&path, "model = 'before'\n[profiles.work]\nkeep = true\n").unwrap();
                let gateway = super::super::tests::sample_gateway(None, "codex");
                mutation::run("fixture", &path, Format::TomlMcpServers, || {
                    write_toml(&path, std::slice::from_ref(&gateway))
                })
                .unwrap();
                let connected: DocumentMut = read_config_file(&path).unwrap().parse().unwrap();
                let native = if inline {
                    "[mcp_servers]\n# BEGIN ADDED\n'zebra' = {command = 'zebra'}\n# between entries\n'alpha' = {command = 'alpha', metadata = {keep = true}}\n# END ADDED\n"
                } else {
                    "# BEGIN ADDED\n[mcp_servers.zebra]\ncommand = 'zebra'\n# between entries\n[mcp_servers.alpha]\ncommand = 'alpha'\nmetadata = {keep = true}\n# END ADDED\n"
                };
                let mut edited: DocumentMut =
                    format!("model = 'before'\n{native}[profiles.work]\nkeep = true\n")
                        .parse()
                        .unwrap();
                super::put(
                    &mut edited,
                    "toolport",
                    connected["mcp_servers"]["toolport"].clone(),
                );
                std::fs::write(&path, edited.to_string()).unwrap();
                mutation::run("fixture", &path, Format::TomlMcpServers, || {
                    moved::record("fixture", Format::TomlMcpServers, &path)?;
                    write_toml(&path, std::slice::from_ref(&gateway))
                })
                .unwrap();
                std::fs::write(
                    &path,
                    read_config_file(&path)
                        .unwrap()
                        .replace("model = 'before'", "model = 'user-edit'"),
                )
                .unwrap();
                mutation::run("fixture", &path, Format::TomlMcpServers, || {
                    if direct {
                        moved::restore("fixture", Format::TomlMcpServers, &path)?;
                    } else {
                        assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
                    }
                    Ok(())
                })
                .unwrap();
                let output = read_config_file(&path).unwrap();
                let begin = output.find("# BEGIN ADDED").unwrap();
                let zebra = output
                    .find(if inline {
                        "'zebra' ="
                    } else {
                        "[mcp_servers.zebra]"
                    })
                    .unwrap();
                let between = output.find("# between entries").unwrap();
                let alpha = output
                    .find(if inline {
                        "'alpha' ="
                    } else {
                        "[mcp_servers.alpha]"
                    })
                    .unwrap();
                let end = output.find("# END ADDED").unwrap();
                assert!(
                    begin < zebra && zebra < between && between < alpha && alpha < end,
                    "{output}"
                );
                assert_eq!(output.matches("# BEGIN ADDED").count(), 1);
                assert_eq!(output.matches("# END ADDED").count(), 1);
                let parsed: toml::Value = toml::from_str(&output).unwrap();
                assert_eq!(
                    parsed["mcp_servers"]["alpha"]["metadata"]["keep"].as_bool(),
                    Some(true)
                );
                assert_eq!(parsed["model"].as_str(), Some("user-edit"));
            }
        }
    }

    #[test]
    fn separately_added_prefixless_entry_is_not_guessed_into_an_earlier_boundary() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("separate-prefixless-move");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        let path = fixture.0.join("config.toml");
        let gateway = super::super::tests::sample_gateway(None, "codex");
        std::fs::write(&path, "model = 'before'\n[profiles.work]\nkeep = true\n").unwrap();
        mutation::run("fixture", &path, Format::TomlMcpServers, || {
            write_toml(&path, std::slice::from_ref(&gateway))
        })
        .unwrap();
        let connected: DocumentMut = read_config_file(&path).unwrap().parse().unwrap();
        let mut edited: DocumentMut = "model = 'before'\n# BEGIN ADDED\n[mcp_servers.native]\ncommand = 'native'\n# END ADDED\n[profiles.work]\nkeep = true\n".parse().unwrap();
        super::put(
            &mut edited,
            "toolport",
            connected["mcp_servers"]["toolport"].clone(),
        );
        std::fs::write(&path, edited.to_string()).unwrap();
        mutation::run("fixture", &path, Format::TomlMcpServers, || {
            moved::record("fixture", Format::TomlMcpServers, &path)?;
            write_toml(&path, std::slice::from_ref(&gateway))
        })
        .unwrap();
        let mut edited: DocumentMut = read_config_file(&path).unwrap().parse().unwrap();
        let mut extra = gateway.clone();
        extra.name = "extra".into();
        extra.command = Some("extra".into());
        extra.env.clear();
        super::put(
            &mut edited,
            "extra",
            toml_value_to_item(&entry_to_toml(&extra)),
        );
        std::fs::write(&path, edited.to_string()).unwrap();
        mutation::run("fixture", &path, Format::TomlMcpServers, || {
            moved::record("fixture", Format::TomlMcpServers, &path)?;
            write_toml(&path, std::slice::from_ref(&gateway))
        })
        .unwrap();
        std::fs::write(
            &path,
            read_config_file(&path)
                .unwrap()
                .replace("model = 'before'", "model = 'user-edit'"),
        )
        .unwrap();
        mutation::run("fixture", &path, Format::TomlMcpServers, || {
            assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
            Ok(())
        })
        .unwrap();
        let output = read_config_file(&path).unwrap();
        let native = output.find("[mcp_servers.native]").unwrap();
        let end = output.find("# END ADDED").unwrap();
        let extra = output.find("[mcp_servers.extra]").unwrap();
        assert!(
            output.find("# BEGIN ADDED").unwrap() < native && native < end && end < extra,
            "{output}"
        );
        let parsed: toml::Value = toml::from_str(&output).unwrap();
        assert_eq!(
            parsed["mcp_servers"]["extra"]["command"].as_str(),
            Some("extra")
        );
        assert_eq!(parsed["profiles"]["work"]["keep"].as_bool(), Some(true));
    }
    #[test]
    fn successive_partial_moves_restore_overlapping_prefix_spans_once_in_source_order() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("partial-move-prefix-overlap");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        for added in [false, true] {
            for inline in [false, true] {
                for direct in [false, true] {
                    for inter_comment in [false, true] {
                        let path = fixture
                            .0
                            .join(format!("{added}-{inline}-{direct}-{inter_comment}.toml"));
                        let between = if inter_comment {
                            "# between partial entries\n"
                        } else {
                            ""
                        };
                        let native = if inline {
                            format!("[mcp_servers]\n# BEGIN PARTIAL\n'zebra' = {{command = 'zebra', env = {{TOKEN = 'keep'}}}}\n{between}'middle' = {{command = 'middle'}}\n{between}'alpha' = {{command = 'alpha', metadata = {{keep = true}}}}\n# END PARTIAL\n")
                        } else {
                            format!("# BEGIN PARTIAL\n[mcp_servers.zebra]\ncommand = 'zebra'\n[mcp_servers.zebra.env]\nTOKEN = 'keep'\n{between}[mcp_servers.middle]\ncommand = 'middle'\n{between}[mcp_servers.alpha]\ncommand = 'alpha'\nmetadata = {{keep = true}}\n# END PARTIAL\n")
                        };
                        let full =
                            format!("model = 'before'\n{native}[profiles.work]\nkeep = true\n");
                        let gateway = super::super::tests::sample_gateway(None, "codex");
                        if added {
                            std::fs::write(
                                &path,
                                "model = 'before'\n[profiles.work]\nkeep = true\n",
                            )
                            .unwrap();
                            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                                write_toml(&path, std::slice::from_ref(&gateway))
                            })
                            .unwrap();
                            let connected: DocumentMut =
                                read_config_file(&path).unwrap().parse().unwrap();
                            let mut edited: DocumentMut = full.parse().unwrap();
                            super::put(
                                &mut edited,
                                "toolport",
                                connected["mcp_servers"]["toolport"].clone(),
                            );
                            std::fs::write(&path, edited.to_string()).unwrap();
                        } else {
                            std::fs::write(&path, full).unwrap();
                        }
                        for name in ["zebra", "middle", "alpha"] {
                            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                                moved::record_selected(
                                    "fixture",
                                    Format::TomlMcpServers,
                                    &path,
                                    &[name.into()],
                                )?;
                                moved::remove_selected(
                                    Format::TomlMcpServers,
                                    &path,
                                    &[name.into()],
                                )?;
                                edit_toml_gateway(&path, Some(&gateway))
                            })
                            .unwrap();
                        }
                        std::fs::write(
                            &path,
                            read_config_file(&path)
                                .unwrap()
                                .replace("model = 'before'", "model = 'user-edit'"),
                        )
                        .unwrap();
                        mutation::run("fixture", &path, Format::TomlMcpServers, || {
                            if direct {
                                moved::restore("fixture", Format::TomlMcpServers, &path)?;
                            } else {
                                assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
                            }
                            Ok(())
                        })
                        .unwrap();
                        let output = read_config_file(&path).unwrap();
                        assert_eq!(
                            output.matches("# BEGIN PARTIAL").count(),
                            1,
                            "{added}-{inline}-{direct}-{inter_comment}: {output}"
                        );
                        assert_eq!(output.matches("# END PARTIAL").count(), 1, "{output}");
                        assert_eq!(
                            output.matches("# between partial entries").count(),
                            2 * usize::from(inter_comment),
                            "{output}"
                        );
                        let zebra = output
                            .find(if inline {
                                "'zebra' ="
                            } else {
                                "[mcp_servers.zebra]"
                            })
                            .unwrap();
                        let middle = output
                            .find(if inline {
                                "'middle' ="
                            } else {
                                "[mcp_servers.middle]"
                            })
                            .unwrap();
                        let alpha = output
                            .find(if inline {
                                "'alpha' ="
                            } else {
                                "[mcp_servers.alpha]"
                            })
                            .unwrap();
                        assert!(
                            output.find("# BEGIN PARTIAL").unwrap() < zebra
                                && zebra < middle
                                && middle < alpha
                                && alpha < output.find("# END PARTIAL").unwrap(),
                            "{output}"
                        );
                        if inter_comment {
                            assert!(
                                zebra < output.find(between.trim_end()).unwrap()
                                    && output.find(between.trim_end()).unwrap() < middle,
                                "{output}"
                            );
                        }
                        let parsed: toml::Value = toml::from_str(&output).unwrap();
                        assert_eq!(parsed["model"].as_str(), Some("user-edit"));
                        assert_eq!(
                            parsed["mcp_servers"]["zebra"]["env"]["TOKEN"].as_str(),
                            Some("keep")
                        );
                        assert_eq!(
                            parsed["mcp_servers"]["alpha"]["metadata"]["keep"].as_bool(),
                            Some(true)
                        );
                        assert_eq!(parsed["profiles"]["work"]["keep"].as_bool(), Some(true));
                    }
                }
            }
        }
    }
    #[test]
    fn original_eof_and_added_entry_share_one_actual_restore_plan() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("combined-eof-added-prefix");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        for (direct, external_rebind) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let path = fixture.0.join(format!("{direct}-{external_rebind}.toml"));
            std::fs::write(&path, "model = 'before'\n# BEGIN ORIGINAL\n[mcp_servers.original]\ncommand = 'original'\n# END ORIGINAL\n").unwrap();
            let gateway = super::super::tests::sample_gateway(None, "codex");
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                moved::record_selected(
                    "fixture",
                    Format::TomlMcpServers,
                    &path,
                    &["original".into()],
                )?;
                moved::remove_selected(Format::TomlMcpServers, &path, &["original".into()])?;
                edit_toml_gateway(&path, Some(&gateway))
            })
            .unwrap();
            let current = read_config_file(&path).unwrap();
            let added = if external_rebind {
                "# BEGIN ADDED\n[mcp_servers.added]\ncommand = 'added'\nmetadata = {keep = true}\n# END ADDED\n"
            } else {
                "# BEGIN ADDED\n[mcp_servers.added]\ncommand = 'added' # END ADDED\nmetadata = {keep = true}\n"
            };
            let edited = if external_rebind {
                current.replace(
                    "[mcp_servers.toolport]",
                    &format!("{added}[mcp_servers.toolport]"),
                )
            } else {
                current.replace("# BEGIN ORIGINAL\n", &format!("{added}# BEGIN ORIGINAL\n"))
            };
            if !external_rebind {
                let before: DocumentMut = current.parse().unwrap();
                let after: DocumentMut = edited.parse().unwrap();
                let (_, gateway_item) = super::raw_entry(&before, "toolport").unwrap();
                let destination = super::source_destination("toolport", &gateway_item);
                assert_eq!(
                    super::destination_prefix(&before, &destination),
                    super::destination_prefix(&after, &destination)
                );
                let (key, item, _) = moved::toml_entry("fixture", &path, "original")
                    .unwrap()
                    .unwrap();
                let provenance = moved::toml_provenance("fixture", &path, &[(key, item)]).unwrap();
                let capture = provenance[0].capture.as_ref().unwrap();
                let live = provenance[0]
                    .live
                    .iter()
                    .find(|live| live.source_id == capture.id)
                    .unwrap();
                assert_eq!(
                    super::valid_live(&before, live),
                    super::valid_live(&after, live)
                );
                assert!(super::valid_live(&after, live).is_some());
            }
            std::fs::write(&path, edited).unwrap();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                moved::record_selected(
                    "fixture",
                    Format::TomlMcpServers,
                    &path,
                    &["added".into()],
                )?;
                moved::remove_selected(Format::TomlMcpServers, &path, &["added".into()])?;
                edit_toml_gateway(&path, Some(&gateway))
            })
            .unwrap();
            std::fs::write(
                &path,
                read_config_file(&path)
                    .unwrap()
                    .replace("model = 'before'", "model = 'user-edit'"),
            )
            .unwrap();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                if direct {
                    moved::restore("fixture", Format::TomlMcpServers, &path)?;
                } else {
                    assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
                }
                Ok(())
            })
            .unwrap();
            let output = read_config_file(&path).unwrap();
            if external_rebind {
                let doc: DocumentMut = output.parse().unwrap();
                let (key, item) = super::raw_entry(&doc, "original").unwrap();
                assert_eq!(
                    super::entry_prefix(&key, &item),
                    "# BEGIN ORIGINAL\n",
                    "{output}"
                );
                let (key, item) = super::raw_entry(&doc, "added").unwrap();
                let prefix = super::entry_prefix(&key, &item);
                assert!(
                    prefix.ends_with("# BEGIN ORIGINAL\n# END ORIGINAL\n# BEGIN ADDED\n"),
                    "{output}"
                );
                let current_prefix = if direct {
                    let (key, item) = super::raw_entry(&doc, "toolport").unwrap();
                    super::entry_prefix(&key, &item).to_string()
                } else {
                    prefix.to_string()
                };
                assert!(
                    current_prefix.contains("# END ORIGINAL\n# BEGIN ADDED\n# END ADDED\n"),
                    "{output}"
                );
                for (marker, expected) in [
                    ("# BEGIN ORIGINAL", 2),
                    ("# END ORIGINAL", 2),
                    ("# BEGIN ADDED", 2),
                    ("# END ADDED", 1),
                ] {
                    assert_eq!(output.matches(marker).count(), expected, "{output}");
                }
                let parsed: toml::Value = toml::from_str(&output).unwrap();
                assert_eq!(parsed["model"].as_str(), Some("user-edit"));
                assert_eq!(
                    parsed["mcp_servers"]["original"]["command"].as_str(),
                    Some("original")
                );
                assert_eq!(
                    parsed["mcp_servers"]["added"]["metadata"]["keep"].as_bool(),
                    Some(true)
                );
                continue;
            }
            for marker in [
                "# BEGIN ORIGINAL",
                "# END ORIGINAL",
                "# BEGIN ADDED",
                "# END ADDED",
            ] {
                assert_eq!(output.matches(marker).count(), 1, "{output}");
            }
            let positions: Vec<_> = [
                "# BEGIN ADDED",
                "[mcp_servers.added]",
                "# END ADDED",
                "# BEGIN ORIGINAL",
                "[mcp_servers.original]",
                "# END ORIGINAL",
            ]
            .iter()
            .map(|needle| output.find(needle).unwrap())
            .collect();
            assert!(
                positions.windows(2).all(|pair| pair[0] < pair[1]),
                "{output}"
            );
            let parsed: toml::Value = toml::from_str(&output).unwrap();
            assert_eq!(parsed["model"].as_str(), Some("user-edit"));
            assert_eq!(
                parsed["mcp_servers"]["added"]["metadata"]["keep"].as_bool(),
                Some(true)
            );
        }
    }

    #[test]
    fn independent_equal_comment_text_at_different_destinations_is_preserved() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("independent-prefix-destinations");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        let path = fixture.0.join("config.toml");
        std::fs::write(&path, "model = 'before'\n# BEGIN A\n# shared note\n[mcp_servers.a]\ncommand = 'a'\n# END A\n[profiles.a]\nkeep = true\n# BEGIN B\n# shared note\n[mcp_servers.b]\ncommand = 'b'\n# END B\n[profiles.b]\nkeep = true\n").unwrap();
        let gateway = super::super::tests::sample_gateway(None, "codex");
        for name in ["a", "b"] {
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                moved::record_selected("fixture", Format::TomlMcpServers, &path, &[name.into()])?;
                moved::remove_selected(Format::TomlMcpServers, &path, &[name.into()])?;
                edit_toml_gateway(&path, Some(&gateway))
            })
            .unwrap();
        }
        std::fs::write(
            &path,
            read_config_file(&path)
                .unwrap()
                .replace("model = 'before'", "model = 'user-edit'"),
        )
        .unwrap();
        mutation::run("fixture", &path, Format::TomlMcpServers, || {
            assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
            Ok(())
        })
        .unwrap();
        let output = read_config_file(&path).unwrap();
        assert_eq!(output.matches("# shared note").count(), 2, "{output}");
        for (begin, header, end) in [
            ("# BEGIN A", "[mcp_servers.a]", "# END A"),
            ("# BEGIN B", "[mcp_servers.b]", "# END B"),
        ] {
            assert!(
                output.find(begin).unwrap() < output.find(header).unwrap()
                    && output.find(header).unwrap() < output.find(end).unwrap(),
                "{output}"
            );
        }
        let parsed: toml::Value = toml::from_str(&output).unwrap();
        assert_eq!(parsed["profiles"]["a"]["keep"].as_bool(), Some(true));
        assert_eq!(parsed["profiles"]["b"]["keep"].as_bool(), Some(true));
    }

    #[test]
    fn crossing_or_lost_prefix_evidence_preserves_full_raw_and_current_decor() {
        let original: DocumentMut = "[profiles.work]\nkeep = true\n".parse().unwrap();
        let written: DocumentMut = "# a\n# b\n# c\n[profiles.work]\nkeep = true\n"
            .parse()
            .unwrap();
        for lost in [false, true] {
            let mut current = if lost {
                "# user edited\n[profiles.work]\nkeep = true\n"
                    .parse()
                    .unwrap()
            } else {
                written.clone()
            };
            let entries: Vec<_> = [("one", "# a\n# b\n"), ("two", "# b\n# c\n")]
                .iter()
                .map(|(name, prefix)| {
                    let raw: DocumentMut =
                        format!("{prefix}[mcp_servers.{name}]\ncommand = '{name}'\n")
                            .parse()
                            .unwrap();
                    super::raw_entry(&raw, name).unwrap()
                })
                .collect();
            let prior = current["profiles"]["work"]
                .as_table()
                .unwrap()
                .decor()
                .prefix()
                .unwrap()
                .as_str()
                .unwrap()
                .to_string();
            super::restore_raw_entries(&original, &written, &mut current, entries);
            assert_eq!(
                current["profiles"]["work"]
                    .as_table()
                    .unwrap()
                    .decor()
                    .prefix()
                    .unwrap()
                    .as_str()
                    .unwrap(),
                prior
            );
            for (name, prefix) in [("one", "# a\n# b\n"), ("two", "# b\n# c\n")] {
                let (key, item) = super::raw_entry(&current, name).unwrap();
                assert_eq!(super::entry_prefix(&key, &item), prefix);
            }
            let _: toml::Value = toml::from_str(&current.to_string()).unwrap();
        }
    }
    #[test]
    fn lost_original_prefix_does_not_borrow_an_independent_added_comment_span() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("independent-lost-prefix-owner");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        for direct in [false, true] {
            let path = fixture.0.join(format!("{direct}.toml"));
            std::fs::write(&path, "model = 'before'\n# X\n[mcp_servers.a]\ncommand = 'a'\nmetadata = {keep = true}\n[profiles.work]\nkeep = true\n").unwrap();
            let gateway = super::super::tests::sample_gateway(None, "codex");
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                moved::record_selected("fixture", Format::TomlMcpServers, &path, &["a".into()])?;
                moved::remove_selected(Format::TomlMcpServers, &path, &["a".into()])?;
                edit_toml_gateway(&path, Some(&gateway))
            })
            .unwrap();
            let edited = read_config_file(&path).unwrap().replace("# X\n", "").replace("[profiles.work]", "# X\n# Y\n[mcp_servers.b]\ncommand = 'b'\nmetadata = {keep = true}\n[profiles.work]");
            std::fs::write(&path, edited).unwrap();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                moved::record_selected("fixture", Format::TomlMcpServers, &path, &["b".into()])?;
                moved::remove_selected(Format::TomlMcpServers, &path, &["b".into()])?;
                edit_toml_gateway(&path, Some(&gateway))
            })
            .unwrap();
            std::fs::write(
                &path,
                read_config_file(&path)
                    .unwrap()
                    .replace("model = 'before'", "model = 'user-edit'"),
            )
            .unwrap();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                if direct {
                    moved::restore("fixture", Format::TomlMcpServers, &path)?;
                } else {
                    assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
                }
                Ok(())
            })
            .unwrap();
            let output = read_config_file(&path).unwrap();
            assert_eq!(output.matches("# X").count(), 2, "{output}");
            assert_eq!(output.matches("# Y").count(), 1, "{output}");
            for (name, prefix) in [("a", "# X\n"), ("b", "# X\n# Y\n")] {
                let doc: DocumentMut = output.parse().unwrap();
                let (key, item) = super::raw_entry(&doc, name).unwrap();
                assert_eq!(super::entry_prefix(&key, &item), prefix, "{output}");
            }
            let parsed: toml::Value = toml::from_str(&output).unwrap();
            assert_eq!(
                parsed["mcp_servers"]["a"]["metadata"]["keep"].as_bool(),
                Some(true)
            );
            assert_eq!(
                parsed["mcp_servers"]["b"]["metadata"]["keep"].as_bool(),
                Some(true)
            );
            assert_eq!(parsed["model"].as_str(), Some("user-edit"));
            assert_eq!(parsed["profiles"]["work"]["keep"].as_bool(), Some(true));
        }
    }
    #[test]
    fn changed_original_and_added_equal_prefixes_remain_independent() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("changed-original-independent-prefix");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        for direct in [false, true] {
            let path = fixture.0.join(format!("{direct}.toml"));
            std::fs::write(&path, "model = 'before'\n# Y\n[mcp_servers.b]\ncommand = 'b'\n# X\n[profiles.work]\nkeep = true\n").unwrap();
            let gateway = super::super::tests::sample_gateway(None, "codex");
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                edit_toml_gateway(&path, Some(&gateway))
            })
            .unwrap();
            let edited = read_config_file(&path)
                .unwrap()
                .replace("# X\n[profiles.work]", "[profiles.work]")
                .replace("# Y\n[mcp_servers.b]", "# X\n[mcp_servers.b]");
            std::fs::write(&path, edited).unwrap();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                moved::record_selected("fixture", Format::TomlMcpServers, &path, &["b".into()])?;
                moved::remove_selected(Format::TomlMcpServers, &path, &["b".into()])?;
                edit_toml_gateway(&path, Some(&gateway))
            })
            .unwrap();
            let edited = read_config_file(&path)
                .unwrap()
                .replace("# X\n", "")
                .replace(
                    "[profiles.work]",
                    "# X\n# Z\n[mcp_servers.c]\ncommand = 'c'\n[profiles.work]",
                );
            std::fs::write(&path, edited).unwrap();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                moved::record_selected("fixture", Format::TomlMcpServers, &path, &["c".into()])?;
                moved::remove_selected(Format::TomlMcpServers, &path, &["c".into()])?;
                edit_toml_gateway(&path, Some(&gateway))
            })
            .unwrap();
            std::fs::write(
                &path,
                read_config_file(&path)
                    .unwrap()
                    .replace("model = 'before'", "model = 'user-edit'"),
            )
            .unwrap();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                if direct {
                    moved::restore("fixture", Format::TomlMcpServers, &path)?;
                } else {
                    assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
                }
                Ok(())
            })
            .unwrap();
            let output = read_config_file(&path).unwrap();
            assert_eq!(output.matches("# X").count(), 2, "{output}");
            let doc: DocumentMut = output.parse().unwrap();
            for (name, expected) in [("b", "# X\n"), ("c", "# X\n# Z\n")] {
                let (key, item) = super::raw_entry(&doc, name).unwrap();
                assert_eq!(super::entry_prefix(&key, &item), expected, "{output}");
            }
        }
    }
    #[test]
    fn prefixless_original_entries_return_before_block_end() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("prefixless-original-anchors");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        for inline in [false, true] {
            for direct in [false, true] {
                for eof in [false, true] {
                    for mode in 0..3 {
                        let path = fixture
                            .0
                            .join(format!("{inline}-{direct}-{eof}-{mode}.toml"));
                        let native = if inline {
                            "[mcp_servers]\n# BEGIN\nfirst = {command = 'first'}\nsecond = {command = 'second', metadata = {keep = true}}\nthird = {command = 'third'}\n# END\n"
                        } else {
                            "# BEGIN\n[mcp_servers.first]\ncommand = 'first'\n[mcp_servers.second]\ncommand = 'second'\nmetadata = {keep = true}\n[mcp_servers.third]\ncommand = 'third'\n# END\n"
                        };
                        let profile = if eof {
                            ""
                        } else {
                            "[profiles.work]\nkeep = true\nname = 'before'\n"
                        };
                        std::fs::write(&path, format!("model = 'before'\n{native}{profile}"))
                            .unwrap();
                        let names = match mode {
                            0 => vec!["second".into()],
                            1 => vec!["second".into(), "third".into()],
                            _ => vec!["first".into(), "second".into()],
                        };
                        let gateway = super::super::tests::sample_gateway(None, "codex");
                        mutation::run("fixture", &path, Format::TomlMcpServers, || {
                            moved::record_selected(
                                "fixture",
                                Format::TomlMcpServers,
                                &path,
                                &names,
                            )?;
                            moved::remove_selected(Format::TomlMcpServers, &path, &names)?;
                            edit_toml_gateway(&path, Some(&gateway))
                        })
                        .unwrap();
                        let edited = read_config_file(&path)
                            .unwrap()
                            .replace("model = 'before'", "model = 'user-edit'")
                            .replace("name = 'before'", "name = 'user-edit'");
                        std::fs::write(&path, edited).unwrap();
                        mutation::run("fixture", &path, Format::TomlMcpServers, || {
                            if direct {
                                moved::restore("fixture", Format::TomlMcpServers, &path)?;
                            } else {
                                assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
                            }
                            Ok(())
                        })
                        .unwrap();
                        let output = read_config_file(&path).unwrap();
                        let marker = |name| {
                            if inline {
                                format!("{name} =")
                            } else {
                                format!("[mcp_servers.{name}]")
                            }
                        };
                        let positions: Vec<_> = [
                            "# BEGIN".to_string(),
                            marker("first"),
                            marker("second"),
                            marker("third"),
                            "# END".to_string(),
                        ]
                        .iter()
                        .map(|needle| output.find(needle).unwrap())
                        .collect();
                        assert!(
                            positions.windows(2).all(|pair| pair[0] < pair[1]),
                            "{inline}-{direct}-{eof}-{mode}: {output}"
                        );
                        assert_eq!(output.matches("# BEGIN").count(), 1, "{output}");
                        assert_eq!(output.matches("# END").count(), 1, "{output}");
                        let parsed: toml::Value = toml::from_str(&output).unwrap();
                        assert_eq!(parsed["model"].as_str(), Some("user-edit"));
                        assert_eq!(
                            parsed["mcp_servers"]["second"]["metadata"]["keep"].as_bool(),
                            Some(true)
                        );
                        if !eof {
                            assert_eq!(parsed["profiles"]["work"]["keep"].as_bool(), Some(true));
                            assert_eq!(
                                parsed["profiles"]["work"]["name"].as_str(),
                                Some("user-edit")
                            );
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn prefixless_original_anchor_preserves_latest_removed_generation() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("prefixless-newest-generation");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        for inline in [false, true] {
            for direct in [false, true] {
                let path = fixture.0.join(format!("{inline}-{direct}.toml"));
                let original = if inline {
                    "model = 'before'\n[mcp_servers]\n# BEGIN\nfirst = {command = 'first'}\nsecond = {command = 'old'}\nthird = {command = 'third'}\n# END\n[profiles.work]\nkeep = true\n"
                } else {
                    "model = 'before'\n# BEGIN\n[mcp_servers.first]\ncommand = 'first'\n[mcp_servers.second]\ncommand = 'old'\n[mcp_servers.third]\ncommand = 'third'\n# END\n[profiles.work]\nkeep = true\n"
                };
                std::fs::write(&path, original).unwrap();
                let gateway = super::super::tests::sample_gateway(None, "codex");
                mutation::run("fixture", &path, Format::TomlMcpServers, || {
                    moved::record_selected(
                        "fixture",
                        Format::TomlMcpServers,
                        &path,
                        &["second".into()],
                    )?;
                    moved::remove_selected(Format::TomlMcpServers, &path, &["second".into()])?;
                    edit_toml_gateway(&path, Some(&gateway))
                })
                .unwrap();
                let current = read_config_file(&path).unwrap();
                let edited = if inline {
                    current.replace(
                        "third =",
                        "second = {command = 'latest', metadata = {new = true}}\nthird =",
                    )
                } else {
                    current.replace("[mcp_servers.third]", "[mcp_servers.second]\ncommand = 'latest'\nmetadata = {new = true}\n[mcp_servers.third]")
                };
                std::fs::write(&path, edited).unwrap();
                mutation::run("fixture", &path, Format::TomlMcpServers, || {
                    moved::record_selected(
                        "fixture",
                        Format::TomlMcpServers,
                        &path,
                        &["second".into()],
                    )?;
                    moved::remove_selected(Format::TomlMcpServers, &path, &["second".into()])
                })
                .unwrap();
                std::fs::write(
                    &path,
                    read_config_file(&path)
                        .unwrap()
                        .replace("model = 'before'", "model = 'user-edit'"),
                )
                .unwrap();
                mutation::run("fixture", &path, Format::TomlMcpServers, || {
                    if direct {
                        moved::restore("fixture", Format::TomlMcpServers, &path)?;
                    } else {
                        assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
                    }
                    Ok(())
                })
                .unwrap();
                let output = read_config_file(&path).unwrap();
                let label = |name| {
                    if inline {
                        format!("{name} =")
                    } else {
                        format!("[mcp_servers.{name}]")
                    }
                };
                let positions: Vec<_> = [
                    "# BEGIN".to_string(),
                    label("first"),
                    label("second"),
                    label("third"),
                    "# END".to_string(),
                ]
                .iter()
                .map(|needle| output.find(needle).unwrap())
                .collect();
                assert!(
                    positions.windows(2).all(|pair| pair[0] < pair[1]),
                    "{output}"
                );
                let parsed: toml::Value = toml::from_str(&output).unwrap();
                assert_eq!(
                    parsed["mcp_servers"]["second"]["command"].as_str(),
                    Some("latest")
                );
                assert_eq!(
                    parsed["mcp_servers"]["second"]["metadata"]["new"].as_bool(),
                    Some(true)
                );
                assert_eq!(parsed["model"].as_str(), Some("user-edit"));
                assert_eq!(parsed["profiles"]["work"]["keep"].as_bool(), Some(true));
            }
        }
    }
    #[test]
    fn stale_capture_is_not_reissued_by_uncaptured_generic_removal() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("stale-generation-generic-removal");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        for direct in [false, true] {
            let path = fixture.0.join(format!("{direct}.toml"));
            std::fs::write(&path, "model = 'before'\n# X\n[mcp_servers.a]\ncommand = 'old-a'\nmetadata = {original = true}\n# Y\n[mcp_servers.b]\ncommand = 'b'\nmetadata = {keep = true}\n[profiles.work]\nkeep = true\n").unwrap();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                moved::record_selected("fixture", Format::TomlMcpServers, &path, &["a".into()])?;
                moved::remove_selected(Format::TomlMcpServers, &path, &["a".into()])
            })
            .unwrap();
            let (key, item, _) = moved::toml_entry("fixture", &path, "a").unwrap().unwrap();
            let old = moved::toml_provenance("fixture", &path, &[(key, item)]).unwrap();
            let old_id = old[0].capture.as_ref().unwrap().id.clone();
            let edited = read_config_file(&path).unwrap().replace("# X\n", "").replace("# Y\n[mcp_servers.b]", "# X\n[mcp_servers.a]\ncommand = 'new-a'\nmetadata = {new = true}\n# Y\n[mcp_servers.b]");
            std::fs::write(&path, edited).unwrap();
            let mut b = super::super::tests::sample_gateway(None, "codex");
            b.name = "b".into();
            b.command = Some("b".into());
            b.args.clear();
            b.env.clear();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                write_toml(&path, &[b.clone()])
            })
            .unwrap();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                moved::record_selected("fixture", Format::TomlMcpServers, &path, &["b".into()])?;
                moved::remove_selected(Format::TomlMcpServers, &path, &["b".into()])
            })
            .unwrap();
            let (key, item, _) = moved::toml_entry("fixture", &path, "b").unwrap().unwrap();
            assert_eq!(super::entry_prefix(&key, &item), "# X\n# Y\n");
            let b_meta = moved::toml_provenance("fixture", &path, &[(key, item)]).unwrap();
            assert!(b_meta[0]
                .capture
                .as_ref()
                .unwrap()
                .inherited
                .iter()
                .all(|edge| edge.source_id != old_id));
            std::fs::write(
                &path,
                read_config_file(&path)
                    .unwrap()
                    .replace("model = 'before'", "model = 'user-edit'"),
            )
            .unwrap();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                if direct {
                    moved::restore("fixture", Format::TomlMcpServers, &path)?;
                } else {
                    assert!(restore::apply("fixture", Format::TomlMcpServers, &path)?);
                }
                Ok(())
            })
            .unwrap();
            let output = read_config_file(&path).unwrap();
            assert_eq!(output.matches("# X").count(), 2, "{output}");
            let parsed: toml::Value = toml::from_str(&output).unwrap();
            assert_eq!(parsed["model"].as_str(), Some("user-edit"));
            assert_eq!(
                parsed["mcp_servers"]["a"]["command"].as_str(),
                Some("old-a")
            );
            assert_eq!(
                parsed["mcp_servers"]["a"]["metadata"]["original"].as_bool(),
                Some(true)
            );
            assert_eq!(
                parsed["mcp_servers"]["b"]["metadata"]["keep"].as_bool(),
                Some(true)
            );
            assert_eq!(parsed["profiles"]["work"]["keep"].as_bool(), Some(true));
        }
    }
    #[test]
    fn capture_cannot_be_reused_after_removal_in_same_mutation() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("capture-consumed-once");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        let path = fixture.0.join("config.toml");
        std::fs::write(
            &path,
            "# X\n[mcp_servers.a]\ncommand = 'a'\n# Y\n[mcp_servers.b]\ncommand = 'b'\n",
        )
        .unwrap();
        mutation::run("fixture", &path, Format::TomlMcpServers, || {
            moved::record_selected("fixture", Format::TomlMcpServers, &path, &["a".into()])?;
            moved::remove_selected(Format::TomlMcpServers, &path, &["a".into()])?;
            let replacement = read_config_file(&path)?.replace("# X\n", "").replace(
                "# Y\n[mcp_servers.b]",
                "# X\n[mcp_servers.a]\ncommand = 'a'\n# Y\n[mcp_servers.b]",
            );
            atomic_write(&path, &replacement)?;
            moved::remove_selected(Format::TomlMcpServers, &path, &["a".into()])?;
            moved::record_selected("fixture", Format::TomlMcpServers, &path, &["b".into()])?;
            Ok(())
        })
        .unwrap();
        let (key, item, _) = moved::toml_entry("fixture", &path, "b").unwrap().unwrap();
        assert_eq!(super::entry_prefix(&key, &item), "# X\n# Y\n");
        let provenance = moved::toml_provenance("fixture", &path, &[(key, item)]).unwrap();
        assert!(provenance[0].capture.as_ref().unwrap().inherited.is_empty());
    }
    #[test]
    fn captured_source_changed_within_mutation_does_not_issue_witness() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("changed-source-after-capture");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        let path = fixture.0.join("config.toml");
        std::fs::write(
            &path,
            "# X\n[mcp_servers.a]\ncommand = 'old'\n# Y\n[mcp_servers.b]\ncommand = 'b'\n",
        )
        .unwrap();
        mutation::run("fixture", &path, Format::TomlMcpServers, || {
            moved::record_selected("fixture", Format::TomlMcpServers, &path, &["a".into()])?;
            let updated = read_config_file(&path)?.replace("command = 'old'", "command = 'new'");
            atomic_write(&path, &updated)?;
            moved::remove_selected(Format::TomlMcpServers, &path, &["a".into()])
        })
        .unwrap();
        mutation::run("fixture", &path, Format::TomlMcpServers, || {
            moved::record_selected("fixture", Format::TomlMcpServers, &path, &["b".into()])?;
            moved::remove_selected(Format::TomlMcpServers, &path, &["b".into()])
        })
        .unwrap();
        let (key, item, _) = moved::toml_entry("fixture", &path, "b").unwrap().unwrap();
        assert_eq!(super::entry_prefix(&key, &item), "# X\n# Y\n");
        let provenance = moved::toml_provenance("fixture", &path, &[(key, item)]).unwrap();
        assert!(provenance[0].capture.as_ref().unwrap().inherited.is_empty());
    }
    #[test]
    fn generic_writer_provenance_checks_source_generation_and_invalid_records() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("provenance-source-generation");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        for mode in [
            "valid",
            "legacy",
            "invalid",
            "missing",
            "current",
            "removed-again",
            "bad-range",
        ] {
            let path = fixture.0.join(format!("{mode}.toml"));
            std::fs::write(&path, "model = 'before'\n# OWN A\n[mcp_servers.a]\ncommand = 'a'\n# OWN B\n[mcp_servers.b]\ncommand = 'b'\nmetadata = {keep = true}\n[profiles.work]\nkeep = true\n").unwrap();
            let mut b = super::super::tests::sample_gateway(None, "codex");
            b.name = "b".into();
            b.command = Some("b".into());
            b.args.clear();
            b.env.clear();
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                moved::record_selected("fixture", Format::TomlMcpServers, &path, &["a".into()])?;
                write_toml(&path, &[b.clone()])
            })
            .unwrap();
            let record_path = backup_dir("fixture").unwrap().join("moved-servers.json");
            let mut record: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&record_path).unwrap()).unwrap();
            match mode {
                "legacy" => {
                    record.as_object_mut().unwrap().remove("livePrefixes");
                    record["entries"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("capture");
                }
                "invalid" => record["entries"][0]["capture"] = serde_json::json!({"id": 1}),
                "missing" => record["entries"].as_array_mut().unwrap().clear(),
                "bad-range" => {
                    record["livePrefixes"][0]["range"]["end"] = serde_json::json!(999999)
                }
                _ => {}
            }
            if matches!(mode, "legacy" | "invalid" | "missing" | "bad-range") {
                std::fs::write(&record_path, serde_json::to_string_pretty(&record).unwrap())
                    .unwrap();
            }
            if matches!(mode, "current" | "removed-again") {
                let edited = format!(
                    "{}\n# NEW A\n[mcp_servers.a]\ncommand = 'new-a'\n",
                    read_config_file(&path).unwrap()
                );
                std::fs::write(&path, edited).unwrap();
            }
            if mode == "removed-again" {
                mutation::run("fixture", &path, Format::TomlMcpServers, || {
                    moved::record_selected(
                        "fixture",
                        Format::TomlMcpServers,
                        &path,
                        &["a".into()],
                    )?;
                    moved::remove_selected(Format::TomlMcpServers, &path, &["a".into()])
                })
                .unwrap();
            }
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                moved::record_selected("fixture", Format::TomlMcpServers, &path, &["b".into()])?;
                moved::remove_selected(Format::TomlMcpServers, &path, &["b".into()])
            })
            .unwrap();
            let (key, item, _) = moved::toml_entry("fixture", &path, "b").unwrap().unwrap();
            assert_eq!(
                super::entry_prefix(&key, &item),
                "# OWN A\n# OWN B\n",
                "{mode}"
            );
            let meta = moved::toml_provenance("fixture", &path, &[(key, item)]).unwrap();
            let inherited = &meta[0].capture.as_ref().unwrap().inherited;
            assert_eq!(
                !inherited.is_empty(),
                mode == "valid",
                "{mode}: {inherited:?}"
            );
            mutation::run("fixture", &path, Format::TomlMcpServers, || {
                moved::restore("fixture", Format::TomlMcpServers, &path)?;
                Ok(())
            })
            .unwrap();
            let output = read_config_file(&path).unwrap();
            if mode == "valid" {
                assert_eq!(output.matches("# OWN A").count(), 1, "{output}");
                assert_eq!(output.matches("# OWN B").count(), 1, "{output}");
            } else {
                assert!(output.contains("# OWN A\n# OWN B\n"), "{mode}: {output}");
            }
            let parsed: toml::Value = toml::from_str(&output).unwrap();
            assert_eq!(
                parsed["mcp_servers"]["b"]["metadata"]["keep"].as_bool(),
                Some(true)
            );
        }
    }
    #[test]
    fn provenance_auxiliary_record_rolls_back_on_config_commit_conflict() {
        let _lock = crate::registry::data_dir_test_lock();
        let fixture = Fixture::new("provenance-rollback");
        let _data = crate::registry::DataDirOverride::set(fixture.0.join("data"));
        let path = fixture.0.join("config.toml");
        let initial = "# A\n[mcp_servers.a]\ncommand = 'a'\n# B\n[mcp_servers.b]\ncommand = 'b'\n";
        std::fs::write(&path, initial).unwrap();
        mutation::run("fixture", &path, Format::TomlMcpServers, || {
            moved::record_selected("fixture", Format::TomlMcpServers, &path, &["a".into()])?;
            moved::remove_selected(Format::TomlMcpServers, &path, &["a".into()])
        })
        .unwrap();
        let record_path = backup_dir("fixture").unwrap().join("moved-servers.json");
        let before_record = std::fs::read(&record_path).unwrap();
        let native = read_config_file(&path)
            .unwrap()
            .replace("command = 'b'", "command = 'user-b'");
        let native_write = native.clone();
        crate::registry::client_file::HOOK.with(|slot| {
            *slot.borrow_mut() = Some((
                "commit",
                Box::new(move |path| std::fs::write(path, &native_write).unwrap()),
            ));
        });
        let error = mutation::run("fixture", &path, Format::TomlMcpServers, || {
            moved::record_selected("fixture", Format::TomlMcpServers, &path, &["b".into()])?;
            moved::remove_selected(Format::TomlMcpServers, &path, &["b".into()])
        })
        .unwrap_err();
        assert!(
            error.contains("changed") || error.contains("conflict"),
            "{error}"
        );
        assert_eq!(std::fs::read(&record_path).unwrap(), before_record);
        assert_eq!(read_config_file(&path).unwrap(), native);
    }
    #[test]
    fn inherited_prefix_cannot_extend_beyond_the_independent_owner_union() {
        let original: DocumentMut = "# X\n[mcp_servers.a]\ncommand = 'a'\n# Y\n[mcp_servers.b]\ncommand = 'old-b'\n[profiles.work]\nkeep = true\n# X\n# Y\n[profiles.unrelated]\nkeep = true\n".parse().unwrap();
        let written: DocumentMut = "# X\n# Y\n[profiles.work]\nkeep = true\n".parse().unwrap();
        let raw_b: DocumentMut =
            "# X\n# Y\n[mcp_servers.b]\ncommand = 'latest-b'\nmetadata = {latest = true}\n"
                .parse()
                .unwrap();
        let entries = vec![
            super::raw_entry(&original, "a").unwrap(),
            super::raw_entry(&raw_b, "b").unwrap(),
        ];
        let mut current = written.clone();
        super::restore_raw_entries(&original, &written, &mut current, entries);
        assert_eq!(
            current["profiles"]["work"]
                .as_table()
                .unwrap()
                .decor()
                .prefix()
                .unwrap()
                .as_str()
                .unwrap(),
            "# Y\n"
        );
        let (key, item) = super::raw_entry(&current, "b").unwrap();
        assert_eq!(super::entry_prefix(&key, &item), "# X\n# Y\n");
        let parsed: toml::Value = toml::from_str(&current.to_string()).unwrap();
        assert_eq!(
            parsed["mcp_servers"]["b"]["command"].as_str(),
            Some("latest-b")
        );
        assert_eq!(
            parsed["mcp_servers"]["b"]["metadata"]["latest"].as_bool(),
            Some(true)
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
            let mut trailing = doc.trailing().as_str().unwrap_or("").to_string();
            if !trailing.is_empty() {
                let prefix = table
                    .decor()
                    .prefix()
                    .and_then(|raw| raw.as_str())
                    .unwrap_or("")
                    .to_string();
                // EOFコメントと見出しが同じ物理行にならない分だけ終端する。
                if !trailing.ends_with('\n')
                    && trailing
                        .rsplit('\n')
                        .next()
                        .is_some_and(|line| line.contains('#'))
                    && !prefix.starts_with('\n')
                    && !prefix.starts_with("\r\n")
                {
                    trailing.push('\n');
                }
                table.decor_mut().set_prefix(format!("{trailing}{prefix}"));
                doc.set_trailing("");
            }
        }
    }
    insert(super::toml_mcp_servers_mut(doc), name, item);
}

pub(super) fn raw_entry(doc: &DocumentMut, name: &str) -> Option<(toml_edit::Key, Item)> {
    let servers = doc.get("mcp_servers")?.as_table_like()?;
    Some((servers.key(name)?.clone(), servers.get(name)?.clone()))
}

// 既存keyの書式は保ち、不在entryを戻す場合だけ記録したkeyの表記を使う。
pub(super) fn put_raw(doc: &mut DocumentMut, key: toml_edit::Key, item: Item) {
    // 復元は既存EOFコメントの前へ戻す。新規登録のputとは末尾の扱いを分ける。
    super::toml_mcp_servers_mut(doc);
    let trailing = doc.trailing().clone();
    doc.set_trailing("");
    let name = key.get().to_string();
    let existed = doc
        .get("mcp_servers")
        .and_then(|servers| servers.get(&name))
        .is_some();
    put(doc, &name, item);
    doc.set_trailing(trailing);
    if !existed {
        let servers = super::toml_mcp_servers_mut(doc);
        let item = servers.get(&name).unwrap().clone();
        servers.insert_formatted(&key, item);
    }
}

// edit_toml_gatewayと同じ先頭aliasの改名だけを再現し、実際の移送先と照合する。
fn gateway_rename_layout(original: &DocumentMut, written: &DocumentMut) -> DocumentMut {
    let mut layout = original.clone();
    let Some(servers) = original.get("mcp_servers").and_then(Item::as_table_like) else {
        return layout;
    };
    if servers.get(super::GATEWAY_ENTRY_NAME).is_some() {
        return layout;
    }
    let Some(gateway) = written
        .get("mcp_servers")
        .and_then(|servers| servers.get(super::GATEWAY_ENTRY_NAME))
    else {
        return layout;
    };
    if !gateway
        .get("command")
        .and_then(Item::as_str)
        .is_some_and(super::command_is_gateway_binary)
    {
        return layout;
    }
    let aliases: Vec<_> = servers
        .iter()
        .filter(|(name, item)| {
            super::gateway_edit_matches(name, item.get("command").and_then(Item::as_str))
        })
        .map(|(name, _)| name.to_string())
        .collect();
    let Some(first) = aliases.first() else {
        return layout;
    };
    if aliases.iter().any(|name| {
        written
            .get("mcp_servers")
            .and_then(|servers| servers.get(name))
            .is_some()
    }) {
        return layout;
    }
    rename(
        &mut layout,
        first,
        super::GATEWAY_ENTRY_NAME,
        gateway.clone(),
    );
    layout
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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub(super) enum PrefixDestination {
    Table(Vec<(String, Option<usize>)>),
    InlineKey(Vec<(String, Option<usize>)>, String),
    Eof,
}

pub(super) struct PrefixTransfer {
    pub source_name: String,
    pub prefix: String,
    pub destination: PrefixDestination,
    pub expected_destination_prefix: String,
    pub landing: std::ops::Range<usize>,
}

// 実Moveが観測した範囲だけを保存する。同文検索を継承の証拠にはしない。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct TomlCapture {
    pub id: String,
    pub prefix_hash: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherited: Vec<PrefixInheritance>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct PrefixInheritance {
    pub range: std::ops::Range<usize>,
    pub source_id: String,
    pub source_range: std::ops::Range<usize>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct LivePrefix {
    pub source_id: String,
    pub source_range: std::ops::Range<usize>,
    pub destination: PrefixDestination,
    pub range: std::ops::Range<usize>,
    pub decor_hash: String,
}

#[derive(Clone, Default)]
pub(super) struct TomlProvenance {
    pub capture: Option<TomlCapture>,
    pub live: Vec<LivePrefix>,
}

pub(super) fn capture_prefix<'a>(key: &'a toml_edit::Key, item: &'a Item) -> &'a str {
    entry_prefix(key, item)
}

pub(super) fn source_destination(name: &str, item: &Item) -> PrefixDestination {
    let path = vec![("mcp_servers".to_string(), None)];
    if item.is_table() {
        PrefixDestination::Table(vec![("mcp_servers".into(), None), (name.into(), None)])
    } else {
        PrefixDestination::InlineKey(path, name.into())
    }
}

pub(super) fn valid_live<'a>(doc: &'a DocumentMut, live: &LivePrefix) -> Option<&'a str> {
    let decor = destination_prefix(doc, &live.destination)?;
    if crate::registry::sha256_hex(decor) != live.decor_hash {
        return None;
    }
    decor.get(live.range.clone())
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

pub(super) fn destination_prefix<'a>(
    doc: &'a DocumentMut,
    destination: &PrefixDestination,
) -> Option<&'a str> {
    let raw = match destination {
        PrefixDestination::Eof => return doc.trailing().as_str(),
        PrefixDestination::InlineKey(path, name) => table_at_path(doc.as_table(), path)?
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

fn table_at_path<'a>(mut table: &'a Table, path: &[(String, Option<usize>)]) -> Option<&'a Table> {
    for (name, index) in path {
        let item = table.get(name)?;
        table = if let Some(index) = index {
            item.as_array_of_tables()?.get(*index)?
        } else {
            item.as_table()?
        };
    }
    Some(table)
}

fn prefix_locations(doc: &DocumentMut) -> Vec<(PrefixDestination, String)> {
    fn collect(
        table: &Table,
        path: &mut Vec<(String, Option<usize>)>,
        out: &mut Vec<(PrefixDestination, String)>,
    ) {
        if !path.is_empty() && (!table.is_implicit() || !table.get_values().is_empty()) {
            out.push((
                PrefixDestination::Table(path.clone()),
                table
                    .decor()
                    .prefix()
                    .and_then(|raw| raw.as_str())
                    .unwrap_or("")
                    .to_string(),
            ));
        }
        for (name, item) in table.iter() {
            if item.is_value() {
                out.push((
                    PrefixDestination::InlineKey(path.clone(), name.to_string()),
                    table
                        .key(name)
                        .unwrap()
                        .leaf_decor()
                        .prefix()
                        .and_then(|raw| raw.as_str())
                        .unwrap_or("")
                        .to_string(),
                ));
            } else if let Some(child) = item.as_table() {
                path.push((name.to_string(), None));
                collect(child, path, out);
                path.pop();
            } else if let Some(children) = item.as_array_of_tables() {
                for (index, child) in children.iter().enumerate() {
                    path.push((name.to_string(), Some(index)));
                    collect(child, path, out);
                    path.pop();
                }
            }
        }
    }
    let mut locations = Vec::new();
    collect(doc.as_table(), &mut Vec::new(), &mut locations);
    locations.push((
        PrefixDestination::Eof,
        doc.trailing().as_str().unwrap_or("").to_string(),
    ));
    locations
}

fn entry_prefix<'a>(key: &'a toml_edit::Key, item: &'a Item) -> &'a str {
    let decor = item
        .as_table()
        .map(|table| table.decor())
        .unwrap_or_else(|| key.leaf_decor());
    decor.prefix().and_then(|raw| raw.as_str()).unwrap_or("")
}

fn line_prefix_matches(decor: &str, prefix: &str) -> usize {
    decor
        .match_indices(prefix)
        .filter(|(offset, _)| *offset == 0 || decor.as_bytes()[offset - 1] == b'\n')
        .count()
}

// 最新Rawのprefixだけを判定し、値や未知のフィールドは原本へ差し替えない。
fn raw_prefix_location(
    original: &DocumentMut,
    written: &DocumentMut,
    current: &DocumentMut,
    key: &toml_edit::Key,
    item: &Item,
) -> Option<PrefixDestination> {
    let prefix = entry_prefix(key, item);
    if !prefix.contains('#') {
        return None;
    }
    if raw_entry(original, key.get())
        .is_some_and(|(old_key, old_item)| entry_prefix(&old_key, &old_item) == prefix)
    {
        if !retained_prefix(original, written, current)
            .iter()
            .any(|name| name == key.get())
        {
            return None;
        }
    } else {
        // 初回snapshot後の追加・再Moveは、対応decorの一意な完全prefix一致だけを証拠にする。
        if prefix_locations(original)
            .iter()
            .any(|(_, decor)| line_prefix_matches(decor, prefix) != 0)
        {
            return None;
        }
    }
    let mut witness = None;
    for (destination, decor) in prefix_locations(written) {
        let count = line_prefix_matches(&decor, prefix);
        if count > 1 || (count == 1 && witness.is_some()) {
            return None;
        }
        if count == 1 {
            witness = Some(destination);
        }
    }
    witness.filter(|destination| {
        destination_prefix(current, destination)
            .is_some_and(|decor| line_prefix_matches(decor, prefix) == 1)
    })
}

pub(super) fn dedup_raw_prefix(
    original: &DocumentMut,
    written: &DocumentMut,
    current: &DocumentMut,
    key: &mut toml_edit::Key,
    item: &mut Item,
) {
    if raw_prefix_location(original, written, current, key, item).is_some() {
        if let Some(table) = item.as_table_mut() {
            table.decor_mut().set_prefix("");
        } else {
            key.leaf_decor_mut().set_prefix("");
        }
    }
}

fn table_at_path_mut<'a>(
    mut table: &'a mut Table,
    path: &[(String, Option<usize>)],
) -> Option<&'a mut Table> {
    for (name, index) in path {
        let item = table.get_mut(name)?;
        table = if let Some(index) = index {
            item.as_array_of_tables_mut()?.get_mut(*index)?
        } else {
            item.as_table_mut()?
        };
    }
    Some(table)
}

fn set_destination_prefix(doc: &mut DocumentMut, destination: &PrefixDestination, prefix: String) {
    match destination {
        PrefixDestination::Eof => doc.set_trailing(prefix),
        PrefixDestination::Table(path) => table_at_path_mut(doc.as_table_mut(), path)
            .unwrap()
            .decor_mut()
            .set_prefix(prefix),
        PrefixDestination::InlineKey(path, name) => table_at_path_mut(doc.as_table_mut(), path)
            .unwrap()
            .key_mut(name)
            .unwrap()
            .leaf_decor_mut()
            .set_prefix(prefix),
    }
}

fn original_anchor(
    original: &DocumentMut,
    remaining: &DocumentMut,
    current: &DocumentMut,
    name: &str,
) -> Option<PrefixDestination> {
    let servers = original.get("mcp_servers")?.as_table()?;
    let source = servers.get(name)?;
    let position = if let Some(table) = source.as_table() {
        table.position()?
    } else {
        let mut after = false;
        for (candidate, item) in servers.iter() {
            if candidate == name {
                after = true;
                continue;
            }
            if after
                && item.is_value()
                && remaining
                    .get("mcp_servers")
                    .and_then(|table| table.get(candidate))
                    .is_some()
                && current
                    .get("mcp_servers")
                    .and_then(|table| table.get(candidate))
                    .is_some_and(Item::is_value)
            {
                return Some(PrefixDestination::InlineKey(
                    vec![("mcp_servers".into(), None)],
                    candidate.to_string(),
                ));
            }
        }
        servers.position().unwrap_or(0)
    };
    prefix_locations(remaining)
        .into_iter()
        .filter_map(|(destination, _)| {
            let PrefixDestination::Table(path) = &destination else {
                return None;
            };
            let next = table_at_path(remaining.as_table(), path)?.position()?;
            (next > position && destination_prefix(current, &destination).is_some())
                .then_some((next, destination))
        })
        .min_by_key(|(position, _)| *position)
        .map(|(_, destination)| destination)
        .or(Some(PrefixDestination::Eof))
}

// 新gatewayの追加で原本のEOFコメントが見出しへ移った場合、そこを位置の境界にする。
// コメントは変更せず、構造位置と書込み後のdecor全体が維持された場合だけ追従する。
fn eof_gateway_anchor(
    original: &DocumentMut,
    written: &DocumentMut,
    current: &DocumentMut,
) -> Option<PrefixDestination> {
    let footer = original.trailing().as_str()?;
    if !footer.contains('#')
        || original
            .get("mcp_servers")
            .and_then(|s| s.get(super::GATEWAY_ENTRY_NAME))
            .is_some()
    {
        return None;
    }
    let gateway = written.get("mcp_servers")?.get(super::GATEWAY_ENTRY_NAME)?;
    if !gateway.is_table()
        || !gateway
            .get("command")
            .and_then(Item::as_str)
            .is_some_and(super::command_is_gateway_binary)
    {
        return None;
    }
    let destination = source_destination(super::GATEWAY_ENTRY_NAME, gateway);
    let written_prefix = destination_prefix(written, &destination)?;
    if !written_prefix.starts_with(footer)
        || destination_prefix(current, &destination)? != written_prefix
    {
        return None;
    }
    Some(destination)
}

fn insert_before_anchor(
    doc: &mut DocumentMut,
    anchor: &PrefixDestination,
    entries: Vec<(toml_edit::Key, Item)>,
) {
    let mut maximum = 0;
    visit(doc.as_table_mut(), &mut |table| {
        maximum = maximum.max(table.position().unwrap_or(0))
    });
    let mut next = match anchor {
        PrefixDestination::Table(path) => table_at_path(doc.as_table(), path)
            .unwrap()
            .position()
            .unwrap_or(maximum + 1),
        _ => maximum + 1,
    };
    let place_parent = !matches!(anchor, PrefixDestination::InlineKey(_, _))
        && entries.iter().any(|(_, item)| item.is_value())
        && super::toml_mcp_servers_mut(doc)
            .position()
            .is_none_or(|position| position >= next);
    let count = entries
        .iter()
        .map(|(_, item)| {
            let mut item = item.clone();
            let mut count = 0;
            if let Some(table) = item.as_table_mut() {
                visit(table, &mut |_| count += 1);
            }
            count
        })
        .sum::<usize>()
        + usize::from(place_parent);
    visit(doc.as_table_mut(), &mut |table| {
        if let Some(position) = table.position().filter(|position| *position >= next) {
            table.set_position(position + count);
        }
    });
    if place_parent {
        // inline値は親headerと共に描画されるため、anchor前に親の枠も確保する。
        super::toml_mcp_servers_mut(doc).set_position(next);
        next += 1;
    }
    let mut inserted = Vec::new();
    for (key, mut item) in entries {
        if let Some(table) = item.as_table_mut() {
            let mut positions = Vec::new();
            visit(table, &mut |table| {
                positions.push(table.position().unwrap_or(usize::MAX))
            });
            let mut order: Vec<_> = (0..positions.len()).collect();
            order.sort_by_key(|index| (positions[*index], *index));
            let mut ranks = vec![0; positions.len()];
            for (rank, index) in order.into_iter().enumerate() {
                ranks[index] = rank;
            }
            let mut index = 0;
            visit(table, &mut |table| {
                table.set_position(next + ranks[index]);
                index += 1;
            });
            next += positions.len();
        }
        inserted.push((key, item));
    }
    let servers = super::toml_mcp_servers_mut(doc);
    if let PrefixDestination::InlineKey(path, name) = anchor {
        if path == &vec![("mcp_servers".into(), None)] {
            let prior: Vec<_> = servers
                .iter()
                .map(|(name, item)| (servers.key(name).unwrap().clone(), item.clone()))
                .collect();
            servers.clear();
            let mut batch = Some(inserted);
            for (key, item) in prior {
                if key.get() == name {
                    for (key, item) in batch.take().unwrap_or_default() {
                        servers.insert_formatted(&key, item);
                    }
                }
                servers.insert_formatted(&key, item);
            }
            return;
        }
    }
    for (key, item) in inserted {
        servers.insert_formatted(&key, item);
    }
}

fn merge_prefix_spans(mut spans: Vec<std::ops::Range<usize>>) -> Vec<std::ops::Range<usize>> {
    spans.sort_by_key(|span| (span.start, span.end));
    let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
    for span in spans {
        if let Some(last) = merged.last_mut().filter(|last| span.start <= last.end) {
            last.end = last.end.max(span.end);
        } else {
            merged.push(span);
        }
    }
    merged
}

fn unassigned_prefix_spans(
    span: &std::ops::Range<usize>,
    assigned: &[std::ops::Range<usize>],
) -> Vec<std::ops::Range<usize>> {
    let mut next = span.start;
    let mut remaining = Vec::new();
    for prior in assigned {
        if prior.end <= next || prior.start >= span.end {
            continue;
        }
        if prior.start > next {
            remaining.push(next..prior.start.min(span.end));
        }
        next = next.max(prior.end);
    }
    if next < span.end {
        remaining.push(next..span.end);
    }
    remaining
}

fn proven_prefix_span(
    written: &DocumentMut,
    current: &DocumentMut,
    destination: &PrefixDestination,
    prefix: &str,
) -> Option<std::ops::Range<usize>> {
    if prefix_locations(written)
        .iter()
        .map(|(_, decor)| line_prefix_matches(decor, prefix))
        .sum::<usize>()
        != 1
        || prefix_locations(current)
            .iter()
            .map(|(_, decor)| line_prefix_matches(decor, prefix))
            .sum::<usize>()
            != 1
        || line_prefix_matches(destination_prefix(written, destination)?, prefix) != 1
    {
        return None;
    }
    let decor = destination_prefix(current, destination)?;
    let start = decor
        .match_indices(prefix)
        .find(|(offset, _)| *offset == 0 || decor.as_bytes()[offset - 1] == b'\n')?
        .0;
    Some(start..start + prefix.len())
}

// 同じactual移送先の計画を統合し、証明された物理spanを一度だけ復元へ割り当てる。
// 継承の因果や同文コメントの同一性は推測せず、crossingや曖昧な証拠はRawを保全する。
#[cfg(test)]
pub(super) fn restore_raw_entries(
    original: &DocumentMut,
    written: &DocumentMut,
    doc: &mut DocumentMut,
    entries: Vec<(toml_edit::Key, Item)>,
) {
    restore_raw_entries_with_provenance(original, written, doc, entries, &[]);
}

pub(super) fn restore_raw_entries_with_provenance(
    original: &DocumentMut,
    written: &DocumentMut,
    doc: &mut DocumentMut,
    entries: Vec<(toml_edit::Key, Item)>,
    provenance: &[TomlProvenance],
) {
    super::toml_mcp_servers_mut(doc);
    let evidence = doc.clone();
    // ownerは初期証拠だけで独立に確定し、借用を通じて追加しない。
    let mut owners: std::collections::HashMap<_, _> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, (key, item))| {
            raw_prefix_location(original, written, &evidence, key, item)
                .map(|destination| (index, destination))
        })
        .collect();
    for (index, (key, item)) in entries.iter().enumerate() {
        let Some(meta) = provenance.get(index) else {
            continue;
        };
        let Some(capture) = &meta.capture else {
            continue;
        };
        let prefix = entry_prefix(key, item);
        if crate::registry::sha256_hex(prefix) != capture.prefix_hash {
            continue;
        }
        if let Some(live) = meta.live.iter().find(|live| {
            live.source_id == capture.id
                && live.source_range == (0..prefix.len())
                && valid_live(&evidence, live) == Some(prefix)
        }) {
            owners.insert(index, live.destination.clone());
        }
    }
    let mut layout = gateway_rename_layout(original, written);
    super::toml_mcp_servers_mut(&mut layout);
    let missing: Vec<_> = entries
        .iter()
        .filter(|(key, _)| {
            evidence
                .get("mcp_servers")
                .and_then(|table| table.get(key.get()))
                .is_none()
        })
        .map(|(key, _)| key.get().to_string())
        .collect();
    let mut remaining = layout.clone();
    remove(&mut remaining, &missing);
    let order: std::collections::HashMap<_, _> = layout
        .get("mcp_servers")
        .and_then(Item::as_table_like)
        .map(|servers| {
            servers
                .iter()
                .enumerate()
                .map(|(index, (name, item))| {
                    (
                        name.to_string(),
                        (
                            item.as_table().and_then(Table::position).unwrap_or(0),
                            index,
                        ),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let mut groups = std::collections::BTreeMap::<PrefixDestination, Vec<usize>>::new();
    let mut preserve_raw = std::collections::HashSet::new();
    for (index, (key, item)) in entries.iter().enumerate() {
        if !missing.iter().any(|name| name == key.get()) {
            continue;
        }
        if let Some(old) = layout
            .get("mcp_servers")
            .and_then(|table| table.get(key.get()))
        {
            if old.is_table() != item.is_table() {
                preserve_raw.insert(index);
                continue;
            }
        }
        let anchor = original_anchor(&layout, &remaining, &evidence, key.get())
            .or_else(|| owners.get(&index).cloned());
        if let Some(anchor) = anchor {
            groups.entry(anchor).or_default().push(index);
        }
    }
    let mut actual_groups = std::collections::BTreeMap::<PrefixDestination, Vec<usize>>::new();
    for (anchor, indices) in groups {
        let locations: Vec<_> = indices
            .iter()
            .filter_map(|index| owners.get(index).cloned())
            .collect();
        let Some(actual) = locations.first() else {
            // コメント転送が不要な原本entryは、所有spanがなくても構造位置で戻す。
            let mut placeable = Vec::new();
            for index in indices {
                let (key, item) = &entries[index];
                if !entry_prefix(key, item).contains('#') && raw_entry(&layout, key.get()).is_some()
                {
                    placeable.push(index);
                } else {
                    preserve_raw.insert(index);
                }
            }
            if !placeable.is_empty() {
                let actual = if matches!(anchor, PrefixDestination::Eof) {
                    eof_gateway_anchor(original, written, &evidence).unwrap_or(anchor)
                } else {
                    anchor
                };
                actual_groups.entry(actual).or_default().extend(placeable);
            }
            continue;
        };
        let eof_redirect = matches!(anchor, PrefixDestination::Eof)
            && matches!(actual, PrefixDestination::Table(path) if path.len() == 2 && path[0].0 == "mcp_servers" && original.get("mcp_servers").and_then(|table| table.get(&path[1].0)).is_none());
        if (actual != &anchor && !eof_redirect)
            || locations.iter().any(|destination| destination != actual)
        {
            let mut placeable = Vec::new();
            for index in indices {
                let (key, item) = &entries[index];
                if !entry_prefix(key, item).contains('#') && raw_entry(&layout, key.get()).is_some()
                {
                    placeable.push(index);
                } else {
                    preserve_raw.insert(index);
                }
            }
            if !placeable.is_empty() {
                let actual = if matches!(anchor, PrefixDestination::Eof) {
                    eof_gateway_anchor(original, written, &evidence).unwrap_or(anchor)
                } else {
                    anchor
                };
                actual_groups.entry(actual).or_default().extend(placeable);
            }
            continue;
        }
        actual_groups
            .entry(actual.clone())
            .or_default()
            .extend(indices);
    }
    let mut plans = Vec::new();
    let mut handled = std::collections::HashSet::new();
    for (actual, mut indices) in actual_groups {
        indices.sort_by_key(|index| {
            order
                .get(entries[*index].0.get())
                .copied()
                .unwrap_or((usize::MAX, *index))
        });
        let mut spans = std::collections::HashMap::new();
        let mut invalid = false;
        for index in &indices {
            if owners.get(index) != Some(&actual) {
                continue;
            }
            let prefix = entry_prefix(&entries[*index].0, &entries[*index].1);
            let observed = provenance.get(*index).and_then(|meta| {
                let capture = meta.capture.as_ref()?;
                if crate::registry::sha256_hex(prefix) != capture.prefix_hash {
                    return None;
                }
                meta.live
                    .iter()
                    .find(|live| {
                        live.source_id == capture.id
                            && live.destination == actual
                            && live.source_range == (0..prefix.len())
                            && valid_live(&evidence, live) == Some(prefix)
                    })
                    .map(|live| live.range.clone())
            });
            if let Some(span) =
                observed.or_else(|| proven_prefix_span(written, &evidence, &actual, prefix))
            {
                spans.insert(*index, span);
            }
        }
        // crossingは所有者の採否とは別に検知し、移送先全体をそのまま保つ。
        let geometry: Vec<_> = indices
            .iter()
            .filter_map(|index| {
                spans.get(index).cloned().or_else(|| {
                    let prefix = entry_prefix(&entries[*index].0, &entries[*index].1);
                    proven_prefix_span(written, &evidence, &actual, prefix)
                })
            })
            .collect();
        if geometry.iter().enumerate().any(|(i, a)| {
            geometry.iter().skip(i + 1).any(|b| {
                a.start < b.end
                    && b.start < a.end
                    && !((a.start <= b.start && a.end >= b.end)
                        || (b.start <= a.start && b.end >= a.end))
            })
        }) {
            let mut prefixless = Vec::new();
            for index in indices {
                let (key, item) = &entries[index];
                if !entry_prefix(key, item).contains('#') && raw_entry(&layout, key.get()).is_some()
                {
                    handled.insert(index);
                    prefixless.push((key.clone(), item.clone()));
                } else {
                    preserve_raw.insert(index);
                }
            }
            if !prefixless.is_empty() {
                plans.push((actual, Vec::new(), prefixless));
            }
            continue;
        }
        // 明示された継承を座標へ変換する。文面一致は因果の根拠にしない。
        let valid_edge = |child: usize, source: usize| -> Option<&PrefixInheritance> {
            let child_capture = provenance.get(child)?.capture.as_ref()?;
            let source_capture = provenance.get(source)?.capture.as_ref()?;
            let child_prefix = entry_prefix(&entries[child].0, &entries[child].1);
            let source_prefix = entry_prefix(&entries[source].0, &entries[source].1);
            if child_capture.id == source_capture.id
                || crate::registry::sha256_hex(child_prefix) != child_capture.prefix_hash
                || crate::registry::sha256_hex(source_prefix) != source_capture.prefix_hash
            {
                return None;
            }
            child_capture.inherited.iter().find(|edge| {
                edge.source_id == source_capture.id
                    && edge.range.start < edge.range.end
                    && child_prefix
                        .get(edge.range.clone())
                        .zip(source_prefix.get(edge.source_range.clone()))
                        .is_some_and(|(a, b)| a == b)
            })
        };
        for _ in 0..indices.len() {
            let mut added = Vec::new();
            for child in &indices {
                if spans.contains_key(child) {
                    continue;
                }
                for source in &indices {
                    let (Some(edge), Some(source_span)) =
                        (valid_edge(*child, *source), spans.get(source))
                    else {
                        continue;
                    };
                    let Some(start) = source_span
                        .start
                        .checked_add(edge.source_range.start)
                        .and_then(|v| v.checked_sub(edge.range.start))
                    else {
                        continue;
                    };
                    let prefix = entry_prefix(&entries[*child].0, &entries[*child].1);
                    let Some(end) = start.checked_add(prefix.len()) else {
                        continue;
                    };
                    if destination_prefix(&evidence, &actual).and_then(|d| d.get(start..end))
                        == Some(prefix)
                    {
                        added.push((*child, start..end));
                        break;
                    }
                }
            }
            if added.is_empty() {
                break;
            }
            spans.extend(added);
        }
        let owner_union = merge_prefix_spans(spans.values().cloned().collect());
        if owner_union.is_empty()
            && indices
                .iter()
                .any(|index| entry_prefix(&entries[*index].0, &entries[*index].1).contains('#'))
        {
            preserve_raw.extend(indices);
            continue;
        }
        indices.retain(|index| {
            let (key, item) = &entries[*index];
            let prefix = entry_prefix(key, item);
            if !prefix.contains('#') || spans.contains_key(index) {
                return true;
            }
            preserve_raw.insert(*index);
            false
        });
        // 同じ物理spanを複数Rawに割り当てるのは、範囲が一致する実継承だけ。
        let mut independent = std::collections::HashSet::new();
        for (position, first) in indices.iter().enumerate() {
            for second in indices.iter().skip(position + 1) {
                let (Some(a), Some(b)) = (spans.get(first), spans.get(second)) else {
                    continue;
                };
                let overlap = a.start.max(b.start)..a.end.min(b.end);
                if overlap.start >= overlap.end {
                    continue;
                }
                let explains =
                    |child: usize,
                     source: usize,
                     child_span: &std::ops::Range<usize>,
                     source_span: &std::ops::Range<usize>| {
                        valid_edge(child, source).is_some_and(|edge| {
                            child_span.start.checked_add(edge.range.start)
                                == source_span.start.checked_add(edge.source_range.start)
                                && child_span.start + edge.range.start <= overlap.start
                                && child_span.start + edge.range.end >= overlap.end
                        })
                    };
                if !explains(*first, *second, a, b) && !explains(*second, *first, b, a) {
                    independent.insert(*second);
                }
            }
        }
        indices.retain(|index| {
            if independent.contains(index) {
                spans.remove(index);
                preserve_raw.insert(*index);
                false
            } else {
                true
            }
        });
        for first in spans.values() {
            for second in spans.values() {
                let intersects = first.start < second.end && second.start < first.end;
                let contains = (first.start <= second.start && first.end >= second.end)
                    || (second.start <= first.start && second.end >= first.end);
                if intersects && !contains {
                    invalid = true;
                }
            }
        }
        if invalid {
            preserve_raw.extend(indices);
            continue;
        }
        // 原本順を保ち、不在entryは包含spanの短→長、非重複spanの実順を使う。
        // 同一spanは既存のMove記録順をtie-breakにし、名前順へ置き換えない。
        let precedes = |first: usize, second: usize| {
            if let (Some(first_order), Some(second_order)) = (
                order.get(entries[first].0.get()),
                order.get(entries[second].0.get()),
            ) {
                return first_order < second_order;
            }
            let (Some(first_span), Some(second_span)) = (spans.get(&first), spans.get(&second))
            else {
                return false;
            };
            if first_span == second_span {
                return first < second;
            }
            if first_span.start >= second_span.start && first_span.end <= second_span.end {
                return true;
            }
            if second_span.start >= first_span.start && second_span.end <= first_span.end {
                return false;
            }
            first_span.end <= second_span.start
        };
        let mut pending = indices.clone();
        let mut sorted = Vec::new();
        while !pending.is_empty() {
            let Some(next) = pending.iter().position(|index| {
                !pending
                    .iter()
                    .any(|other| other != index && precedes(*other, *index))
            }) else {
                invalid = true;
                break;
            };
            sorted.push(pending.remove(next));
        }
        if invalid {
            preserve_raw.extend(indices);
            continue;
        }
        let decor = destination_prefix(&evidence, &actual).unwrap();
        let mut assigned = Vec::new();
        let mut batch = Vec::new();
        for index in sorted {
            let (mut key, mut item) = entries[index].clone();
            if let Some(span) = spans.get(&index) {
                let prefix = unassigned_prefix_spans(span, &assigned)
                    .iter()
                    .map(|range| &decor[range.clone()])
                    .collect::<String>();
                if let Some(table) = item.as_table_mut() {
                    table.decor_mut().set_prefix(prefix);
                } else {
                    key.leaf_decor_mut().set_prefix(prefix);
                }
                assigned.push(span.clone());
                assigned = merge_prefix_spans(assigned);
            }
            handled.insert(index);
            batch.push((key, item));
        }
        plans.push((
            actual,
            merge_prefix_spans(spans.into_values().collect()),
            batch,
        ));
    }
    // 全計画は初期evidence座標。挿入に先立って各actualを一度だけ更新する。
    for (actual, spans, _) in &plans {
        if spans.is_empty() {
            continue;
        }
        let mut updated = destination_prefix(&evidence, actual).unwrap().to_string();
        for span in spans.iter().rev() {
            updated.replace_range(span.clone(), "");
        }
        set_destination_prefix(doc, actual, updated);
    }
    for (actual, _, batch) in plans {
        insert_before_anchor(doc, &actual, batch);
    }
    for (index, (mut key, mut item)) in entries.into_iter().enumerate() {
        if handled.contains(&index) {
            continue;
        }
        let verified_live = provenance
            .get(index)
            .and_then(|meta| {
                let capture = meta.capture.as_ref()?;
                let prefix = entry_prefix(&key, &item);
                if crate::registry::sha256_hex(prefix) != capture.prefix_hash {
                    return None;
                }
                meta.live.iter().find(|live| {
                    live.source_id == capture.id
                        && live.source_range == (0..prefix.len())
                        && valid_live(&evidence, live) == Some(prefix)
                })
            })
            .is_some();
        if verified_live && !preserve_raw.contains(&index) {
            dedup_raw_prefix(original, written, &evidence, &mut key, &mut item);
        }
        put_raw(doc, key, item);
    }
}

// 原本から実際に転送されたprefixが同じ移送先へ残る証拠がある場合だけ重複を省く。
pub(super) fn retained_prefix(
    original: &DocumentMut,
    written: &DocumentMut,
    current: &DocumentMut,
) -> Vec<String> {
    let mut replay = gateway_rename_layout(original, written);
    let missing: Vec<_> = replay
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
    remove(&mut replay, &missing)
        .into_iter()
        .filter(|transfer| {
            if !transfer.prefix.contains('#') {
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
                if let Some(prefix) = table
                    .decor()
                    .prefix()
                    .and_then(|raw| raw.as_str())
                    .filter(|prefix| prefix.contains('#'))
                {
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
                if !prefix.contains('#') {
                    continue;
                }
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
                        destination: PrefixDestination::InlineKey(
                            vec![("mcp_servers".into(), None)],
                            next.clone(),
                        ),
                        expected_destination_prefix: String::new(),
                        landing: 0..0,
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
            landing: 0..0,
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
    let mut offsets = std::collections::BTreeMap::new();
    for transfer in &mut transfers {
        let offset = offsets.entry(transfer.destination.clone()).or_insert(0);
        transfer.landing = *offset..*offset + transfer.prefix.len();
        *offset += transfer.prefix.len();
        transfer.expected_destination_prefix = destination_prefix(doc, &transfer.destination)
            .unwrap_or("")
            .to_string();
    }
    transfers
}
