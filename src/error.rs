use std::{fmt, io};

/// The position in a pattern where a parse error occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatternError {
    /// Byte offset into the pattern string.
    pub pos: usize,
    /// What went wrong.
    pub kind: PatternErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternErrorKind {
    /// More than two consecutive `*` characters.
    InvalidWildcard,
    /// `**` is not a standalone path component (e.g. `a**b`).
    RecursiveNotAlone,
    /// Unclosed `[` bracket expression.
    UnclosedBracket,
    /// Empty bracket expression `[]` or `[!]`.
    EmptyBracket,
    /// Unclosed brace expression `{`.
    UnclosedBrace,
    /// Empty brace alternative (e.g. `{,}`).
    EmptyBraceAlternative,
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self.kind {
            PatternErrorKind::InvalidWildcard => "wildcards are either `*` or `**`",
            PatternErrorKind::RecursiveNotAlone => {
                "`**` must be an entire path component"
            }
            PatternErrorKind::UnclosedBracket => "unclosed `[` bracket expression",
            PatternErrorKind::EmptyBracket => "empty bracket expression",
            PatternErrorKind::UnclosedBrace => "unclosed `{` brace expression",
            PatternErrorKind::EmptyBraceAlternative => "empty brace alternative",
        };
        write!(f, "pattern error at byte {}: {}", self.pos, msg)
    }
}

impl std::error::Error for PatternError {}

/// Errors returned by filesystem walking.
#[derive(Debug)]
pub enum GlobError {
    /// A pattern failed to compile.
    Pattern(PatternError),
    /// An I/O error while reading a directory.
    Io { path: std::path::PathBuf, error: io::Error },
}

impl fmt::Display for GlobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GlobError::Pattern(e) => write!(f, "{}", e),
            GlobError::Io { path, error } => {
                write!(f, "reading `{}`: {}", path.display(), error)
            }
        }
    }
}

impl std::error::Error for GlobError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GlobError::Pattern(e) => Some(e),
            GlobError::Io { error, .. } => Some(error),
        }
    }
}

impl From<PatternError> for GlobError {
    fn from(e: PatternError) -> Self {
        GlobError::Pattern(e)
    }
}
