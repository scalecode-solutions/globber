// Git integration — diff-aware file discovery.
//
// --git-changed [ref] yields only files modified since a git ref.
// Uses `git diff --name-only` under the hood.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A set of changed files for fast membership tests against walk paths.
///
/// Lookups first check the file name (no syscall), and only canonicalize
/// the candidate path when the name matches a changed file.
#[derive(Debug, Clone, Default)]
pub struct ChangedSet {
    names: HashSet<OsString>,
    paths: HashSet<PathBuf>,
}

impl ChangedSet {
    pub fn new(changed: &[PathBuf]) -> Self {
        let mut set = ChangedSet::default();
        for p in changed {
            if let Some(name) = p.file_name() {
                set.names.insert(name.to_os_string());
            }
            set.paths.insert(std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()));
        }
        set
    }

    /// Whether `path` (relative to the cwd, or absolute) is a changed file.
    pub fn contains(&self, path: &Path) -> bool {
        match path.file_name() {
            Some(name) if self.names.contains(name) => std::fs::canonicalize(path)
                .map(|c| self.paths.contains(&c))
                .unwrap_or(false),
            _ => false,
        }
    }
}

/// Get the files changed since `ref_name` in the repo containing `root`:
/// committed, staged, and unstaged changes, plus untracked files.
///
/// Changes are measured from the merge base of `ref_name` and `HEAD`, so
/// `-G main` on a feature branch lists what the branch changed, not what
/// landed on main since the branch forked. For an ancestor ref (`HEAD~3`,
/// a release tag) the merge base is the ref itself.
///
/// Returns absolute paths. Fails if `ref_name` does not name a commit.
pub fn changed_files(root: &Path, ref_name: &str) -> Result<Vec<PathBuf>, String> {
    let repo_root = find_repo_root(root)?;

    let commit = git(&repo_root, &["rev-parse", "--verify", "--quiet", &format!("{}^{{commit}}", ref_name)])
        .map_err(|_| format!("unknown git ref: {}", ref_name))?;
    let base = git(&repo_root, &["merge-base", &commit, "HEAD"]).unwrap_or(commit);

    // -z: raw paths, no quoting of unusual characters.
    let diff = git(&repo_root, &["diff", "--name-only", "-z", &base])?;
    let untracked = git(&repo_root, &["ls-files", "--others", "--exclude-standard", "-z"])?;

    let mut paths: Vec<PathBuf> = diff
        .split('\0')
        .chain(untracked.split('\0'))
        .filter(|l| !l.is_empty())
        .map(|l| repo_root.join(l))
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// The files git considers part of the working tree under `dir`: tracked
/// files (even ones matching an ignore rule) plus untracked files that are
/// not ignored — `git ls-files --cached --others --exclude-standard`.
/// Paths are relative to `dir`. With `nested`, recurses into initialized
/// submodules and untracked nested repositories.
pub fn repo_files(dir: &Path, nested: bool) -> Result<Vec<PathBuf>, String> {
    Ok(list_files(dir, nested)?.into_iter().map(|(p, _)| p).collect())
}

/// Like [`repo_files`], also saying whether each file is tracked.
pub(crate) fn list_files(dir: &Path, nested: bool) -> Result<Vec<(PathBuf, bool)>, String> {
    let not_repo = |e: String| {
        if e.contains("not a git repository") {
            format!("not a git repository: {}", dir.display())
        } else {
            e
        }
    };
    let tracked = git(dir, &["ls-files", "--cached", "-z"]).map_err(not_repo)?;
    let untracked = git(dir, &["ls-files", "--others", "--exclude-standard", "-z"]).map_err(not_repo)?;
    let submodules = submodule_paths(dir);

    let mut files = Vec::new();
    let mut nested_dirs = Vec::new();
    let listed = tracked
        .split('\0')
        .map(|p| (p, true))
        .chain(untracked.split('\0').map(|p| (p, false)));
    for (p, is_tracked) in listed.filter(|(p, _)| !p.is_empty()) {
        if let Some(d) = p.strip_suffix('/') {
            // Untracked nested repository: git lists it, not its contents.
            nested_dirs.push(PathBuf::from(d));
        } else if is_tracked && submodules.iter().any(|s| s == Path::new(p)) {
            nested_dirs.push(PathBuf::from(p));
        } else {
            files.push((PathBuf::from(p), is_tracked));
        }
    }

    if nested {
        for d in nested_dirs {
            let sub = dir.join(&d);
            if sub.join(".git").exists() {
                if let Ok(inner) = list_files(&sub, true) {
                    files.extend(inner.into_iter().map(|(f, t)| (d.join(f), t)));
                }
            }
        }
    }
    Ok(files)
}

/// Submodule paths relative to `dir`, from the repository's .gitmodules.
fn submodule_paths(dir: &Path) -> Vec<PathBuf> {
    let Ok(info) = git(dir, &["rev-parse", "--show-toplevel", "--show-prefix"]) else {
        return Vec::new();
    };
    let mut lines = info.lines();
    let top = PathBuf::from(lines.next().unwrap_or(""));
    let prefix = lines.next().unwrap_or("");
    if !top.join(".gitmodules").exists() {
        return Vec::new();
    }
    let Ok(config) = git(
        &top,
        &["config", "-f", ".gitmodules", "--get-regexp", r"^submodule\..*\.path$"],
    ) else {
        return Vec::new();
    };
    config
        .lines()
        .filter_map(|l| l.split_once(' '))
        .filter_map(|(_, path)| path.strip_prefix(prefix).map(PathBuf::from))
        .collect()
}

/// Find the git repository root from a given path.
fn find_repo_root(from: &Path) -> Result<PathBuf, String> {
    git(from, &["rev-parse", "--show-toplevel"])
        .map(|s| PathBuf::from(s.trim_end()))
        .map_err(|_| format!("not a git repository: {}", from.display()))
}

/// Run git in `dir`, returning stdout, or stderr as the error.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;
    if output.status.success() {
        let out = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(if args.contains(&"-z") { out } else { out.trim().to_string() })
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
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
