//! Comments formatted for the agent, grouped by file; send marks pending sent on `Ok`, copy does not.

use anyhow::Result;

use crate::herdr;
use crate::model::{Comment, CommentKind, Side};

/// Inclusive–inclusive line ranges and exclusive end columns use this dash.
const RANGE: &str = "–";

/// One comment as an `Annotation` block, numbered `1` when exported alone.
pub fn format_comment(comment: &Comment) -> String {
    format_annotation(1, comment)
}

/// Comment text without blank lines, which would read as the separator between comments.
fn normalize_text(text: &str) -> String {
    text.replace('\r', "")
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn lines_clause(comment: &Comment) -> String {
    let span = match (comment.start_col, comment.end_col) {
        (Some(start_col), Some(end_col)) => {
            format!("{}:{start_col}{RANGE}{}:{end_col}", comment.start, comment.end)
        }
        _ if comment.start == comment.end => comment.start.to_string(),
        _ => format!("{}{RANGE}{}", comment.start, comment.end),
    };
    if comment.side == Side::Old { format!("{span}, removed") } else { span }
}

fn excerpt_text(comment: &Comment) -> String {
    if comment.start_col.is_some() {
        return comment.lines.clone();
    }
    comment
        .lines
        .lines()
        .map(|line| match line.chars().next() {
            Some('+' | '-' | ' ') => line.chars().skip(1).collect(),
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_annotation(n: usize, comment: &Comment) -> String {
    let mut out = format!("### Annotation {n} (lines {})", lines_clause(comment));
    match comment.kind {
        CommentKind::LooksGood | CommentKind::Delete => {
            let tag = match comment.kind {
                CommentKind::LooksGood => "Looks good",
                CommentKind::Delete => "Remove this",
                CommentKind::Comment => unreachable!(),
            };
            if comment.start_col.is_some() {
                let excerpt = excerpt_text(comment).replace('"', "\\\"");
                out.push_str(&format!("\n{tag}: \"{excerpt}\""));
            } else {
                out.push_str(&format!("\n{tag}"));
            }
        }
        CommentKind::Comment => {
            if comment.side == Side::Old || comment.start_col.is_some() {
                let excerpt = excerpt_text(comment).replace('"', "\\\"");
                out.push_str(&format!("\nComment on: \"{excerpt}\""));
            }
            let text = normalize_text(&comment.text);
            if !text.is_empty() {
                out.push('\n');
                out.push_str(
                    &text.lines().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n"),
                );
            }
        }
    }
    out
}

/// Many comments: a header, then one `##` file with numbered annotations.
pub fn format_all(comments: &[&Comment]) -> String {
    if comments.is_empty() {
        return String::new();
    }
    let mut sorted = comments.to_vec();
    sorted.sort_by(|a, b| {
        a.file.cmp(&b.file).then(a.start.cmp(&b.start)).then(a.start_col.cmp(&b.start_col))
    });
    let mut out = String::from("审阅意见如下:\n");
    let mut current: Option<&str> = None;
    let mut n = 0usize;
    for c in sorted {
        if current != Some(c.file.as_str()) {
            current = Some(c.file.as_str());
            n = 0;
            out.push_str(&format!("\n## {}\n", c.file));
        }
        n += 1;
        out.push('\n');
        out.push_str(&format_annotation(n, c));
        out.push('\n');
    }
    out.pop();
    out
}

/// A destination comments can be exported to. Export succeeds or errors as a whole.
pub trait ExportTarget {
    fn export(&self, text: &str) -> Result<()>;
    fn label(&self) -> &'static str;
    /// Destination-specific confirmation shown after a successful export.
    fn success_message(&self, count: usize) -> String;
    /// The status after a failed export, `copy` naming the copy key; the cause goes to the log.
    fn failure_message(&self, error: &anyhow::Error, copy: &str) -> String;
    /// Whether a successful export removes the comments from the store.
    /// Send does; clipboard copy does not, so the reviewer can still edit.
    fn consumes_comments(&self) -> bool {
        true
    }
}

/// The status for a failed send to `agent`: the cause first, since a narrow pane keeps only the
/// line's start, then the way out.
pub fn send_failure(error: &herdr::SendError, agent: Option<&str>, copy: &str) -> String {
    use herdr::{HerdrError as H, SendError as S};
    let cause = match error {
        S::AtPrompt(name) => return format!("answer {name}'s prompt first"),
        S::Herdr(H::PaneGone) => format!("{} closed", agent.unwrap_or("the agent")),
        S::NoAgent => "no agent in this workspace".to_string(),
        S::TooLarge => "review too large to send".to_string(),
        S::Herdr(H::Unanswered) => "herdr didn't answer".to_string(),
        S::Herdr(H::Refused(_) | H::Unreadable) => "herdr refused the send".to_string(),
    };
    format!("{cause}, press {copy} to copy")
}

pub(crate) fn counted_comments(count: usize) -> String {
    let noun = if count == 1 { "comment" } else { "comments" };
    format!("{count} {noun}")
}

/// The system clipboard: the first tool on `PATH`, or the Win32 clipboard on Windows.
#[derive(Debug)]
pub struct Clipboard;

impl ExportTarget for Clipboard {
    fn label(&self) -> &'static str {
        "clipboard"
    }

    fn success_message(&self, count: usize) -> String {
        format!("copied {}", counted_comments(count))
    }

    fn failure_message(&self, error: &anyhow::Error, _copy: &str) -> String {
        match clipboard::remedy(error) {
            Some(remedy) => format!("copy failed: {remedy}"),
            None => "copy failed".to_string(),
        }
    }

    fn export(&self, text: &str) -> Result<()> {
        clipboard::write(text)
    }

    fn consumes_comments(&self) -> bool {
        false
    }
}

/// macOS and Linux copy through a clipboard tool.
#[cfg(not(windows))]
mod clipboard {
    use std::io::Write;
    use std::process::Stdio;

    use anyhow::{Context, Result, bail};

    /// Clipboard tools that read stdin, first one on `PATH` wins.
    pub(super) const TOOLS: &[(&str, &[&str])] = &[
        ("pbcopy", &[]),
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];

    /// No clipboard tool on `PATH`, the one copy failure the reviewer can fix.
    #[derive(Debug)]
    pub(super) struct NoTool;

    impl std::fmt::Display for NoTool {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "no clipboard tool found (wl-clipboard, xclip, or xsel)")
        }
    }

    impl std::error::Error for NoTool {}

    pub(super) fn remedy(error: &anyhow::Error) -> Option<&'static str> {
        error.is::<NoTool>().then_some("install wl-clipboard, xclip, or xsel")
    }

    /// Pipe `text` into the first tool on `PATH`, succeeding only when it exits clean.
    pub(super) fn write(text: &str) -> Result<()> {
        let (cmd, args) = select_tool(TOOLS, crate::proc::on_path).ok_or(NoTool)?;
        let mut child = crate::proc::command(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawning {cmd}"))?;
        child
            .stdin
            .as_mut()
            .with_context(|| format!("{cmd} stdin unavailable"))?
            .write_all(text.as_bytes())
            .with_context(|| format!("writing to {cmd}"))?;
        if !child.wait().with_context(|| format!("waiting for {cmd}"))?.success() {
            bail!("{cmd} exited non-zero");
        }
        Ok(())
    }

    /// The first clipboard tool the `present` predicate accepts, preserving list order.
    pub(super) fn select_tool(
        tools: &'static [(&'static str, &'static [&'static str])],
        present: impl Fn(&str) -> bool,
    ) -> Option<(&'static str, &'static [&'static str])> {
        tools.iter().copied().find(|(cmd, _)| present(cmd))
    }
}

/// The Win32 clipboard as Unicode text; `clip.exe` would mangle non-ASCII.
#[cfg(windows)]
mod clipboard {
    use anyhow::{Context, Result};

    /// Nothing to install, so no failure here has a remedy to name.
    pub(super) fn remedy(_error: &anyhow::Error) -> Option<&'static str> {
        None
    }

    /// Line breaks as CRLF, the Windows clipboard's own convention.
    pub(super) fn write(text: &str) -> Result<()> {
        let text = crate::text::crlf_line_breaks(text);
        arboard::Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_text(text))
            .context("writing the Windows clipboard")
    }
}

/// One chosen agent pane: fill its input in one socket request, then focus it.
#[derive(Clone, Debug)]
pub struct Agent {
    pub pane: String,
    pub name: String,
}

impl ExportTarget for Agent {
    fn label(&self) -> &'static str {
        "agent"
    }

    /// Names the agent: the reviewer's only record of where the review went.
    fn success_message(&self, count: usize) -> String {
        format!("sent {} to {}", counted_comments(count), self.name)
    }

    /// Every send failure is a [`herdr::SendError`]; anything else only says the send failed.
    fn failure_message(&self, error: &anyhow::Error, copy: &str) -> String {
        error.downcast_ref().map_or_else(
            || format!("send failed, press {copy} to copy"),
            |error| send_failure(error, Some(&self.name), copy),
        )
    }

    /// An agent at a prompt refuses: the picker's rows can be minutes old.
    fn export(&self, text: &str) -> Result<()> {
        herdr::send_text(&self.pane, text)?;
        // A focus failure must not fail a delivered export.
        let _ = herdr::focus(&self.pane);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Agent, Clipboard, ExportTarget, format_all, format_comment, send_failure};
    use crate::model::{Comment, CommentKind, Side};

    #[cfg(not(windows))]
    #[test]
    fn clipboard_tool_selection_prefers_list_order_and_can_be_empty() {
        use super::clipboard::{TOOLS, select_tool};
        // None present -> no tool (the caller surfaces the "install one" error).
        assert!(select_tool(TOOLS, |_| false).is_none());
        // Only an X11 tool present -> it's chosen, with its selection args.
        assert_eq!(
            select_tool(TOOLS, |c| c == "xclip"),
            Some(("xclip", &["-selection", "clipboard"][..]))
        );
        // When several are present, earlier in the list wins (pbcopy over xclip).
        assert_eq!(
            select_tool(TOOLS, |c| c == "pbcopy" || c == "xclip").map(|(cmd, _)| cmd),
            Some("pbcopy")
        );
    }

    #[test]
    fn export_confirmations_name_the_actual_result_and_pluralize_comments() {
        // The agent line names the pane it addressed.
        let agent = Agent { pane: "w8:p1".into(), name: "release-bot".into() };
        assert_eq!(agent.success_message(1), "sent 1 comment to release-bot");
        assert_eq!(agent.success_message(2), "sent 2 comments to release-bot");
        assert_eq!(Clipboard.success_message(1), "copied 1 comment");
        assert_eq!(Clipboard.success_message(2), "copied 2 comments");
    }

    #[test]
    fn a_failed_send_or_copy_says_what_to_do() {
        use crate::herdr::{HerdrError as H, SendError as S};
        let agent = Agent { pane: "w8:p1".into(), name: "release-bot".into() };
        let rows = [
            (S::Herdr(H::PaneGone), "release-bot closed, press y to copy"),
            (S::AtPrompt("codex".into()), "answer codex's prompt first"),
            (S::NoAgent, "no agent in this workspace, press y to copy"),
            (S::TooLarge, "review too large to send, press y to copy"),
            (S::Herdr(H::Unanswered), "herdr didn't answer, press y to copy"),
            // Any other refusal never claims the agent closed.
            (
                S::Herdr(H::Refused(Some("internal".into()))),
                "herdr refused the send, press y to copy",
            ),
            (S::Herdr(H::Refused(None)), "herdr refused the send, press y to copy"),
            (S::Herdr(H::Unreadable), "herdr refused the send, press y to copy"),
        ];
        for (error, line) in rows {
            let error = anyhow::Error::from(error);
            assert_eq!(agent.failure_message(&error, "y"), line, "{error}");
        }
        // Before any agent is chosen, a gone pane names no one.
        let gone = S::Herdr(H::PaneGone);
        assert_eq!(send_failure(&gone, None, "y"), "the agent closed, press y to copy");
        #[cfg(not(windows))]
        {
            let missing = anyhow::Error::from(super::clipboard::NoTool);
            assert_eq!(
                Clipboard.failure_message(&missing, "y"),
                "copy failed: install wl-clipboard, xclip, or xsel"
            );
            assert_eq!(
                Clipboard.failure_message(&anyhow::anyhow!("pbcopy exited non-zero"), "y"),
                "copy failed"
            );
        }
        // Windows has no tool to install, so a failed write never points at one.
        #[cfg(windows)]
        {
            let busy = anyhow::Error::from(arboard::Error::ClipboardOccupied)
                .context("writing the Windows clipboard");
            assert_eq!(Clipboard.failure_message(&busy, "y"), "copy failed");
        }
    }

    fn comment(file: &str, side: Side, start: u32, end: u32, lines: &str, text: &str) -> Comment {
        Comment {
            file: file.into(),
            side,
            start,
            end,
            lines: lines.into(),
            text: text.into(),
            ..Comment::default()
        }
    }

    #[test]
    fn a_whole_line_range_skips_the_excerpt() {
        let c = comment(
            "extruct/core/llm_registry.py",
            Side::New,
            40,
            41,
            "-from .z import w\n+from .x import y",
            "this import path looks wrong",
        );
        assert_eq!(
            format_comment(&c),
            "### Annotation 1 (lines 40–41)\n> this import path looks wrong"
        );
    }

    #[test]
    fn removed_side_quotes_the_deleted_text() {
        let c = comment("a.rs", Side::Old, 38, 38, "-    cleanup()", "still needed");
        assert_eq!(
            format_comment(&c),
            "### Annotation 1 (lines 38, removed)\nComment on: \"    cleanup()\"\n> still needed"
        );
    }

    #[test]
    fn a_character_span_uses_line_col_and_excerpt() {
        let mut c =
            comment("feat.feature", Side::New, 12, 12, "review-fix-loop", "拆到三个 Feature");
        c.start_col = Some(21);
        c.end_col = Some(36);
        assert_eq!(
            format_comment(&c),
            "### Annotation 1 (lines 12:21–12:36)\nComment on: \"review-fix-loop\"\n> 拆到三个 Feature"
        );
    }

    #[test]
    fn multiline_text_keeps_breaks_but_drops_blank_lines() {
        let c = comment("a.rs", Side::New, 1, 1, "+x", "first line\n\n  \nsecond line\n");
        assert_eq!(format_comment(&c), "### Annotation 1 (lines 1)\n> first line\n> second line");
    }

    #[test]
    fn all_groups_by_file_and_numbers_per_file() {
        let b = comment("b.rs", Side::New, 5, 5, "+x", "two");
        let a2 = comment("a.rs", Side::New, 20, 20, "+y", "later");
        let a1 = comment("a.rs", Side::New, 3, 3, "+z", "earlier");
        let out = format_all(&[&b, &a2, &a1]);
        assert_eq!(
            out,
            "审阅意见如下:\n\n## a.rs\n\n### Annotation 1 (lines 3)\n> earlier\n\n### Annotation 2 (lines 20)\n> later\n\n## b.rs\n\n### Annotation 1 (lines 5)\n> two"
        );
    }

    #[test]
    fn a_looks_good_span_quotes_the_excerpt() {
        let mut c = comment("feat.feature", Side::New, 77, 77, "圈中的字", "");
        c.start_col = Some(50);
        c.end_col = Some(93);
        c.kind = CommentKind::LooksGood;
        assert_eq!(
            format_comment(&c),
            "### Annotation 1 (lines 77:50–77:93)\nLooks good: \"圈中的字\""
        );
    }

    #[test]
    fn a_whole_line_looks_good_has_no_colon_or_excerpt() {
        let mut c = comment("a.rs", Side::New, 76, 76, "+long line of code", "");
        c.kind = CommentKind::LooksGood;
        assert_eq!(format_comment(&c), "### Annotation 1 (lines 76)\nLooks good");
    }

    #[test]
    fn a_delete_span_quotes_the_excerpt() {
        let mut c = comment("a.rs", Side::Old, 77, 77, "无 Feature 句式规范", "");
        c.start_col = Some(13);
        c.end_col = Some(27);
        c.kind = CommentKind::Delete;
        assert_eq!(
            format_comment(&c),
            "### Annotation 1 (lines 77:13–77:27, removed)\nRemove this: \"无 Feature 句式规范\""
        );
    }

    #[test]
    fn a_whole_line_delete_has_no_colon_or_excerpt() {
        let mut c = comment("a.rs", Side::Old, 38, 38, "-    cleanup()", "");
        c.kind = CommentKind::Delete;
        assert_eq!(format_comment(&c), "### Annotation 1 (lines 38, removed)\nRemove this");
    }
}
