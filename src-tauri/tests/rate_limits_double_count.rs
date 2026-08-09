use conduit_lib::rate_limits::{check_and_count, Cap};

#[test]
fn overlapping_caps_same_window_and_tool_do_not_double_count() {
    let caps = vec![
        Cap {
            id: "org-day".into(),
            window: "day".into(),
            max_calls: 2,
            tool: None,
        },
        Cap {
            id: "team-day".into(),
            window: "day".into(),
            max_calls: 2,
            tool: None,
        },
    ];

    assert!(check_and_count(&caps, "srv", "echo").is_ok());
    assert!(check_and_count(&caps, "srv", "echo").is_ok());
    assert!(check_and_count(&caps, "srv", "echo").is_err());
}
