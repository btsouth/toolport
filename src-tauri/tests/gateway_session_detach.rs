//! Regression guard for gateways launched as process-group leaders (SBS-1063).
//!
//! Codex puts each MCP child in a new process group before `exec`. A process-group
//! leader cannot call `setsid`, so a gateway that ignored the resulting `EPERM`
//! stayed attached to Codex's controlling terminal. Its login-shell PATH probe then
//! received SIGTTIN as a background group and stopped the whole gateway before it
//! could answer `toolport_status`.

#![cfg(unix)]

use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

unsafe fn get_process_group(pid: i32) -> i32 {
    extern "C" {
        fn getpgid(pid: i32) -> i32;
    }
    getpgid(pid)
}

unsafe fn get_session(pid: i32) -> i32 {
    extern "C" {
        fn getsid(pid: i32) -> i32;
    }
    getsid(pid)
}

#[test]
fn process_group_leader_gateway_still_creates_its_own_session() {
    let dir = std::env::temp_dir().join(format!(
        "toolport-sbs1063-session-detach-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test data dir");

    let gateway = env!("CARGO_BIN_EXE_toolport-gateway");
    let child = Command::new(gateway)
        .env("TOOLPORT_REGISTRY", dir.join("registry.json"))
        .env("TOOLPORT_CLIENT_ID", "session-detach-test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("spawn gateway as a process-group leader");
    let mut child = ChildGuard(child);
    let pid = child.0.id() as i32;

    assert_eq!(
        unsafe { get_process_group(pid) },
        pid,
        "fixture must launch the gateway as its own process-group leader"
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        assert!(
            child.0.try_wait().expect("read gateway status").is_none(),
            "gateway exited before detaching"
        );
        if unsafe { get_session(pid) } == pid {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "gateway remained in its launcher's terminal session"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    drop(child);
    std::fs::remove_dir_all(&dir).expect("remove test data dir");
}
