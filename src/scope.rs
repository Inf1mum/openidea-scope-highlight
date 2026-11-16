//! Bounded text-level scan for the innermost enclosing bracket or tag pair.
//!
//! The plugin API has no bracket-pair query, so Scope Highlight scans the
//! document text itself: UTF-16 code units in, a [`Pair`] of offsets out. The
//! scan is deliberately bounded (`MAX_SCAN` in each direction, so a pair whose
//! closer is far beyond any viewport lookahead is still found, while a broken
//! parse cannot make the scan run away). It is text-level: it does not know
//! strings or comments, and a tag candidate only counts when its closer exists,
//! so a `Vec<u32>` is never a tag pair (`</u32>` does not occur).

/// How far the scan reaches in each direction, in UTF-16 code units.
pub const MAX_SCAN: usize = 1_000_000;
/// Total UTF-16 units the forward candidate scans may spend. Normally one
/// scan finds the pair; the budget keeps documents full of unmatched openers
/// (where every candidate scan fails) from turning the scan quadratic.
const FORWARD_BUDGET: usize = 4 * MAX_SCAN;
/// Longest `< … >` span parsed as a tag while scanning.
const TAG_WINDOW: usize = 4096;

const PAREN: u16 = b'(' as u16;
const CLOSE_PAREN: u16 = b')' as u16;
const BRACKET: u16 = b'[' as u16;
const CLOSE_BRACKET: u16 = b']' as u16;
const BRACE: u16 = b'{' as u16;
const CLOSE_BRACE: u16 = b'}' as u16;
const LT: u16 = b'<' as u16;
const GT: u16 = b'>' as u16;
const SLASH: u16 = b'/' as u16;
const NEWLINE: u16 = b'\n' as u16;

/// An enclosing pair, in UTF-16 offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pair {
    /// First unit of the opener (`(`, `[`, `{` or the `<` of a tag).
    pub start: usize,
    /// One past the opener's last unit (a plain bracket: `start + 1`).
    pub opener_end: usize,
    /// First unit of the closer (`)`, `]`, `}` or the `<` of `</name>`).
    pub closer_start: usize,
    /// One past the closer's last unit.
    pub end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Paren,
    Bracket,
    Brace,
    /// A `<name …>` opener or `</name>` closer; the name's offsets.
    Tag(usize, usize),
}

fn is_name_char(u: u16) -> bool {
    (u >= b'a' as u16 && u <= b'z' as u16)
        || (u >= b'A' as u16 && u <= b'Z' as u16)
        || (u >= b'0' as u16 && u <= b'9' as u16)
        || u == b'_' as u16
        || u == b'-' as u16
        || u == b'.' as u16
        || u == b':' as u16
}

fn is_letter(u: u16) -> bool {
    (u >= b'a' as u16 && u <= b'z' as u16) || (u >= b'A' as u16 && u <= b'Z' as u16)
}

fn is_space(u: u16) -> bool {
    u == b' ' as u16 || u == b'\t' as u16 || u == NEWLINE || u == b'\r' as u16
}

/// Whether two tag-name offsets name the same text.
fn same_name(units: &[u16], a: (usize, usize), b: (usize, usize)) -> bool {
    units[a.0..a.1] == units[b.0..b.1]
}

enum TagBack {
    /// Not a tag ending at this `>`.
    None,
    /// `</name …>`: the `<` index and the name's offsets.
    Closer { lt: usize, name: (usize, usize) },
    /// `<name …>`: the `<` index, the name's offsets, and whether it closes itself.
    Opener {
        lt: usize,
        name: (usize, usize),
        self_closing: bool,
    },
}

/// Parses a tag ending at `gt` (a `>`), looking back for its `<`. Only a plain
/// opener or closer with a name and nothing but whitespace after it counts;
/// comments, declarations, fragments (`<>`) and closers with attributes are
/// ordinary text.
fn parse_tag_backwards(units: &[u16], gt: usize, floor: usize) -> TagBack {
    let floor = floor.max(gt.saturating_sub(TAG_WINDOW));
    let mut lt = None;
    let mut j = gt;
    while j > floor {
        j -= 1;
        match units[j] {
            GT => return TagBack::None,
            LT => {
                lt = Some(j);
                break;
            }
            _ => {}
        }
    }
    let Some(lt) = lt else { return TagBack::None };
    let first = units[lt + 1];
    if first == SLASH {
        let ns = lt + 2;
        if ns >= gt {
            return TagBack::None;
        }
        let mut ne = ns;
        while ne < gt && is_name_char(units[ne]) {
            ne += 1;
        }
        if ne == ns || !units[ne..gt].iter().all(|&u| is_space(u)) {
            return TagBack::None;
        }
        TagBack::Closer { lt, name: (ns, ne) }
    } else if is_letter(first) {
        let ns = lt + 1;
        let mut ne = ns + 1;
        while ne < gt && is_name_char(units[ne]) {
            ne += 1;
        }
        TagBack::Opener {
            lt,
            name: (ns, ne),
            self_closing: units[gt - 1] == SLASH,
        }
    } else {
        TagBack::None
    }
}

enum TagFwd {
    /// `</name …>`: one past its `>` and the name's offsets.
    Closer { end: usize, name: (usize, usize) },
    /// `<name …>` that does not close itself: one past its `>` and the name's offsets.
    Opener { end: usize, name: (usize, usize) },
    /// A `<` that starts no nesting tag (`<`, `</`, `<!`, `<?`, `<name … />`,
    /// or no `>` in sight): `end` is where scanning resumes.
    Other { end: usize },
}

fn parse_tag_forward(units: &[u16], lt: usize, limit: usize) -> Option<TagFwd> {
    let cap = (lt + TAG_WINDOW).min(limit);
    let mut gt = lt;
    let mut found = false;
    while gt < cap {
        let u = units[gt];
        if u == LT && gt != lt {
            return None;
        }
        if u == GT {
            found = true;
            break;
        }
        gt += 1;
    }
    if !found {
        return None;
    }
    let end = gt + 1;
    let first = units[lt + 1];
    if first == SLASH {
        let ns = lt + 2;
        if ns >= gt {
            return Some(TagFwd::Other { end });
        }
        let mut ne = ns;
        while ne < gt && is_name_char(units[ne]) {
            ne += 1;
        }
        if ne == ns || !units[ne..gt].iter().all(|&u| is_space(u)) {
            return Some(TagFwd::Other { end });
        }
        Some(TagFwd::Closer {
            end,
            name: (ns, ne),
        })
    } else if is_letter(first) {
        let ns = lt + 1;
        let mut ne = ns + 1;
        while ne < gt && is_name_char(units[ne]) {
            ne += 1;
        }
        if units[gt - 1] == SLASH {
            Some(TagFwd::Other { end })
        } else {
            Some(TagFwd::Opener {
                end,
                name: (ns, ne),
            })
        }
    } else {
        Some(TagFwd::Other { end })
    }
}

fn find_plain_closer(
    units: &[u16],
    from: usize,
    limit: usize,
    budget: &mut usize,
    opener: u16,
    closer: u16,
) -> Option<usize> {
    let cap = from.saturating_add(*budget).min(limit);
    let mut depth = 0usize;
    for (j, &u) in units.iter().enumerate().take(cap).skip(from) {
        *budget -= 1;
        if u == opener {
            depth += 1;
        } else if u == closer {
            if depth == 0 {
                return Some(j);
            }
            depth -= 1;
        }
    }
    None
}

/// Scans forward for `</name>` at nesting depth zero. Returns the closer's
/// first unit and one past its `>`.
fn find_tag_closer(
    units: &[u16],
    from: usize,
    limit: usize,
    budget: &mut usize,
    name: (usize, usize),
) -> Option<(usize, usize)> {
    let cap = from.saturating_add(*budget).min(limit);
    let mut depth = 0usize;
    let mut j = from;
    while j < cap {
        *budget -= 1;
        if units[j] == LT {
            match parse_tag_forward(units, j, limit) {
                Some(TagFwd::Closer { end, name: n }) => {
                    if same_name(units, n, name) {
                        if depth == 0 {
                            return Some((j, end));
                        }
                        depth -= 1;
                    }
                    j = end;
                }
                Some(TagFwd::Opener { end, name: n }) => {
                    if same_name(units, n, name) {
                        depth += 1;
                    }
                    j = end;
                }
                Some(TagFwd::Other { end }) => j = end.max(j + 1),
                None => j += 1,
            }
        } else {
            j += 1;
        }
    }
    None
}

/// Scans backward for the `<name …>` opener matching the `</name>` closer at
/// `from` (the closer's `<`). Returns the opener's `<` and one past its `>`.
/// The mirror of [`find_tag_closer`]: nested same-name closers count up, their
/// openers count back down, and other names, self-closers and plain brackets
/// do not affect the depth.
fn find_tag_opener(
    units: &[u16],
    from: usize,
    limit: usize,
    name: (usize, usize),
) -> Option<(usize, usize)> {
    let mut depth = 0usize;
    let mut i = from;
    while i > limit {
        i -= 1;
        if units[i] == GT {
            match parse_tag_backwards(units, i, limit) {
                TagBack::Closer { lt, name: n } => {
                    if same_name(units, n, name) {
                        depth += 1;
                    }
                    i = lt;
                }
                TagBack::Opener {
                    lt,
                    name: n,
                    self_closing,
                } => {
                    if !self_closing && same_name(units, n, name) {
                        if depth == 0 {
                            return Some((lt, i + 1));
                        }
                        depth -= 1;
                    }
                    i = lt;
                }
                TagBack::None => {}
            }
        }
    }
    None
}

/// Finds the innermost pair enclosing `caret` (a UTF-16 offset into `units`).
///
/// Scanning backwards from the caret, closers stack up and matched openers pop
/// them; an opener that does not close before the caret encloses it. Candidates
/// are tried innermost first, and an opener whose closer is not found within
/// the bound is skipped, so one stray unmatched bracket does not hide the scope
/// around it. A tag token that contains the caret (its `>` is ahead of it) is
/// the candidate pair's own end: an opener pairs forward, a closer pairs
/// backward, so the caret inside `</div>` highlights that div.
pub fn find_enclosing(units: &[u16], caret: usize) -> Option<Pair> {
    let caret = caret.min(units.len());
    let back_limit = caret.saturating_sub(MAX_SCAN);
    let fwd_limit = caret.saturating_add(MAX_SCAN).min(units.len());
    let mut budget = FORWARD_BUDGET;
    let mut stack: Vec<Kind> = Vec::new();
    let mut i = caret;
    while i > back_limit {
        i -= 1;
        match units[i] {
            CLOSE_PAREN => stack.push(Kind::Paren),
            CLOSE_BRACKET => stack.push(Kind::Bracket),
            CLOSE_BRACE => stack.push(Kind::Brace),
            PAREN => {
                if stack.last() == Some(&Kind::Paren) {
                    stack.pop();
                } else if let Some(ci) =
                    find_plain_closer(units, caret, fwd_limit, &mut budget, PAREN, CLOSE_PAREN)
                {
                    return Some(Pair {
                        start: i,
                        opener_end: i + 1,
                        closer_start: ci,
                        end: ci + 1,
                    });
                }
            }
            BRACKET => {
                if stack.last() == Some(&Kind::Bracket) {
                    stack.pop();
                } else if let Some(ci) =
                    find_plain_closer(units, caret, fwd_limit, &mut budget, BRACKET, CLOSE_BRACKET)
                {
                    return Some(Pair {
                        start: i,
                        opener_end: i + 1,
                        closer_start: ci,
                        end: ci + 1,
                    });
                }
            }
            BRACE => {
                if stack.last() == Some(&Kind::Brace) {
                    stack.pop();
                } else if let Some(ci) =
                    find_plain_closer(units, caret, fwd_limit, &mut budget, BRACE, CLOSE_BRACE)
                {
                    return Some(Pair {
                        start: i,
                        opener_end: i + 1,
                        closer_start: ci,
                        end: ci + 1,
                    });
                }
            }
            GT => match parse_tag_backwards(units, i, back_limit) {
                TagBack::Closer { lt, name } => {
                    stack.push(Kind::Tag(name.0, name.1));
                    i = lt;
                }
                TagBack::Opener {
                    lt,
                    name,
                    self_closing,
                } => {
                    if !self_closing {
                        if matches!(stack.last(), Some(&Kind::Tag(ns, ne)) if same_name(units, (ns, ne), name))
                        {
                            stack.pop();
                        } else if let Some((cs, ce)) =
                            find_tag_closer(units, caret, fwd_limit, &mut budget, name)
                        {
                            return Some(Pair {
                                start: lt,
                                opener_end: i + 1,
                                closer_start: cs,
                                end: ce,
                            });
                        }
                    }
                    i = lt;
                }
                TagBack::None => {}
            },
            // A tag whose `>` is still ahead of the caret: the caret sits in
            // the token itself. An opener that already ended before the caret
            // was handled at its `>`; a closer is handled at its `<` below.
            LT => match parse_tag_forward(units, i, fwd_limit) {
                // The caret sits in an opening tag, say on an attribute: the
                // pair is this opener with whichever closer follows the caret.
                Some(TagFwd::Opener { end, name }) if end > caret => {
                    if matches!(stack.last(), Some(&Kind::Tag(ns, ne)) if same_name(units, (ns, ne), name))
                    {
                        stack.pop();
                    } else if let Some((cs, ce)) =
                        find_tag_closer(units, caret, fwd_limit, &mut budget, name)
                    {
                        return Some(Pair {
                            start: i,
                            opener_end: end,
                            closer_start: cs,
                            end: ce,
                        });
                    }
                }
                // The caret sits inside `</name>`: that closer ends the
                // enclosing pair, whose opener lies before it.
                Some(TagFwd::Closer { end, name }) if end > caret => {
                    if let Some((lt, opener_end)) = find_tag_opener(units, i, back_limit, name) {
                        return Some(Pair {
                            start: lt,
                            opener_end,
                            closer_start: i,
                            end,
                        });
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    None
}

/// Maps UTF-16 offsets to zero-based lines and UTF-16 columns, as decorations
/// name their ranges.
pub struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(units: &[u16]) -> Self {
        let mut starts = vec![0usize];
        for (i, &u) in units.iter().enumerate() {
            if u == NEWLINE {
                starts.push(i + 1);
            }
        }
        LineIndex { starts }
    }

    /// The UTF-16 offset of a line and column; unknown lines clamp to the last.
    pub fn offset(&self, line: u32, column: u32) -> usize {
        match self.starts.get(line as usize) {
            Some(&s) => s + column as usize,
            None => self.starts.last().copied().unwrap_or(0) + column as usize,
        }
    }

    /// The line and column of a UTF-16 offset.
    pub fn pos(&self, offset: usize) -> (u32, u32) {
        let line = self.starts.partition_point(|&s| s <= offset) - 1;
        (line as u32, (offset - self.starts[line]) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enclosing(text: &str, caret: usize) -> Option<(usize, usize)> {
        let units: Vec<u16> = text.encode_utf16().collect();
        find_enclosing(&units, caret).map(|p| (p.start, p.end))
    }

    /// The caret sits right before `marker`; returns the span found around it.
    fn span(text: &str, marker: &str) -> Option<(usize, usize)> {
        let at = text.find(marker).expect("marker present");
        enclosing(text, at)
    }

    #[test]
    fn innermost_bracket_pair_wins() {
        let text = "fn main() { let v = vec![1, 2]; }";
        // Caret inside the vec! brackets.
        let at = text.find("1,").unwrap();
        assert_eq!(
            enclosing(text, at),
            Some((text.find('[').unwrap(), text.find(']').unwrap() + 1))
        );
        // Caret inside the block but outside the vec.
        let at = text.find("let").unwrap();
        assert_eq!(
            enclosing(text, at),
            Some((text.find("{").unwrap(), text.rfind('}').unwrap() + 1))
        );
    }

    #[test]
    fn closed_pairs_before_the_caret_are_not_scopes() {
        let text = "fn a() {} fn b() {}";
        let at = text.rfind("fn b").unwrap();
        assert_eq!(enclosing(text, at), None);
    }

    #[test]
    fn unclosed_inner_bracket_falls_back_to_the_outer_pair() {
        let text = "{ [ }";
        assert_eq!(enclosing(text, 3), Some((0, text.len())));
    }

    #[test]
    fn no_pair_means_none() {
        assert_eq!(enclosing("plain text", 5), None);
        assert_eq!(enclosing("", 0), None);
    }

    #[test]
    fn tag_pair_encloses_the_caret() {
        let text = "<div><p>text</p></div>";
        let at = text.find("text").unwrap();
        assert_eq!(
            enclosing(text, at),
            Some((text.find("<p").unwrap(), text.find("</p>").unwrap() + 4))
        );
        // The outer div when the caret is between the tags.
        let at = text.find("<p").unwrap();
        assert_eq!(enclosing(text, at), Some((0, text.len())));
    }

    #[test]
    fn innermost_tag_wins_over_brackets_and_tags_nest() {
        let text = "<ul><li><b>go</b></li></ul>";
        let at = text.find("go").unwrap();
        assert_eq!(
            enclosing(text, at),
            Some((text.find("<b").unwrap(), text.rfind("</b>").unwrap() + 4))
        );
    }

    #[test]
    fn nested_same_name_tags_count_depth() {
        let text = "<div><div>deep</div></div>";
        let at = text.find("deep").unwrap();
        // The inner div: its opener at 5, its closer ends at 20.
        assert_eq!(enclosing(text, at), Some((5, 20)));
    }

    #[test]
    fn tag_with_attributes_matches() {
        let text = "<section id=\"main\"><p>hi</p></section>";
        let at = text.find("hi").unwrap();
        let start = text.find("<p").unwrap();
        let end = text.find("</p>").unwrap() + 4;
        assert_eq!(enclosing(text, at), Some((start, end)));
    }

    #[test]
    fn self_closing_and_void_tags_are_not_scopes() {
        let text = "<div><img src=\"x\" /><span>hi</span></div>";
        let at = text.find("hi").unwrap();
        assert_eq!(
            enclosing(text, at),
            Some((
                text.find("<span").unwrap(),
                text.rfind("</span>").unwrap() + 7
            ))
        );
    }

    #[test]
    fn generics_without_closers_are_not_tags() {
        let text = "let v: Vec<u32> = v; if a < b { return; }";
        let at = text.find("return").unwrap();
        let start = text.find('{').unwrap();
        assert_eq!(enclosing(text, at), Some((start, text.len())));
    }

    #[test]
    fn comparisons_are_not_tags() {
        let text = "if a < b && c > d { work(); }";
        let at = text.find("work").unwrap();
        assert_eq!(
            enclosing(text, at),
            Some((text.find('{').unwrap(), text.len()))
        );
    }

    #[test]
    fn closer_far_away_is_found() {
        // 50k lines of content, caret at the top: the matching closer is far
        // beyond any viewport lookahead but within MAX_SCAN.
        let pad = "x\n".repeat(50_000);
        let text = format!("{{\n{pad}}}");
        let units: Vec<u16> = text.encode_utf16().collect();
        let pair = find_enclosing(&units, 1).expect("pair found");
        assert_eq!(pair.start, 0);
        assert_eq!(pair.end, units.len());
    }

    #[test]
    fn unbalanced_openers_exhaust_the_budget_without_running_away() {
        // 2M units of open braces around the caret: every candidate scan fails,
        // the budget stops the search and the call returns quickly.
        let text = "{".repeat(MAX_SCAN + 1);
        let units: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(find_enclosing(&units, units.len() / 2), None);
    }

    #[test]
    fn utf16_positions_are_offsets_not_chars() {
        // An emoji is two UTF-16 units; offsets must stay in units. `text.find`
        // counts bytes, so the caret is taken from the units themselves: the
        // `x` sits at unit 4.
        let units: Vec<u16> = "😀 (x)".encode_utf16().collect();
        let pair = find_enclosing(&units, units.len() - 2).expect("pair");
        assert_eq!(pair.start, 3);
        assert_eq!(pair.end, units.len());
    }

    #[test]
    fn caret_inside_tag_or_attribute_area() {
        let text = "<div class=\"a\">hi</div>";
        // Caret inside the attribute: the div still encloses it.
        let at = text.find("class").unwrap();
        assert_eq!(enclosing(text, at), Some((0, text.len())));
    }

    #[test]
    fn caret_inside_an_opening_tag_finds_that_pair() {
        // Inside the opener's name and inside its attribute: the span is the
        // scope, not the outer div.
        let text = "<div><span lang=\"en\">x</span></div>";
        for marker in ["span", "lang"] {
            let at = text.find(marker).unwrap();
            assert_eq!(
                enclosing(text, at),
                Some((5, text.rfind("</span>").unwrap() + 7)),
                "caret inside {marker}"
            );
        }
        // A self-closing tag is no pair: the enclosing div is the scope.
        let text = "<div><img src=\"x\" />hi</div>";
        let at = text.find("src").unwrap();
        assert_eq!(enclosing(text, at), Some((0, text.len())));
    }

    #[test]
    fn caret_inside_a_closing_tag_finds_that_pair() {
        // The caret within `</span>`'s name: the span pair encloses it (its
        // opener at column 5), not the outer div (column 0).
        let text = "<div><span>x</span></div>";
        for at in [13, 14, 16, 18] {
            assert_eq!(enclosing(text, at), Some((5, 19)), "caret at {at}");
        }
        // The enclosing-parent control: inside `</div>` the div is the scope.
        for at in [20, 22, 24] {
            assert_eq!(enclosing(text, at), Some((0, 25)), "caret at {at}");
        }
        // The closer may sit lines below its opener.
        let text = "<div>\n  <span>x\n  </span>\n</div>\n";
        let at = text.find("</span>").unwrap() + 4;
        assert_eq!(
            enclosing(text, at),
            Some((
                text.find("<span").unwrap(),
                text.find("</span>").unwrap() + 7
            ))
        );
    }

    #[test]
    fn nested_closing_tags_pick_the_innermost_pair() {
        let text = "<div><span><span>x</span>mid</span></div>";
        // Inside the inner closer's name: the inner span.
        let at = text.find("</span>").unwrap() + 3;
        assert_eq!(enclosing(text, at), Some((11, 25)));
        // Inside the outer closer: the outer span.
        let at = text.find("mid").unwrap() + 4;
        assert_eq!(enclosing(text, at), Some((5, 35)));
        // Inside the div's closer: the div.
        let at = text.rfind("</div>").unwrap() + 2;
        assert_eq!(enclosing(text, at), Some((0, text.len())));
    }

    #[test]
    fn multiline_index_maps_offsets_and_lines() {
        let text = "ab\ncd😀\nef";
        let units: Vec<u16> = text.encode_utf16().collect();
        let index = LineIndex::new(&units);
        assert_eq!(index.pos(0), (0, 0));
        assert_eq!(index.pos(3), (1, 0));
        assert_eq!(index.pos(7), (1, 4), "the newline belongs to its line");
        assert_eq!(index.pos(8), (2, 0));
        assert_eq!(index.pos(units.len()), (2, 2));
        assert_eq!(index.offset(1, 0), 3);
        assert_eq!(index.offset(2, 1), 9);
        // A line past the end clamps to the last line's start.
        assert_eq!(index.offset(99, 0), 8);
    }

    #[test]
    fn caret_at_the_opener_finds_nothing() {
        // A scope encloses the caret: it must start strictly before it.
        assert_eq!(span("(a)", "("), None);
    }
}
