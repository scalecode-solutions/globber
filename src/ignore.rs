// Gitignore support for the walker.
//
// The stack is an Arc-linked list so each directory can extend its
// parent's rules without copying them.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use crate::pattern::Pattern;

#[derive(Debug, Clone)]
struct IgnoreRule {
    pattern: Pattern,
    negated: bool,
}

#[derive(Debug)]
struct IgnoreNode {
    rules: Vec<IgnoreRule>,
    parent: IgnoreStack,
}

/// The ignore rules in effect for a directory, innermost last.
#[derive(Debug, Clone, Default)]
pub(crate) struct IgnoreStack(Option<Arc<IgnoreNode>>);

impl IgnoreStack {
    /// Rules in effect at the root of a walk.
    pub(crate) fn for_walk_root(_scope: &Path) -> Self {
        IgnoreStack::default()
    }

    /// Extend the stack with `dir/.gitignore`, if present.
    pub(crate) fn enter_dir(&self, dir: &Path) -> Self {
        match load_gitignore(&dir.join(".gitignore")) {
            Some(rules) => IgnoreStack(Some(Arc::new(IgnoreNode { rules, parent: self.clone() }))),
            None => self.clone(),
        }
    }

    /// Whether `path` is ignored. Later (deeper) rules override earlier ones.
    pub(crate) fn is_ignored(&self, path: &Path, _is_dir: bool) -> bool {
        let Some(path_str) = path.to_str() else { return false };
        let filename = path.file_name().and_then(|f| f.to_str()).unwrap_or(path_str);

        let mut nodes = Vec::new();
        let mut cur = &self.0;
        while let Some(node) = cur {
            nodes.push(node);
            cur = &node.parent.0;
        }

        let mut ignored = false;
        for node in nodes.iter().rev() {
            for rule in &node.rules {
                if rule.pattern.matches(path_str) || rule.pattern.matches(filename) {
                    ignored = !rule.negated;
                }
            }
        }
        ignored
    }
}

/// Load and parse a .gitignore file into ignore rules.
fn load_gitignore(path: &Path) -> Option<Vec<IgnoreRule>> {
    let content = fs::read_to_string(path).ok()?;
    let mut rules = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (negated, pat_str) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        let pat_str = pat_str.strip_prefix('/').unwrap_or(pat_str);
        let pat_str = pat_str.strip_suffix('/').unwrap_or(pat_str);
        if let Ok(pattern) = Pattern::new(&format!("**/{}", pat_str)) {
            rules.push(IgnoreRule { pattern, negated });
        }
    }
    if rules.is_empty() { None } else { Some(rules) }
}
