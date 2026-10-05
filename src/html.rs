//! A small HTML reader: parse a page into a node tree, find elements, and
//! render an element's readable text.
//!
//! It serves the web-page sources of `lit read <URL>` (ADR-005), which need
//! three things from a page: `<meta>` values, the text of a few named
//! elements, and the article body as plain text. A browser-grade parser is
//! not needed for that, and this one has no dependencies.
//!
//! Tolerance: end tags close the nearest matching open element and are
//! ignored when nothing matches; a block element closes an open `<p>`;
//! nesting deeper than `MAX_DEPTH` attaches to the deepest open element, so
//! malformed input cannot exhaust the stack of the recursive walkers.
//! Cost: parsing and rendering are linear in the input.

/// One parsed node.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Element(Element),
    Text(String),
}

/// An element with lowercased name and attribute names.
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

/// Elements whose content is raw text, never markup.
const RAW_TEXT: &[&str] = &["script", "style"];

/// Elements that never have content.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr",
];

/// Elements that close an open `<p>` when they start.
const CLOSES_P: &[&str] = &[
    "p", "div", "section", "article", "ul", "ol", "li", "table", "pre", "blockquote", "figure", "h1", "h2", "h3",
    "h4", "h5", "h6", "dl", "hr",
];

/// Elements whose subtree is navigation, code, styling or chrome, not body text.
const SKIPPED: &[&str] = &[
    "script", "style", "noscript", "template", "nav", "svg", "footer", "button", "form", "iframe", "canvas",
    "d-contents", "d-comments", "distill-header", "distill-footer", "d-appendix", "d-bibliography",
];

/// Elements set off from their neighbors by a blank line.
const BLOCKS: &[&str] = &[
    "p", "div", "section", "article", "main", "header", "ul", "ol", "table", "pre", "blockquote", "figure",
    "figcaption", "dl", "hr", "h1", "h2", "h3", "h4", "h5", "h6", "d-title", "d-abstract", "d-article", "d-byline",
];

/// Elements rendered on their own line with no blank line around them.
const LINES: &[&str] = &["li", "tr", "dt", "dd", "br"];

/// Open elements deeper than this attach their content to the deepest one.
const MAX_DEPTH: usize = 256;

impl Element {
    /// The value of attribute `name`, if present.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// True when the `class` attribute lists `class`.
    pub fn has_class(&self, class: &str) -> bool {
        self.attr("class").is_some_and(|c| c.split_whitespace().any(|t| t == class))
    }

    /// Concatenated descendant text with no rendering, whitespace collapsed.
    pub fn text(&self) -> String {
        let mut out = String::new();
        collect_text(&self.children, &mut out);
        collapse_whitespace(&out)
    }
}

fn collect_text(nodes: &[Node], out: &mut String) {
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Element(e) => collect_text(&e.children, out),
        }
    }
}

/// Collapse every whitespace run to one space and trim the ends.
pub fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse `html` into the document's top-level nodes. Comments, doctypes and
/// processing instructions are dropped; entities in text and attribute values
/// are decoded.
pub fn parse(html: &str) -> Vec<Node> {
    let mut stack: Vec<Element> = vec![Element { name: String::new(), attrs: Vec::new(), children: Vec::new() }];
    let bytes = html.as_bytes();
    let mut i = 0;
    let mut text_start = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &html[i..];
        let next = rest.as_bytes().get(1).copied().unwrap_or(0);
        let markup_end = if rest.starts_with("<!--") {
            Some(rest.find("-->").map_or(html.len(), |e| i + e + 3))
        } else if next == b'!' || next == b'?' {
            Some(rest.find('>').map_or(html.len(), |e| i + e + 1))
        } else {
            None
        };
        if let Some(end) = markup_end {
            push_text(&mut stack, &html[text_start..i]);
            i = end;
            text_start = i;
            continue;
        }
        let is_end = next == b'/';
        let name_start = if is_end { 2 } else { 1 };
        if !rest.as_bytes().get(name_start).is_some_and(|b| b.is_ascii_alphabetic()) {
            i += 1;
            continue;
        }
        push_text(&mut stack, &html[text_start..i]);
        let Some((tag, consumed)) = read_tag(&rest[name_start..]) else {
            // An unterminated tag runs to the end of input.
            i = html.len();
            text_start = i;
            break;
        };
        i += name_start + consumed;
        text_start = i;
        if is_end {
            close(&mut stack, &tag.name);
            continue;
        }
        if CLOSES_P.contains(&tag.name.as_str()) && stack.last().is_some_and(|e| e.name == "p") {
            close(&mut stack, "p");
        }
        if tag.name == "li" && stack.last().is_some_and(|e| e.name == "li") {
            close(&mut stack, "li");
        }
        let element = Element { name: tag.name, attrs: tag.attrs, children: Vec::new() };
        if RAW_TEXT.contains(&element.name.as_str()) {
            let closing = format!("</{}", element.name);
            let body_end = find_ascii_ci(&html[i..], &closing).map_or(html.len(), |e| i + e);
            let mut element = element;
            if body_end > i {
                element.children.push(Node::Text(html[i..body_end].to_string()));
            }
            append(&mut stack, Node::Element(element));
            i = html[body_end..].find('>').map_or(html.len(), |e| body_end + e + 1);
            text_start = i;
        } else if tag.self_closing || VOID.contains(&element.name.as_str()) || stack.len() > MAX_DEPTH {
            append(&mut stack, Node::Element(element));
        } else {
            stack.push(element);
        }
    }
    push_text(&mut stack, &html[text_start.min(html.len())..]);
    while stack.len() > 1 {
        let done = stack.pop().unwrap();
        append(&mut stack, Node::Element(done));
    }
    stack.pop().map(|root| root.children).unwrap_or_default()
}

/// A start or end tag's name and attributes.
struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
    self_closing: bool,
}

/// Read a tag starting at its name; returns it and the bytes consumed through
/// the closing `>`, or `None` when no `>` follows.
fn read_tag(s: &str) -> Option<(Tag, usize)> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' && b[i] != b'/' {
        i += 1;
    }
    let name = s[..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    let mut self_closing = false;
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        match b.get(i)? {
            b'>' => return Some((Tag { name, attrs, self_closing }, i + 1)),
            b'/' => {
                self_closing = true;
                i += 1;
                continue;
            }
            _ => {}
        }
        self_closing = false;
        let key_start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        let key = s[key_start..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if b.get(i) == Some(&b'=') {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            match b.get(i)? {
                q @ (b'"' | b'\'') => {
                    let end = s[i + 1..].find(*q as char)? + i + 1;
                    value = decode_entities(&s[i + 1..end]);
                    i = end + 1;
                }
                _ => {
                    let start = i;
                    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                        i += 1;
                    }
                    value = decode_entities(&s[start..i]);
                }
            }
        }
        if !key.is_empty() {
            attrs.push((key, value));
        }
    }
}

/// Case-insensitive ASCII search for `needle` in `hay`.
fn find_ascii_ci(hay: &str, needle: &str) -> Option<usize> {
    let n = needle.len();
    hay.as_bytes().windows(n).position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

fn push_text(stack: &mut [Element], raw: &str) {
    if !raw.is_empty() {
        let top = stack.last_mut().expect("root is never popped while parsing");
        top.children.push(Node::Text(decode_entities(raw)));
    }
}

fn append(stack: &mut [Element], node: Node) {
    stack.last_mut().expect("root is never popped while parsing").children.push(node);
}

/// Close the nearest open element named `name`, and every element opened
/// inside it; an end tag that matches nothing is ignored.
fn close(stack: &mut Vec<Element>, name: &str) {
    let Some(pos) = stack.iter().skip(1).rposition(|e| e.name == name).map(|p| p + 1) else {
        return;
    };
    while stack.len() > pos {
        let done = stack.pop().unwrap();
        append(stack, Node::Element(done));
    }
}

/// Decode character references: the common named ones and every numeric one.
/// An unknown or malformed reference is kept as written.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest[1..]
            .find(';')
            .filter(|&semi| semi <= 10)
            .and_then(|semi| decode_one(&rest[1..1 + semi]).map(|c| (c, semi + 2)));
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn decode_one(entity: &str) -> Option<char> {
    if let Some(num) = entity.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "ndash" => '\u{2013}',
        "mdash" => '\u{2014}',
        "hellip" => '\u{2026}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        "times" => '\u{00d7}',
        "middot" => '\u{00b7}',
        "copy" => '\u{00a9}',
        "dagger" => '\u{2020}',
        "Dagger" => '\u{2021}',
        "lowast" => '\u{2217}',
        _ => return None,
    })
}

/// Depth-first search for the first element satisfying `pred`.
pub fn find<'a>(nodes: &'a [Node], pred: &dyn Fn(&Element) -> bool) -> Option<&'a Element> {
    for node in nodes {
        if let Node::Element(e) = node {
            if pred(e) {
                return Some(e);
            }
            if let Some(found) = find(&e.children, pred) {
                return Some(found);
            }
        }
    }
    None
}

/// Every element satisfying `pred`, in document order. A match's own
/// descendants are searched too.
pub fn find_all<'a>(nodes: &'a [Node], pred: &dyn Fn(&Element) -> bool) -> Vec<&'a Element> {
    let mut out = Vec::new();
    find_all_into(nodes, pred, &mut out);
    out
}

fn find_all_into<'a>(nodes: &'a [Node], pred: &dyn Fn(&Element) -> bool, out: &mut Vec<&'a Element>) {
    for node in nodes {
        if let Node::Element(e) = node {
            if pred(e) {
                out.push(e);
            }
            find_all_into(&e.children, pred, out);
        }
    }
}

/// The `content` of `<meta name="{name}">` (or `property=`), every match in order.
pub fn meta_all(nodes: &[Node], name: &str) -> Vec<String> {
    find_all(nodes, &|e| e.name == "meta" && (e.attr("name") == Some(name) || e.attr("property") == Some(name)))
        .into_iter()
        .filter_map(|e| e.attr("content").map(|c| collapse_whitespace(c)))
        .filter(|c| !c.is_empty())
        .collect()
}

/// Render `nodes` as plain text: headings as `#` lines, list items as `- `
/// lines, one blank line between blocks, `<pre>` verbatim, math as `$TeX$`,
/// footnotes as `[footnote: ...]`, citations as `[key]`. Navigation, scripts,
/// styles, SVG, comments sections and `aria-hidden` subtrees are omitted.
pub fn render_text(nodes: &[Node]) -> String {
    let mut r = Renderer { out: String::new(), prefix: String::new() };
    r.nodes(nodes);
    let mut text = String::new();
    let mut blank = 0;
    for line in r.out.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            blank += 1;
            continue;
        }
        if !text.is_empty() {
            text.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        text.push_str(line);
        blank = 0;
    }
    text
}

struct Renderer {
    out: String,
    /// A line marker (`- `, `## `) waiting for the first text of its line,
    /// so a block nested in a list item or heading cannot strand it.
    prefix: String,
}

impl Renderer {
    fn nodes(&mut self, nodes: &[Node]) {
        for node in nodes {
            match node {
                Node::Text(t) => self.inline(t),
                Node::Element(e) => self.element(e),
            }
        }
    }

    fn at_line_start(&self) -> bool {
        self.out.is_empty() || self.out.ends_with('\n')
    }

    /// Append `s`, preceded by the pending line marker at the start of a line.
    fn emit(&mut self, s: &str) {
        if !self.prefix.is_empty() {
            self.line_break();
            self.out.push_str(&std::mem::take(&mut self.prefix));
        }
        self.out.push_str(s);
    }

    /// Append inline text with whitespace collapsed against what precedes it.
    fn inline(&mut self, t: &str) {
        let space_before = t.starts_with(char::is_whitespace) && !self.at_line_start() && !self.out.ends_with(' ');
        let words: Vec<&str> = t.split_whitespace().collect();
        if words.is_empty() {
            if space_before {
                self.out.push(' ');
            }
            return;
        }
        if space_before {
            self.out.push(' ');
        }
        self.emit(&words.join(" "));
        if t.ends_with(char::is_whitespace) {
            self.out.push(' ');
        }
    }

    /// End the current line, so what follows starts a new one.
    fn line_break(&mut self) {
        let trimmed = self.out.trim_end_matches(' ').len();
        self.out.truncate(trimmed);
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push('\n');
        }
    }

    /// End the current paragraph, so a blank line precedes what follows.
    fn block_break(&mut self) {
        self.line_break();
        if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }

    fn element(&mut self, e: &Element) {
        let name = e.name.as_str();
        if SKIPPED.contains(&name)
            || e.attr("aria-hidden") == Some("true")
            || e.attr("id") == Some("comments")
            || e.has_class("comments")
        {
            return;
        }
        match name {
            "pre" => {
                self.block_break();
                let mut raw = String::new();
                collect_text(&e.children, &mut raw);
                self.emit(raw.trim_matches('\n'));
                self.out.push('\n');
                self.block_break();
                return;
            }
            "d-math" => {
                let delim = if e.attr("block").is_some() { "$$" } else { "$" };
                self.inline(&format!(" {}{}{}", delim, e.text(), delim));
                return;
            }
            "d-cite" => {
                if let Some(key) = e.attr("key").or_else(|| e.attr("bibtex-key")) {
                    self.emit(&format!("[{}]", key));
                }
                return;
            }
            "d-footnote" => {
                self.inline(&format!(" [footnote: {}]", e.text()));
                return;
            }
            _ => {}
        }
        if e.has_class("mjx-math")
            && let Some(tex) = e.attr("aria-label")
        {
            self.inline(&format!(" ${}$", tex));
            return;
        }
        let is_line = LINES.contains(&name);
        let is_block = !is_line && BLOCKS.contains(&name);
        if is_line {
            self.line_break();
        } else if is_block {
            self.block_break();
        }
        if let Some(level) = name.strip_prefix('h').and_then(|l| l.parse::<usize>().ok()).filter(|l| (1..=6).contains(l)) {
            self.prefix = format!("{} ", "#".repeat(level));
        } else if name == "li" {
            self.prefix = "- ".to_string();
        } else if matches!(name, "td" | "th") && !self.at_line_start() {
            self.inline(" | ");
        }
        self.nodes(&e.children);
        if is_line || is_block {
            // A marker whose element had no text marks nothing.
            self.prefix.clear();
        }
        if is_line {
            self.line_break();
        } else if is_block {
            self.block_break();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first<'a>(nodes: &'a [Node], name: &str) -> &'a Element {
        find(nodes, &|e| e.name == name).unwrap_or_else(|| panic!("no <{}>", name))
    }

    #[test]
    fn parses_nested_elements_with_attributes() {
        let doc = parse(r#"<div id="a" class='x y'><p data-k=v>Hi <b>there</b></p></div>"#);
        let div = first(&doc, "div");
        assert_eq!(div.attr("id"), Some("a"));
        assert!(div.has_class("y"));
        assert!(!div.has_class("x y"));
        assert_eq!(first(&doc, "p").attr("data-k"), Some("v"));
        assert_eq!(div.text(), "Hi there");
    }

    #[test]
    fn script_and_style_bodies_are_raw_text_not_markup() {
        let doc = parse("<script>if (a < b) { x = '<p>no</p>'; }</script><p>yes</p>");
        assert_eq!(find_all(&doc, &|e| e.name == "p").len(), 1);
        assert!(first(&doc, "script").text().contains("'<p>no</p>'"));
    }

    #[test]
    fn comments_doctype_and_stray_end_tags_are_dropped() {
        let doc = parse("<!DOCTYPE html><!-- <p>hidden</p> --></span><p>shown</p>");
        assert_eq!(render_text(&doc), "shown");
    }

    #[test]
    fn decodes_named_and_numeric_entities_and_keeps_unknown_ones() {
        assert_eq!(decode_entities("a &amp; b &#8220;q&#x201D; &nbsp;&bogus; & c"), "a & b \u{201c}q\u{201d}  &bogus; & c");
        let doc = parse(r#"<meta name="t" content="A &amp; B">"#);
        assert_eq!(meta_all(&doc, "t"), vec!["A & B"]);
    }

    #[test]
    fn collapse_whitespace_joins_runs_and_trims() {
        assert_eq!(collapse_whitespace("  a \n\t b\u{a0} c  "), "a b c");
        assert_eq!(collapse_whitespace(" \n "), "");
    }

    #[test]
    fn unclosed_paragraphs_close_at_the_next_block() {
        let doc = parse("<p>one<p>two<div>three</div>");
        assert_eq!(render_text(&doc), "one\n\ntwo\n\nthree");
    }

    #[test]
    fn pathological_nesting_does_not_overflow_the_stack() {
        let html = "<span>".repeat(100_000) + "deep";
        assert_eq!(render_text(&parse(&html)), "deep");
    }

    #[test]
    fn unterminated_tag_ends_the_document() {
        assert_eq!(render_text(&parse("<p>ok</p><a href=\"x")), "ok");
    }

    #[test]
    fn render_marks_headings_lists_and_blocks() {
        let doc = parse("<h2>Title</h2><p>Para  one\n continues.</p><ul><li>a</li><li>b</ul>");
        assert_eq!(render_text(&doc), "## Title\n\nPara one continues.\n\n- a\n- b");
    }

    #[test]
    fn render_skips_chrome_and_hidden_subtrees() {
        let doc = parse(
            "<nav>menu</nav><p>body<script>x()</script><style>p{}</style></p>\
             <svg><text>label</text></svg><span aria-hidden=\"true\">dup</span><div id=\"comments\">c</div>",
        );
        assert_eq!(render_text(&doc), "body");
    }

    #[test]
    fn list_marker_survives_a_paragraph_inside_the_item() {
        let doc = parse("<ol><li id=\"fn1\"><p>Footnote text.</p></li><li></li></ol><p>after</p>");
        assert_eq!(render_text(&doc), "- Footnote text.\n\nafter");
    }

    #[test]
    fn render_keeps_pre_verbatim() {
        let doc = parse("<p>see</p><pre>a  b\n    c</pre><p>after</p>");
        assert_eq!(render_text(&doc), "see\n\na  b\n    c\n\nafter");
    }

    #[test]
    fn render_writes_math_citations_and_footnotes_inline() {
        let doc = parse(
            "<p>Let <d-math>W_Q</d-math> act<d-cite key=\"v2017\"></d-cite>.<d-footnote>Note.</d-footnote></p>\
             <p>Set <span class=\"mjx-math\" aria-label=\"D\"><span aria-hidden=\"true\">D</span></span> here.</p>",
        );
        assert_eq!(render_text(&doc), "Let $W_Q$ act[v2017]. [footnote: Note.]\n\nSet $D$ here.");
    }

    #[test]
    fn meta_all_returns_every_value_in_order() {
        let doc = parse(r#"<meta name="a" content="1"><meta property="a" content="2"><meta name="b" content="3">"#);
        assert_eq!(meta_all(&doc, "a"), vec!["1", "2"]);
    }
}
