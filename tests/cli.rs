//! CLI smoke tests: both binaries must build, parse args, and respond to
//! `--version` / `--help`. Deeper behavior lands with real implementations.

fn run(bin_env: &str, args: &[&str]) -> (bool, String) {
    let output = std::process::Command::new(env_bin(bin_env))
        .args(args)
        .output()
        .expect("spawn binary");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

fn env_bin(name: &str) -> &'static str {
    match name {
        "CARGO_BIN_EXE_agentd" => env!("CARGO_BIN_EXE_agentd"),
        "CARGO_BIN_EXE_agentdctl" => env!("CARGO_BIN_EXE_agentdctl"),
        _ => panic!("unknown binary {name}"),
    }
}

#[test]
fn agentd_version_works() {
    let (ok, out) = run("CARGO_BIN_EXE_agentd", &["--version"]);
    assert!(ok, "agentd --version should succeed");
    assert!(
        out.contains("agentd"),
        "output should name the binary: {out}"
    );
}

#[test]
fn agentdctl_help_works() {
    let (ok, out) = run("CARGO_BIN_EXE_agentdctl", &["--help"]);
    assert!(ok, "agentdctl --help should succeed");
    for sub in [
        "init",
        "register",
        "update",
        "unregister",
        "list",
        "reload",
        "status",
    ] {
        assert!(out.contains(sub), "help should mention `{sub}`: {out}");
    }
}
