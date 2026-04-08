// Git integration — diff-aware file discovery.
//
// --git-changed [ref] yields only files modified since a git ref.
// Uses `git diff --name-only` under the hood.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Get the list of files changed since `ref_name` in the repo at `root`.
///
/// Returns paths relative to the repo root.
pub fn changed_files(root: &Path, ref_name: &str) -> Result<Vec<PathBuf>, String> {
    // Find the repo root.
    let repo_root = find_repo_root(root)?;

    // Get files changed between ref and working tree (staged + unstaged + untracked).
    let mut paths = Vec::new();

    // Diff against the ref (committed changes on this branch).
    let output = Command::new("git")
        .args(["diff", "--name-only", ref_name])
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if !line.is_empty() {
                paths.push(repo_root.join(line));
            }
        }
    }

    // Also include staged but not yet committed.
    let output = Command::new("git")
        .args(["diff", "--name-only", "--cached"])
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if !line.is_empty() {
                let p = repo_root.join(line);
                if !paths.contains(&p) {
                    paths.push(p);
                }
            }
        }
    }

    // Also include untracked files.
    let output = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if !line.is_empty() {
                let p = repo_root.join(line);
                if !paths.contains(&p) {
                    paths.push(p);
                }
            }
        }
    }

    paths.sort();
    Ok(paths)
}

/// Find the git repository root from a given path.
fn find_repo_root(from: &Path) -> Result<PathBuf, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(from)
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "not a git repository: {}",
            from.display()
        ));
    }

    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(PathBuf::from(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_repo_root_works() {
        // This test runs inside the globber repo itself.
        let root = find_repo_root(Path::new("."));
        assert!(root.is_ok());
    }
}
