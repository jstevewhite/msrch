use std::process::Command;

/// Embed the git commit the binary was built from as MSRCH_GIT_HASH.
/// Best-effort: "unknown" outside a git checkout; a dirty working tree gets a
/// "-dirty" suffix (the dirty flag is only recomputed when HEAD moves, so
/// authoritative hashes come from release builds off a clean, committed tree).
fn main() {
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        // Re-run when HEAD moves: new commit on the current branch, or branch switch.
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        if let Some(head_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!("cargo:rerun-if-changed={git_dir}/{head_ref}");
        }
    }

    let hash = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let dirty = git(&["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let suffix = if dirty { "-dirty" } else { "" };
    println!("cargo:rustc-env=MSRCH_GIT_HASH={hash}{suffix}");
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
