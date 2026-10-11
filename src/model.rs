//! The review model: scopes, changed files, and the session's comments, never lost to a refresh.

/// Which set of changes the Changes view shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    Uncommitted,
    Branch,
    LastTurn,
    /// A picked run of commits, diffed `A^` against `B`.
    Commits,
    /// Worktree against the index, plus untracked. Not fully staged paths.
    Unstaged,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::Uncommitted => "uncommitted",
            Scope::Branch => "branch",
            Scope::LastTurn => "last turn",
            Scope::Commits => "commits",
            Scope::Unstaged => "unstaged",
        }
    }

    /// The kebab-case name config values use (`default_scope`).
    pub fn name(self) -> &'static str {
        match self {
            Scope::Uncommitted => "uncommitted",
            Scope::Branch => "branch",
            Scope::LastTurn => "last-turn",
            Scope::Commits => "commits",
            Scope::Unstaged => "unstaged",
        }
    }
}

/// The old end of a file diff: a Git revision, or the index (`git diff` with no revision).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffBase {
    Index,
    Revision(String),
}

impl From<&str> for DiffBase {
    fn from(s: &str) -> Self {
        Self::Revision(s.to_string())
    }
}

impl From<String> for DiffBase {
    fn from(s: String) -> Self {
        Self::Revision(s)
    }
}

impl From<&String> for DiffBase {
    fn from(s: &String) -> Self {
        Self::Revision(s.clone())
    }
}

/// The `commits` pick: the run `oldest..=newest`, full ids, equal for a run of one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CommitPick {
    pub oldest: String,
    pub newest: String,
}

impl CommitPick {
    pub fn single(sha: &str) -> Self {
        Self { oldest: sha.to_string(), newest: sha.to_string() }
    }

    pub fn is_single(&self) -> bool {
        self.oldest == self.newest
    }
}

/// Where a comment's diff was read; it renders only while the scope reads that same diff.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Rev {
    Worktree,
    Commit(CommitPick),
}

/// How a file changed within a scope.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Untracked,
}

impl ChangeKind {
    pub fn marker(self) -> char {
        match self {
            ChangeKind::Added => 'A',
            ChangeKind::Modified => 'M',
            ChangeKind::Deleted => 'D',
            ChangeKind::Renamed => 'R',
            ChangeKind::Copied => 'C',
            ChangeKind::Untracked => '?',
        }
    }
}

/// A row in the Changes list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChangedFile {
    pub path: String,
    pub kind: ChangeKind,
    pub additions: u32,
    pub deletions: u32,
    /// The source path of a renamed or copied file, whose old content lives there.
    pub previous_path: Option<String>,
    /// git's no-text-diff verdict, read as the binary notice.
    pub binary: bool,
    /// The bytes git stores on the old side, 0 where there is none.
    pub old_size: u64,
    /// The bytes git stores on the new side, 0 where there is none; `None` for the worktree.
    pub new_size: Option<u64>,
}

impl ChangedFile {
    /// Whether `paths` cover this file or the source it was renamed from.
    pub fn touched_by<'a>(&self, paths: impl IntoIterator<Item = &'a String> + Clone) -> bool {
        crate::git::covered(paths.clone(), &self.path)
            || self.previous_path.as_deref().is_some_and(|p| crate::git::covered(paths, p))
    }
}

/// Which side of the diff a comment's lines live on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    New,
    Old,
}

/// What the reviewer submitted: a written note, or a one-click tag.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CommentKind {
    #[default]
    Comment,
    LooksGood,
    Delete,
}

impl CommentKind {
    /// Paint order when several kinds cover one cell: Delete > Looks good > Comment.
    #[must_use]
    pub fn paint_rank(self) -> u8 {
        match self {
            CommentKind::Delete => 3,
            CommentKind::LooksGood => 2,
            CommentKind::Comment => 1,
        }
    }
}

/// A reviewer comment anchored to a run of diff lines, carrying the snippet.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Comment {
    pub file: String,
    pub side: Side,
    pub start: u32,
    pub end: u32,
    /// 1-based inclusive start column when the comment is a character span.
    pub start_col: Option<u32>,
    /// 1-based exclusive end column when the comment is a character span.
    pub end_col: Option<u32>,
    /// Verbatim diff lines the comment anchors to, each keeping its `+`/`-`/space marker.
    /// A character span stores the selected excerpt instead, with no markers.
    pub lines: String,
    pub text: String,
    /// A written note, or a toolbar tag with empty body.
    pub kind: CommentKind,
    /// `false` until a successful send; copy never flips this.
    pub sent: bool,
    /// A diff comment (`Changes`), else a content comment (`All files`).
    pub diff_anchored: bool,
    /// Where the new side was read.
    pub rev: Rev,
}

impl Comment {
    /// The `path:start-end` (or `path:line`) location, with ` (removed)` when old-side.
    pub fn location(&self) -> String {
        let range = if self.start == self.end {
            format!("{}:{}", self.file, self.start)
        } else {
            format!("{}:{}-{}", self.file, self.start, self.end)
        };
        match self.side {
            Side::New => range,
            Side::Old => format!("{range} (removed)"),
        }
    }

    /// Whether this comment is a character span (划词), not a whole-line card.
    #[must_use]
    pub fn is_span(&self) -> bool {
        self.start_col.is_some() && self.end_col.is_some()
    }

    /// The Kind decoration on 1-based `col` of `line_no`, if this comment paints that cell.
    /// A whole-line Comment leaves no mark (the card is enough).
    #[must_use]
    pub fn kind_mark(&self, line_no: u32, col: u32) -> Option<CommentKind> {
        if line_no < self.start || line_no > self.end {
            return None;
        }
        if self.is_span() {
            return self.span_covers(line_no, col).then_some(self.kind);
        }
        match self.kind {
            CommentKind::Comment => None,
            other => Some(other),
        }
    }

    fn span_covers(&self, line_no: u32, col: u32) -> bool {
        let (start_col, end_col) = match (self.start_col, self.end_col) {
            (Some(s), Some(e)) => (s, e),
            _ => return false,
        };
        if self.start == self.end {
            return col >= start_col && col < end_col;
        }
        if line_no == self.start {
            return col >= start_col;
        }
        if line_no == self.end {
            return col < end_col;
        }
        true
    }

    /// Card and list body: typed text, or the tag when the body is empty.
    pub fn card_text(&self) -> &str {
        if !self.text.is_empty() {
            return &self.text;
        }
        match self.kind {
            CommentKind::LooksGood => "Looks good",
            CommentKind::Delete => "Remove this",
            CommentKind::Comment => "",
        }
    }
}

/// The strongest Kind mark among `comments` on this cell.
#[must_use]
pub fn strongest_kind_mark<'a>(
    comments: impl IntoIterator<Item = &'a Comment>,
    line_no: u32,
    col: u32,
) -> Option<CommentKind> {
    comments.into_iter().filter_map(|c| c.kind_mark(line_no, col)).max_by_key(|k| k.paint_rank())
}

impl Default for Comment {
    fn default() -> Self {
        Self {
            file: String::new(),
            side: Side::New,
            start: 1,
            end: 1,
            start_col: None,
            end_col: None,
            lines: String::new(),
            text: String::new(),
            kind: CommentKind::Comment,
            sent: false,
            diff_anchored: true,
            rev: Rev::Worktree,
        }
    }
}

/// The session's comments, each with an id never reused, so a reference never lands on another.
#[derive(Default, Debug)]
pub struct CommentStore {
    items: Vec<Comment>,
    ids: Vec<u64>,
    next_id: u64,
}

impl CommentStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Comment> {
        self.items.iter()
    }

    pub fn get(&self, index: usize) -> Option<&Comment> {
        self.items.get(index)
    }

    /// Append a comment; returns its index.
    pub fn add(&mut self, comment: Comment) -> usize {
        self.items.push(comment);
        self.ids.push(self.next_id);
        self.next_id += 1;
        self.items.len() - 1
    }

    /// The id of the comment at `index`.
    pub fn id(&self, index: usize) -> Option<u64> {
        self.ids.get(index).copied()
    }

    /// The index of the comment with id `id`, while the store holds it.
    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.ids.iter().position(|&i| i == id)
    }

    /// Replace the text of the comment at `index`. Returns `false` if out of range.
    /// An edit is a new note: it drops `sent` and becomes a written `Comment`.
    pub fn edit(&mut self, index: usize, text: String) -> bool {
        if let Some(c) = self.items.get_mut(index) {
            c.text = text;
            c.sent = false;
            c.kind = CommentKind::Comment;
            true
        } else {
            false
        }
    }

    /// Comments that a send still owes the agent.
    pub fn pending_count(&self) -> usize {
        self.items.iter().filter(|c| !c.sent).count()
    }

    /// Mark every pending comment sent, after a successful agent export.
    pub fn mark_pending_sent(&mut self) {
        for c in &mut self.items {
            c.sent = true;
        }
    }

    /// Remove and return the comment at `index` (delete, or consume one on export).
    pub fn take(&mut self, index: usize) -> Option<Comment> {
        if index < self.items.len() {
            self.ids.remove(index);
            Some(self.items.remove(index))
        } else {
            None
        }
    }

    /// Remove and return every comment (consume-all on a successful export).
    pub fn take_all(&mut self) -> Vec<Comment> {
        self.ids.clear();
        std::mem::take(&mut self.items)
    }
}

#[cfg(test)]
mod tests {
    use super::{Comment, CommentStore, Scope, Side};

    fn comment(file: &str, start: u32, end: u32, text: &str) -> Comment {
        Comment {
            file: file.into(),
            start,
            end,
            lines: "+x".into(),
            text: text.into(),
            ..Comment::default()
        }
    }

    #[test]
    fn scope_labels_and_config_names() {
        assert_eq!(Scope::Uncommitted.label(), "uncommitted");
        assert_eq!(Scope::LastTurn.label(), "last turn");
        assert_eq!(Scope::Commits.label(), "commits");
        assert_eq!(Scope::Unstaged.label(), "unstaged");
        assert_eq!(Scope::Commits.name(), "commits");
        assert_eq!(Scope::LastTurn.name(), "last-turn");
        assert_eq!(Scope::Unstaged.name(), "unstaged");
    }

    #[test]
    fn a_pick_of_one_is_single() {
        let pick = super::CommitPick::single("abc");
        assert!(pick.is_single());
        assert!(!super::CommitPick { oldest: "a".into(), newest: "b".into() }.is_single());
    }

    #[test]
    fn location_formats_range_single_and_removed() {
        let mut c = comment("a.rs", 40, 52, "x");
        assert_eq!(c.location(), "a.rs:40-52");
        c.end = 40;
        assert_eq!(c.location(), "a.rs:40");
        c.side = Side::Old;
        assert_eq!(c.location(), "a.rs:40 (removed)");
    }

    #[test]
    fn add_get_edit() {
        let mut s = CommentStore::new();
        let i = s.add(comment("a.rs", 1, 1, "first"));
        assert_eq!(s.len(), 1);
        assert_eq!(s.get(i).unwrap().text, "first");
        assert!(s.edit(i, "second".into()));
        assert_eq!(s.get(i).unwrap().text, "second");
        assert!(!s.edit(99, "nope".into()));
    }

    #[test]
    fn take_one_and_take_all_consume() {
        let mut s = CommentStore::new();
        s.add(comment("a.rs", 1, 1, "one"));
        s.add(comment("b.rs", 2, 2, "two"));
        let taken = s.take(0).unwrap();
        assert_eq!(taken.text, "one");
        assert_eq!(s.len(), 1);
        let rest = s.take_all();
        assert_eq!(rest.len(), 1);
        assert!(s.is_empty());
        assert!(s.take(0).is_none());
    }

    #[test]
    fn ids_are_never_reused_and_survive_an_edit() {
        let mut s = CommentStore::new();
        s.add(comment("a.rs", 1, 1, "one"));
        let two = s.add(comment("a.rs", 2, 2, "two"));
        let id = s.id(two).unwrap();
        s.take(0);
        assert_eq!(s.index_of(id), Some(0), "the list shifted, the id followed");
        s.edit(0, "edited".into());
        assert_eq!(s.id(0), Some(id));
        s.take_all();
        let three = s.add(comment("a.rs", 3, 3, "three"));
        assert_ne!(s.id(three), Some(id), "a fresh comment never takes an old id");
        assert_eq!(s.index_of(id), None);
    }

    #[test]
    fn edit_clears_sent_and_becomes_a_written_comment() {
        let mut s = CommentStore::new();
        let mut c = comment("a.rs", 1, 1, "");
        c.kind = super::CommentKind::LooksGood;
        c.sent = true;
        let i = s.add(c);
        assert!(s.edit(i, "changed my mind".into()));
        let got = s.get(i).unwrap();
        assert!(!got.sent);
        assert_eq!(got.kind, super::CommentKind::Comment);
        assert_eq!(got.text, "changed my mind");
    }

    #[test]
    fn mark_pending_sent_leaves_already_sent_comments() {
        let mut s = CommentStore::new();
        let mut sent = comment("a.rs", 1, 1, "old");
        sent.sent = true;
        s.add(sent);
        s.add(comment("a.rs", 2, 2, "new"));
        assert_eq!(s.pending_count(), 1);
        s.mark_pending_sent();
        assert_eq!(s.pending_count(), 0);
        assert!(s.iter().all(|c| c.sent));
    }

    #[test]
    fn a_whole_line_delete_marks_every_column() {
        let mut c = comment("a.rs", 7, 7, "");
        c.kind = super::CommentKind::Delete;
        assert_eq!(c.kind_mark(7, 1), Some(super::CommentKind::Delete));
        assert_eq!(c.kind_mark(7, 40), Some(super::CommentKind::Delete));
        assert_eq!(c.kind_mark(8, 1), None);
    }

    #[test]
    fn a_whole_line_looks_good_marks_every_column() {
        let mut c = comment("a.rs", 7, 7, "");
        c.kind = super::CommentKind::LooksGood;
        assert_eq!(c.kind_mark(7, 1), Some(super::CommentKind::LooksGood));
        assert_eq!(c.kind_mark(7, 40), Some(super::CommentKind::LooksGood));
    }

    #[test]
    fn a_whole_line_comment_leaves_no_cell_mark() {
        let c = comment("a.rs", 7, 7, "note");
        assert_eq!(c.kind_mark(7, 1), None);
    }

    #[test]
    fn a_span_marks_only_its_columns() {
        let mut c = comment("a.rs", 7, 7, "BE");
        c.start_col = Some(1);
        c.end_col = Some(3);
        c.kind = super::CommentKind::LooksGood;
        assert_eq!(c.kind_mark(7, 1), Some(super::CommentKind::LooksGood));
        assert_eq!(c.kind_mark(7, 2), Some(super::CommentKind::LooksGood));
        assert_eq!(c.kind_mark(7, 3), None);
    }

    #[test]
    fn a_multiline_span_covers_the_middle_whole() {
        let mut c = comment("a.rs", 7, 9, "x");
        c.start_col = Some(4);
        c.end_col = Some(2);
        c.kind = super::CommentKind::Delete;
        assert_eq!(c.kind_mark(7, 3), None);
        assert_eq!(c.kind_mark(7, 4), Some(super::CommentKind::Delete));
        assert_eq!(c.kind_mark(8, 1), Some(super::CommentKind::Delete));
        assert_eq!(c.kind_mark(9, 1), Some(super::CommentKind::Delete));
        assert_eq!(c.kind_mark(9, 2), None);
    }

    #[test]
    fn delete_wins_over_looks_good_and_comment() {
        let mut del = comment("a.rs", 7, 7, "");
        del.kind = super::CommentKind::Delete;
        let mut ok = comment("a.rs", 7, 7, "");
        ok.kind = super::CommentKind::LooksGood;
        let mut note = comment("a.rs", 7, 7, "x");
        note.start_col = Some(1);
        note.end_col = Some(3);
        assert_eq!(
            super::strongest_kind_mark([&del, &ok, &note], 7, 1),
            Some(super::CommentKind::Delete)
        );
        assert_eq!(
            super::strongest_kind_mark([&ok, &note], 7, 1),
            Some(super::CommentKind::LooksGood)
        );
        assert_eq!(super::strongest_kind_mark([&note], 7, 1), Some(super::CommentKind::Comment));
    }
}
