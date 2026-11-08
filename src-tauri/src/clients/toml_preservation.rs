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

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PrefixDestination {
    Table(Vec<(String, Option<usize>)>),
    InlineKey(Vec<(String, Option<usize>)>, String),
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

// 同じ移送先へまとまった登録は、prefixのない兄弟も含めて元の順で境界内へ戻す。
pub(super) fn restore_raw_entries(
    original: &DocumentMut,
    written: &DocumentMut,
    doc: &mut DocumentMut,
    entries: Vec<(toml_edit::Key, Item)>,
) {
    super::toml_mcp_servers_mut(doc);
    let evidence = doc.clone();
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
    for (index, (key, item)) in entries.iter().enumerate() {
        if !missing.iter().any(|name| name == key.get()) {
            continue;
        }
        if let Some(old) = layout
            .get("mcp_servers")
            .and_then(|table| table.get(key.get()))
        {
            if old.is_table() != item.is_table() {
                continue;
            }
        }
        let anchor = original_anchor(&layout, &remaining, &evidence, key.get())
            .or_else(|| raw_prefix_location(original, written, &evidence, key, item));
        if let Some(anchor) = anchor {
            groups.entry(anchor).or_default().push(index);
        }
    }
    let mut handled = std::collections::HashSet::new();
    for (anchor, mut indices) in groups {
        indices.sort_by_key(|index| {
            order
                .get(entries[*index].0.get())
                .copied()
                .unwrap_or((usize::MAX, usize::MAX))
        });
        let locations: Vec<_> = indices
            .iter()
            .filter_map(|index| {
                raw_prefix_location(
                    original,
                    written,
                    &evidence,
                    &entries[*index].0,
                    &entries[*index].1,
                )
                .map(|destination| (*index, destination))
            })
            .collect();
        let Some((_, actual)) = locations.first() else {
            continue;
        };
        let eof_redirect = matches!(anchor, PrefixDestination::Eof)
            && matches!(actual, PrefixDestination::Table(path) if path.len() == 2 && path[0].0 == "mcp_servers" && original.get("mcp_servers").and_then(|table| table.get(&path[1].0)).is_none());
        if (actual != &anchor && !eof_redirect)
            || locations
                .iter()
                .any(|(_, destination)| destination != actual)
        {
            continue;
        }
        if locations.len() == indices.len() {
            // 各entryの転送prefixを証明できる場合は、移送decor内の実順序を優先する。
            let decor = destination_prefix(&evidence, actual).unwrap();
            indices.sort_by_key(|index| {
                let prefix = entry_prefix(&entries[*index].0, &entries[*index].1);
                decor
                    .match_indices(prefix)
                    .find(|(offset, _)| *offset == 0 || decor.as_bytes()[offset - 1] == b'\n')
                    .unwrap()
                    .0
            });
        }
        for (index, _) in &locations {
            let prefix = entry_prefix(&entries[*index].0, &entries[*index].1);
            let current = destination_prefix(doc, actual).unwrap().to_string();
            if let Some(offset) = current
                .match_indices(prefix)
                .find(|(offset, _)| *offset == 0 || current.as_bytes()[offset - 1] == b'\n')
                .map(|(offset, _)| offset)
            {
                let mut updated = current;
                updated.replace_range(offset..offset + prefix.len(), "");
                set_destination_prefix(doc, actual, updated);
            }
        }
        let batch = indices
            .iter()
            .map(|index| {
                handled.insert(*index);
                entries[*index].clone()
            })
            .collect();
        insert_before_anchor(doc, actual, batch);
    }
    for (index, (mut key, mut item)) in entries.into_iter().enumerate() {
        if handled.contains(&index) {
            continue;
        }
        dedup_raw_prefix(original, written, &evidence, &mut key, &mut item);
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
