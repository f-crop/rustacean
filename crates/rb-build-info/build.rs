fn main() {
    // Re-run when git HEAD or its references change, or when the env vars change.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
    println!("cargo:rerun-if-env-changed=RB_BUILD_SHA");
    println!("cargo:rerun-if-env-changed=RB_BUILD_TIMESTAMP");
    println!("cargo:rerun-if-env-changed=RB_BUILD_DIRTY");

    // RB_BUILD_SHA: prefer env (set by Docker builder), fall back to git.
    let sha = std::env::var("RB_BUILD_SHA")
        .ok()
        .or_else(git_sha)
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=RB_BUILD_SHA={sha}");

    // RB_BUILD_TIMESTAMP: prefer env, fall back to git commit timestamp.
    let ts = std::env::var("RB_BUILD_TIMESTAMP")
        .ok()
        .or_else(git_commit_timestamp)
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=RB_BUILD_TIMESTAMP={ts}");

    // RB_BUILD_DIRTY: prefer env, fall back to working-tree inspection.
    let dirty = std::env::var("RB_BUILD_DIRTY")
        .ok()
        .unwrap_or_else(|| working_tree_dirty().to_string());
    println!("cargo:rustc-env=RB_BUILD_DIRTY={dirty}");
}

fn git_sha() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_commit_timestamp() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["log", "-1", "--format=%cI"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn working_tree_dirty() -> bool {
    std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}
