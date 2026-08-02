//! Stamp the binary with the git commit it was built from.
//!
//! Deploys here are `scp` + `install`, with no package manager to tell you
//! what landed, and the journal is read long after the fact — so "which
//! build produced this log?" has to be answerable from the binary itself.
//! `SWEAM_BUILD` becomes part of `--version` and is logged at startup.

use std::process::Command;

fn main() {
    // Re-run on every build. Watching .git/HEAD and .git/index is not
    // enough: editing a tracked file without staging it changes what
    // `git status` reports but touches neither path, so the stamp went
    // stale and a binary built from a dirty tree claimed a clean commit —
    // exactly the confusion this is meant to prevent. A path that never
    // exists is the documented way to ask cargo to always re-run.
    println!("cargo:rerun-if-changed=(always-rerun)");

    let commit = run(&["rev-parse", "--short", "HEAD"]);
    // A tree with uncommitted changes builds something that no commit
    // describes; say so rather than name a commit it isn't.
    let dirty = run(&["status", "--porcelain"]).is_some_and(|out| !out.is_empty());

    let build = match commit {
        Some(commit) if dirty => format!("{commit}-dirty"),
        Some(commit) => commit,
        // No git (source tarball, or git not installed): not an error.
        None => "unknown".to_owned(),
    };
    println!("cargo:rustc-env=SWEAM_BUILD={build}");
}

/// Run a git command, returning its trimmed stdout when it succeeds.
fn run(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
