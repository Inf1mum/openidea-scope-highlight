//! Scope Highlight: three Find Action commands that paint the innermost
//! enclosing bracket or tag pair at the caret, or dim everything outside it,
//! through the plugin API's `decorations` interface (`highlight` and `muted`
//! styles). Presentation only: text, selection and undo history are untouched.
//!
//! The scope clears when its document changes (a `document-changed`
//! subscription), on the Clear command, and on Escape where the IDE has not
//! already taken the keystroke (the manifest binds Escape to Clear; the user
//! can rebind it in the keymap editor).
//!
//! One decoration set holds every painted file, so split editors of different
//! files keep independent scope state; the API keys sets by path, so two
//! editors of the same file share one scope.

mod scope;

use openidea_plugin::{
    Context, Decoration, Error, Event, Invocation, Plugin, Position, Range, host, register_plugin,
};
use scope::{LineIndex, find_enclosing};

/// The plugin's single editor decoration set.
const SET: &str = "scope";
/// Storage key holding the documents with a painted scope, one absolute path
/// per line. Survives idle reclaim, so an edit that arrives after reclaim
/// still clears the ranges it made stale.
const ACTIVE_KEY: &str = "active";

#[derive(Default)]
pub struct ScopeHighlight;

impl ScopeHighlight {
    /// Paints the enclosing scope of the primary caret: the pair itself with
    /// the `highlight` style, and in `dim` mode everything outside it `muted`.
    fn paint(&self, dim: bool, ctx: &Invocation) -> Result<Option<String>, Error> {
        // Decorations are project-scoped: a path outside any project cannot be
        // painted, so ask for the project up front.
        if ctx.project().is_none() {
            return Err(Error::Failed("Open a project first".into()));
        }
        let editor = ctx.editor().ok_or("Open a text file first")?;
        let doc = editor.document()?;
        let units: Vec<u16> = doc.text.encode_utf16().collect();
        // The primary selection; its start is the caret the scope encloses.
        let selection = editor.selections()?.into_iter().next().unwrap_or_default();
        let index = LineIndex::new(&units);
        let caret = index
            .offset(selection.start.line, selection.start.character)
            .min(units.len());
        // No matched enclosing pair: no action, the set is untouched.
        let Some(pair) = find_enclosing(&units, caret) else {
            return Ok(None);
        };
        let at = |offset: usize| {
            let (line, character) = index.pos(offset);
            Position::new(line, character)
        };
        let items = if dim {
            let mut items = Vec::with_capacity(4);
            if pair.start > 0 {
                items.push(muted(Range::new(at(0), at(pair.start))));
            }
            if pair.end < units.len() {
                items.push(muted(Range::new(at(pair.end), at(units.len()))));
            }
            items.push(highlight(Range::new(at(pair.start), at(pair.opener_end))));
            items.push(highlight(Range::new(at(pair.closer_start), at(pair.end))));
            items
        } else {
            vec![highlight(Range::new(at(pair.start), at(pair.end)))]
        };
        let path = editor.path();
        host::set_decorations(&path, SET, &items)?;
        mark_active(&path);
        Ok(None)
    }
}

fn highlight(range: Range) -> Decoration {
    Decoration {
        range,
        style: "highlight".into(),
        hover: None,
        after: None,
    }
}

fn muted(range: Range) -> Decoration {
    Decoration {
        range,
        style: "muted".into(),
        hover: None,
        after: None,
    }
}

/// Records `path` (absolute) among the documents with a painted scope.
fn mark_active(path: &str) {
    let mut paths = active_paths();
    if !paths.iter().any(|p| p == path) {
        paths.push(path.to_owned());
        let _ = host::storage::set(ACTIVE_KEY, &paths.join("\n"));
    }
}

/// Drops every record of painted scopes.
fn forget_active() {
    let _ = host::storage::delete(ACTIVE_KEY);
}

/// The recorded documents, absolute paths.
fn active_paths() -> Vec<String> {
    host::storage::get(ACTIVE_KEY)
        .ok()
        .flatten()
        .map(|list| {
            list.lines()
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `event_path` names the document recorded at `stored` (an absolute
/// path). The event may name it project-relative, with either separator.
fn same_document(stored: &str, event_path: &str) -> bool {
    let stored = stored.replace('\\', "/");
    let event = event_path.replace('\\', "/");
    stored == event
        || (stored.len() > event.len()
            && stored.ends_with(&event)
            && stored.as_bytes()[stored.len() - event.len() - 1] == b'/')
}

/// Forgets one painted document.
fn forget_path(path: &str) {
    let paths: Vec<String> = active_paths().into_iter().filter(|p| p != path).collect();
    if paths.is_empty() {
        forget_active();
    } else {
        let _ = host::storage::set(ACTIVE_KEY, &paths.join("\n"));
    }
}

impl Plugin for ScopeHighlight {
    fn activate(&mut self, _ctx: &Context) -> Result<(), Error> {
        // Every committed edit to any project file is delivered here, so a
        // document the scope was painted on clears it before it goes stale.
        host::subscribe_paths("document-changed", &["**"])?;
        Ok(())
    }

    fn run_command(
        &mut self,
        id: &str,
        ctx: &Invocation,
        _arg: Option<&str>,
    ) -> Result<Option<String>, Error> {
        match id {
            "highlight" => self.paint(false, ctx),
            "dim" => self.paint(true, ctx),
            "clear" => {
                host::clear_decorations(SET)?;
                forget_active();
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn handle_events(&mut self, events: &[Event], _ctx: &Invocation) {
        for event in events {
            if let Event::DocumentChanged(change) = event
                && let Some(stored) = active_paths()
                    .into_iter()
                    .find(|p| same_document(p, &change.path))
            {
                // Edits clear stale ranges: the file's own set empties.
                let _ = host::set_decorations(&stored, SET, &[]);
                forget_path(&stored);
            }
        }
    }
}

register_plugin!(ScopeHighlight);

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{DocumentChanged, DocumentTextChange, mock::MockHost};

    fn set_key(path: &str) -> (String, String) {
        (SET.to_owned(), path.to_owned())
    }

    fn changed(path: &str) -> Event {
        Event::DocumentChanged(DocumentChanged {
            path: path.to_owned(),
            base_version: 1,
            version: 2,
            changes: vec![DocumentTextChange {
                from: 0,
                to: 0,
                insert: "x".into(),
            }],
        })
    }

    #[test]
    fn highlight_paints_the_innermost_pair() {
        let mock = MockHost::install();
        mock.add_project("/project");
        mock.open_editor(
            "/project/main.rs",
            "rust",
            "fn main() {\n    let v = vec![1];\n}\n",
            &[(1, 17, 1, 17)],
        );
        let mut plugin = ScopeHighlight;
        plugin
            .run_command("highlight", &mock.invocation(), None)
            .unwrap();
        let items = &mock.decorations()[&set_key("/project/main.rs")];
        assert_eq!(items.len(), 1);
        // One range over the whole pair: `[` at line 1, column 16 … `]` ends
        // the range at column 19.
        assert_eq!(
            items[0].range.start,
            Position {
                line: 1,
                character: 16
            }
        );
        assert_eq!(
            items[0].range.end,
            Position {
                line: 1,
                character: 19
            }
        );
        assert_eq!(items[0].style, "highlight");
        assert_eq!(items[0].hover, None);
        assert_eq!(items[0].after, None);
        // The text is untouched: presentation only.
        assert_eq!(mock.editor_text(), "fn main() {\n    let v = vec![1];\n}\n");
    }

    #[test]
    fn dim_mutes_outside_and_marks_the_brackets() {
        let mock = MockHost::install();
        mock.add_project("/project");
        mock.open_editor(
            "/project/main.rs",
            "rust",
            "fn main() {\n    let v = 1;\n}\n",
            &[(1, 4, 1, 4)],
        );
        let mut plugin = ScopeHighlight;
        plugin.run_command("dim", &mock.invocation(), None).unwrap();
        let items = &mock.decorations()[&set_key("/project/main.rs")];
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].style, "muted");
        assert_eq!(
            items[0].range.start,
            Position {
                line: 0,
                character: 0
            }
        );
        assert_eq!(
            items[0].range.end,
            Position {
                line: 0,
                character: 10
            },
            "up to the opening brace"
        );
        assert_eq!(items[1].style, "muted");
        assert_eq!(
            items[1].range.start,
            Position {
                line: 2,
                character: 1
            },
            "after the closing brace"
        );
        assert_eq!(
            items[1].range.end,
            Position {
                line: 3,
                character: 0
            }
        );
        assert_eq!(items[2].style, "highlight");
        assert_eq!(
            items[2].range,
            Range {
                start: Position {
                    line: 0,
                    character: 10
                },
                end: Position {
                    line: 0,
                    character: 11
                }
            }
        );
        assert_eq!(items[3].style, "highlight");
        assert_eq!(
            items[3].range,
            Range {
                start: Position {
                    line: 2,
                    character: 0
                },
                end: Position {
                    line: 2,
                    character: 1
                }
            }
        );
        assert_eq!(mock.editor_text(), "fn main() {\n    let v = 1;\n}\n");
    }

    #[test]
    fn dim_at_document_edges_paints_no_empty_ranges() {
        let mock = MockHost::install();
        mock.add_project("/project");
        // The block spans the whole document: no muted ranges exist.
        mock.open_editor("/project/a.txt", "", "(x)", &[(0, 1, 0, 1)]);
        let mut plugin = ScopeHighlight;
        plugin.run_command("dim", &mock.invocation(), None).unwrap();
        let items = &mock.decorations()[&set_key("/project/a.txt")];
        assert_eq!(items.iter().filter(|d| d.style == "muted").count(), 0);
        assert_eq!(items.iter().filter(|d| d.style == "highlight").count(), 2);
        assert!(
            items.iter().all(|d| d.range.start != d.range.end),
            "no empty ranges"
        );
    }

    #[test]
    fn no_enclosing_pair_leaves_decorations_alone() {
        let mock = MockHost::install();
        mock.add_project("/project");
        mock.open_editor("/project/a.txt", "", "plain text", &[(0, 5, 0, 5)]);
        let mut plugin = ScopeHighlight;
        plugin
            .run_command("highlight", &mock.invocation(), None)
            .unwrap();
        plugin.run_command("dim", &mock.invocation(), None).unwrap();
        assert!(mock.decorations().is_empty());
    }

    #[test]
    fn tag_scope_in_an_html_file() {
        let mock = MockHost::install();
        mock.add_project("/project");
        mock.open_editor(
            "/project/index.html",
            "html",
            "<div>\n  <p>hi</p>\n</div>\n",
            &[(1, 5, 1, 5)],
        );
        let mut plugin = ScopeHighlight;
        plugin
            .run_command("highlight", &mock.invocation(), None)
            .unwrap();
        let items = &mock.decorations()[&set_key("/project/index.html")];
        // The <p> … </p> pair: `<` at line 1, column 2 … the `>` of `</p>` ends
        // the range at column 11.
        assert_eq!(
            items[0].range.start,
            Position {
                line: 1,
                character: 2
            }
        );
        assert_eq!(
            items[0].range.end,
            Position {
                line: 1,
                character: 11
            }
        );
    }

    #[test]
    fn clear_removes_every_scope() {
        let mock = MockHost::install();
        mock.add_project("/project");
        mock.open_editor("/project/a.txt", "", "(x)", &[(0, 1, 0, 1)]);
        let mut plugin = ScopeHighlight;
        plugin
            .run_command("highlight", &mock.invocation(), None)
            .unwrap();
        assert_eq!(mock.decorations().len(), 1);
        plugin
            .run_command("clear", &mock.invocation(), None)
            .unwrap();
        assert!(mock.decorations().is_empty());
        assert!(!mock.storage().contains_key(ACTIVE_KEY));
    }

    #[test]
    fn an_edit_to_the_painted_document_clears_it() {
        let mock = MockHost::install();
        mock.add_project("/project");
        mock.open_editor("/project/a.txt", "", "(x)", &[(0, 1, 0, 1)]);
        let mut plugin = ScopeHighlight;
        plugin.activate(&Context::default()).unwrap();
        assert!(mock.path_subscriptions().contains_key("document-changed"));
        plugin
            .run_command("highlight", &mock.invocation(), None)
            .unwrap();
        assert_eq!(mock.decorations().len(), 1);
        // An edit to another document leaves the scope alone.
        plugin.handle_events(&[changed("/project/other.txt")], &mock.invocation());
        assert_eq!(mock.decorations()[&set_key("/project/a.txt")].len(), 1);
        // An edit to the painted document clears it, by absolute path.
        plugin.handle_events(&[changed("/project/a.txt")], &mock.invocation());
        assert!(mock.decorations()[&set_key("/project/a.txt")].is_empty());
        assert!(!mock.storage().contains_key(ACTIVE_KEY));
        // And once more, by project-relative path (what the event may carry).
        plugin
            .run_command("highlight", &mock.invocation(), None)
            .unwrap();
        assert_eq!(mock.decorations()[&set_key("/project/a.txt")].len(), 1);
        plugin.handle_events(&[changed("a.txt")], &mock.invocation());
        assert!(mock.decorations()[&set_key("/project/a.txt")].is_empty());
    }

    #[test]
    fn a_second_file_keeps_its_own_scope() {
        let mock = MockHost::install();
        mock.add_project("/project");
        mock.open_editor("/project/a.txt", "", "(x)", &[(0, 1, 0, 1)]);
        let mut plugin = ScopeHighlight;
        plugin
            .run_command("highlight", &mock.invocation(), None)
            .unwrap();
        mock.open_editor("/project/b.txt", "", "[y]", &[(0, 1, 0, 1)]);
        plugin
            .run_command("highlight", &mock.invocation(), None)
            .unwrap();
        // Both files keep their own scope; the set has two entries.
        assert_eq!(mock.decorations().len(), 2);
        assert!(mock.decorations().contains_key(&set_key("/project/a.txt")));
        assert!(mock.decorations().contains_key(&set_key("/project/b.txt")));
        // An edit to the first clears only the first.
        plugin.handle_events(&[changed("/project/a.txt")], &mock.invocation());
        assert!(mock.decorations()[&set_key("/project/a.txt")].is_empty());
        assert_eq!(mock.decorations()[&set_key("/project/b.txt")].len(), 1);
    }

    #[test]
    fn utf16_columns_survive_an_emoji() {
        let mock = MockHost::install();
        mock.add_project("/project");
        mock.open_editor("/project/a.txt", "", "😀 (x)", &[(0, 4, 0, 4)]);
        let mut plugin = ScopeHighlight;
        plugin
            .run_command("highlight", &mock.invocation(), None)
            .unwrap();
        let items = &mock.decorations()[&set_key("/project/a.txt")];
        // The emoji is two UTF-16 units: `(` is column 3 and `)` ends the
        // range at column 6.
        assert_eq!(
            items[0].range.start,
            Position {
                line: 0,
                character: 3
            }
        );
        assert_eq!(
            items[0].range.end,
            Position {
                line: 0,
                character: 6
            }
        );
    }

    #[test]
    fn without_a_project_the_paint_commands_fail_cleanly() {
        let mock = MockHost::install();
        mock.set_project_root(None);
        mock.open_editor("/a.txt", "", "(x)", &[(0, 1, 0, 1)]);
        let mut plugin = ScopeHighlight;
        assert!(matches!(
            plugin.run_command("highlight", &mock.invocation(), None),
            Err(Error::Failed(_))
        ));
        assert!(matches!(
            plugin.run_command("dim", &mock.invocation(), None),
            Err(Error::Failed(_))
        ));
        // Clearing a set is global: it works without a project and clears nothing.
        plugin
            .run_command("clear", &mock.invocation(), None)
            .unwrap();
        assert!(mock.decorations().is_empty());
    }

    #[test]
    fn paths_match_across_separators_and_prefixes() {
        assert!(same_document("/p/a.txt", "/p/a.txt"));
        assert!(same_document("/p/a.txt", "a.txt"));
        assert!(same_document("C:\\p\\a.txt", "a.txt"));
        assert!(same_document("C:\\p\\a.txt", "p/a.txt"));
        assert!(same_document("/p/a/b.txt", "a/b.txt"));
        assert!(!same_document("/p/a.txt", "b.txt"));
        assert!(!same_document("/p/xa.txt", "a.txt"));
    }
}
