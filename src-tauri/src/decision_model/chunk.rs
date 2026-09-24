//! Cutting bulk material into its original blocks, and blocks into requests.
//!
//! Everything a find tool scores is a sequence of *positions* — the lines of a file or of a
//! command's output, the entries of a console, the paths of a listing, the elements of a page —
//! and its original blocks: the innermost units the material itself is made of. A language
//! server's innermost symbol, and between symbols the blocks the lines form by indentation; a
//! Markdown section down to its next heading; a paragraph of plain text; a log record with its
//! stack trace; one read of a dev server's output; a console entry; a path; a page element.
//! Blocks are what gets scored and what goes back to the model, always whole: a function is never
//! answered in pieces, and a stack trace never leaves its message.
//!
//! Every block is scored on its own, but several share one request, each with a question of its
//! own — TypeSafe evaluates every question of a request in parallel against the same state. How
//! many a request holds is decided by size alone: the request budget grows with the material, so a
//! short input is spread over a few requests and a long one stays near [`TARGET_REQUESTS`], up to
//! [`MAX_REQUEST_CHARS`], past which a longer input takes more requests. Nothing is ever left out.
//! A block longer than [`MAX_REQUEST_CHARS`] is cut into parts for scoring — along its lines, and a
//! single line longer than that into slices — and scores what its best part scores.
//!
//! Positions are 1-based and inclusive, so a line range can be fed straight back into `read` or
//! `find_content`.

use std::ops::Range;

/// The smallest request budget, however short the input: small enough that a short file is still
/// spread over a few parallel requests.
pub const MIN_REQUEST_CHARS: usize = 1_500;
/// The largest request budget, and the longest text one question is asked about: about 3k
/// tokens. Jev's state limit is 32k tokens, and TypeSafe warns that unrelated material in the
/// state costs accuracy, so a request stops growing well short of it.
pub const MAX_REQUEST_CHARS: usize = 12_000;
/// How many requests the budget aims for. At the shared request rate that is several seconds;
/// inputs large enough to hit [`MAX_REQUEST_CHARS`] take more.
pub const TARGET_REQUESTS: usize = 150;
/// The longest run of one-line blocks that is still one paragraph: prose, a run of imports.
/// A longer run — data, a log, a listing — is a block per line.
pub const MAX_PARAGRAPH_CHARS: usize = 1_500;
/// The most blocks one request asks about. Every question carries the rubric again, so this
/// bounds a request's tokens when its blocks are tiny — single log lines, paths.
pub const MAX_QUESTIONS_PER_REQUEST: usize = 32;

/// The request budget for material of `total` characters.
pub fn request_budget(total: usize) -> usize {
    total
        .div_ceil(TARGET_REQUESTS)
        .clamp(MIN_REQUEST_CHARS, MAX_REQUEST_CHARS)
}

/// Consecutive items of the given sizes packed into requests of at most `budget` characters and
/// [`MAX_QUESTIONS_PER_REQUEST`] items; an item larger than the budget goes alone.
pub fn batches(sizes: &[usize], budget: usize) -> Vec<Range<usize>> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut total = 0;
    for (index, &size) in sizes.iter().enumerate() {
        let full = index > start
            && (total + size > budget || index - start >= MAX_QUESTIONS_PER_REQUEST);
        if full {
            batches.push(start..index);
            start = index;
            total = 0;
        }
        total += size;
    }
    if start < sizes.len() {
        batches.push(start..sizes.len());
    }
    batches
}

/// Material cut into its original blocks: one text per position, and the blocks as position
/// ranges, in order. Positions no block covers — blank lines between blocks — are not scored.
#[derive(Clone, Debug)]
pub struct Blocks {
    texts: Vec<String>,
    spans: Vec<(usize, usize)>,
}

impl Blocks {
    fn new(texts: Vec<String>, spans: Vec<(usize, usize)>) -> Self {
        Self { texts, spans }
    }

    /// Plain text with nothing better to go on: indented blocks — a line with the more deeply
    /// indented lines under it, the line that closes it, and the comment and attribute lines
    /// directly above it — and paragraphs of the one-line ones. Code, configuration and prose
    /// alike. Without a language server a container cannot be told from a long function, so one
    /// guess is made: a block too long for one question whose body holds two or more blocks with
    /// bodies of their own is a container — a `mod`, an `impl`, a class — and is opened.
    pub fn text(text: &str) -> Self {
        let texts = split_lines(text);
        let spans = text_spans(&texts, 1, texts.len());
        Self::new(texts, spans)
    }

    /// Markdown: a section is its heading and everything up to the next heading of any level;
    /// text before the first heading is cut as [`Blocks::text`] cuts text. A `#` inside a fenced
    /// code block is not a heading.
    pub fn markdown(text: &str) -> Self {
        let texts = split_lines(text);
        let levels = heading_levels(&texts);
        let mut spans = Vec::new();
        let first_heading = levels.iter().position(Option::is_some).map(|index| index + 1);
        let preamble_end = first_heading.map_or(texts.len(), |heading| heading - 1);
        spans.extend(text_spans(&texts, 1, preamble_end));
        if let Some(mut start) = first_heading {
            while start <= texts.len() {
                let next = (start + 1..=texts.len())
                    .find(|position| levels[position - 1].is_some())
                    .unwrap_or(texts.len() + 1);
                spans.push((start, trim_blank_tail(&texts, start, next - 1)));
                start = next;
            }
        }
        Self::new(texts, spans)
    }

    /// Source code with a language server's document symbols: `spans` are their line ranges,
    /// 1-based and inclusive in `text`'s own numbering. A symbol with no symbols inside it is a
    /// block, together with the comment and attribute lines directly above it at its indentation;
    /// every other line — imports, a container's own header and closing lines, a function's lines
    /// around a nested one — is cut as [`Blocks::text`] cuts text. The spans are only seams: ones
    /// that fall outside the text or straddle their container are ignored, and every line is
    /// covered either way.
    pub fn symbols(text: &str, spans: &[(usize, usize)]) -> Self {
        let texts = split_lines(text);
        let forest = nest(spans, texts.len());
        let mut out = Vec::new();
        symbol_spans(&texts, 1, texts.len(), &forest, &mut out);
        Self::new(texts, out)
    }

    /// Log output: every record — a line and the lines that continue it, such as an indented stack
    /// trace, a `Caused by:` or the exception line that ends a Python traceback — is a block.
    pub fn log_records(text: &str) -> Self {
        let texts = split_lines(text);
        let spans = record_spans(&texts, 1, texts.len());
        Self::new(texts, spans)
    }

    /// A dev server's output as its pipes delivered it: every read is a block — the output of one
    /// write, as a rule, or of several that arrived together. A read that stops mid-line is joined
    /// to the next one, which finishes the line.
    pub fn reads(reads: &[String]) -> Self {
        fn push(texts: &mut Vec<String>, spans: &mut Vec<(usize, usize)>, read: &str) {
            let first = texts.len() + 1;
            texts.extend(split_lines(read));
            if texts.len() >= first
                && (first..=texts.len()).any(|position| !is_blank(&texts[position - 1]))
            {
                spans.push((first, trim_blank_tail(texts, first, texts.len())));
            }
        }
        let mut texts = Vec::new();
        let mut spans = Vec::new();
        let mut pending = String::new();
        for read in reads {
            pending.push_str(read);
            if pending.ends_with('\n') {
                push(&mut texts, &mut spans, &std::mem::take(&mut pending));
            }
        }
        push(&mut texts, &mut spans, &pending);
        Self::new(texts, spans)
    }

    /// Entries that each stand on their own — console messages, however many lines one spans,
    /// paths, page elements: every entry is a block.
    pub fn entries(entries: Vec<String>) -> Self {
        let spans = (1..=entries.len()).map(|position| (position, position)).collect();
        Self::new(entries, spans)
    }

    /// Positions in the material, in blocks or not.
    pub fn len(&self) -> usize {
        self.texts.len()
    }

    pub fn spans(&self) -> &[(usize, usize)] {
        &self.spans
    }

    /// The positions `first..=last`, one per line of the returned text.
    pub fn text_of(&self, first: usize, last: usize) -> String {
        self.texts[first - 1..last].join("\n")
    }

    pub fn lines(&self, first: usize, last: usize) -> &[String] {
        &self.texts[first - 1..last]
    }

    /// Positions `first..=last` as the decision model is shown them: whole, or — longer than one
    /// question takes — cut into parts of at most [`MAX_REQUEST_CHARS`], along lines, and a single
    /// line longer than that into slices. Each part carries the positions it came from.
    pub fn parts(&self, first: usize, last: usize) -> Vec<(String, usize, usize)> {
        let mut parts: Vec<(String, usize, usize)> = Vec::new();
        let mut current: Option<(String, usize, usize)> = None;
        for position in first..=last {
            let line = &self.texts[position - 1];
            let size = line.chars().count();
            if size > MAX_REQUEST_CHARS {
                parts.extend(current.take());
                let characters = line.chars().collect::<Vec<_>>();
                parts.extend(
                    characters
                        .chunks(MAX_REQUEST_CHARS)
                        .map(|slice| (slice.iter().collect(), position, position)),
                );
                continue;
            }
            current = Some(match current.take() {
                Some((text, start, _)) if text.chars().count() + 1 + size <= MAX_REQUEST_CHARS => {
                    (format!("{text}\n{line}"), start, position)
                }
                Some(full) => {
                    parts.push(full);
                    (line.clone(), position, position)
                }
                None => (line.clone(), position, position),
            });
        }
        parts.extend(current);
        parts
    }
}

/// The lines of `text`, `\r` stripped.
pub fn split_lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

// ----------------------------------------------------------------- Indentation

/// A line with any leading ANSI colour codes skipped, so a coloured continuation line still
/// reads as indented.
fn strip_leading_sgr(mut line: &str) -> &str {
    while let Some(rest) = line.strip_prefix("\u{1b}[") {
        match rest.find(|character: char| character.is_ascii_alphabetic()) {
            Some(end) => line = &rest[end + 1..],
            None => break,
        }
    }
    line
}

/// Leading whitespace width, a tab counting four; `None` for a blank line.
fn indent_of(line: &str) -> Option<usize> {
    let mut width = 0;
    for character in strip_leading_sgr(line).chars() {
        match character {
            ' ' => width += 1,
            '\t' => width += 4,
            _ => return Some(width),
        }
    }
    None
}

fn is_blank(line: &str) -> bool {
    indent_of(line).is_none()
}

/// The last position of `first..=last` that is not blank, or `first` when all are.
fn trim_blank_tail(texts: &[String], first: usize, last: usize) -> usize {
    (first..=last)
        .rev()
        .find(|position| !is_blank(&texts[position - 1]))
        .unwrap_or(first)
}

/// A line that closes the block above it at that block's own indentation: `}`, `)`, `]`, a
/// closing tag, `end`, `fi`, `done`, `esac`.
fn closes_block(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with(['}', ')', ']'])
        || trimmed.starts_with("</")
        || matches!(
            trimmed
                .split(|character: char| !character.is_ascii_alphanumeric())
                .next(),
            Some("end" | "fi" | "done" | "esac")
        )
}

/// A comment, attribute or decorator line, which belongs to what is directly below it.
fn is_annotation(line: &str) -> bool {
    let trimmed = line.trim_start();
    ["//", "/*", "*", "#", "--", "@", "\"\"\"", "'''", "<!--", ";;", "%"]
        .iter()
        .any(|marker| trimmed.starts_with(marker))
}

/// The top-level blocks of positions `first..=last`: a line together with the more deeply
/// indented lines under it and the line that closes it, and the comment and attribute lines
/// directly above it at its indentation. Blank lines between blocks belong to none.
fn block_spans(texts: &[String], first: usize, last: usize) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut index = first;
    while index <= last {
        let Some(base) = indent_of(&texts[index - 1]) else {
            index += 1;
            continue;
        };
        let lead = index;
        while index < last
            && is_annotation(&texts[index - 1])
            && indent_of(&texts[index]) == Some(base)
        {
            index += 1;
        }
        let mut end = index;
        for next in index + 1..=last {
            let line = &texts[next - 1];
            match indent_of(line) {
                None => {}
                Some(indent) if indent > base || (indent == base && closes_block(line)) => {
                    end = next
                }
                Some(_) => break,
            }
        }
        spans.push((lead, end));
        index = end + 1;
    }
    spans
}

/// The top-level blocks of positions `first..=last`, the one-line ones grouped into paragraphs.
/// A block with a body — a function, a class — stands alone. A run of one-line blocks between
/// blank lines is one paragraph while it is at most [`MAX_PARAGRAPH_CHARS`] long — a paragraph of
/// prose, a run of imports — and a longer run, which is data rather than prose, is a block per
/// line.
fn paragraph_spans(texts: &[String], first: usize, last: usize) -> Vec<(usize, usize)> {
    fn flush(texts: &[String], run: Option<(usize, usize)>, out: &mut Vec<(usize, usize)>) {
        let Some((start, end)) = run else { return };
        let chars = texts[start - 1..end]
            .iter()
            .map(|line| line.chars().count() + 1)
            .sum::<usize>();
        if chars <= MAX_PARAGRAPH_CHARS {
            out.push((start, end));
        } else {
            out.extend((start..=end).map(|position| (position, position)));
        }
    }
    let mut out = Vec::new();
    let mut run: Option<(usize, usize)> = None;
    for (start, end) in block_spans(texts, first, last) {
        if start != end {
            flush(texts, run.take(), &mut out);
            out.push((start, end));
            continue;
        }
        run = match run {
            Some((run_start, run_end)) if run_end + 1 == start => Some((run_start, start)),
            other => {
                flush(texts, other, &mut out);
                Some((start, start))
            }
        };
    }
    flush(texts, run, &mut out);
    out
}

/// The blocks of positions `first..=last` as [`Blocks::text`] cuts them: paragraph spans, with
/// every container opened into its head, its members and its closing line.
fn text_spans(texts: &[String], first: usize, last: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (start, end) in paragraph_spans(texts, first, last) {
        match container_head(texts, start, end) {
            Some(head_end) => {
                out.push((start, head_end));
                out.extend(text_spans(texts, head_end + 1, end));
            }
            None => out.push((start, end)),
        }
    }
    out
}

/// Where a container's head ends, when the block `start..=end` looks like one: too long for one
/// question, and holding two or more blocks with bodies. The head is the block's annotations and
/// first line, or — for a signature over several lines — everything up to the line at the block's
/// own indentation that closes the bracket.
fn container_head(texts: &[String], start: usize, end: usize) -> Option<usize> {
    let chars = texts[start - 1..end]
        .iter()
        .map(|line| line.chars().count() + 1)
        .sum::<usize>();
    if start == end || chars <= MAX_REQUEST_CHARS {
        return None;
    }
    let base = indent_of(&texts[start - 1])?;
    let mut index = start;
    while index < end && is_annotation(&texts[index - 1]) && indent_of(&texts[index]) == Some(base)
    {
        index += 1;
    }
    let head_end = (index + 1..end)
        .find(|&position| {
            let line = &texts[position - 1];
            indent_of(line) == Some(base) && line.trim_start().starts_with([')', ']'])
        })
        .unwrap_or(index);
    let members = block_spans(texts, head_end + 1, end)
        .into_iter()
        .filter(|(first, last)| first != last)
        .count();
    (members >= 2).then_some(head_end)
}

// ----------------------------------------------------------------- Log records

/// A line that goes on the record above it rather than starting a new one.
fn continues_record(line: &str, base: usize) -> bool {
    match indent_of(line) {
        None => false,
        Some(indent) => {
            indent > base || strip_leading_sgr(line).trim_start().starts_with("Caused by")
        }
    }
}

fn record_spans(texts: &[String], first: usize, last: usize) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut index = first;
    while index <= last {
        let line = &texts[index - 1];
        let Some(base) = indent_of(line) else {
            index += 1;
            continue;
        };
        let traceback = strip_leading_sgr(line)
            .trim_start()
            .starts_with("Traceback (most recent call last)");
        let mut end = index;
        while end < last {
            let next = &texts[end];
            if continues_record(next, base) {
                end += 1;
                continue;
            }
            // A Python traceback ends with the exception, at the traceback's own indentation.
            if traceback && end > index && indent_of(next).is_some() {
                end += 1;
            }
            break;
        }
        spans.push((index, end));
        index = end + 1;
    }
    spans
}

// ----------------------------------------------------------------- Markdown

fn heading_levels(texts: &[String]) -> Vec<Option<usize>> {
    let mut fence: Option<char> = None;
    texts
        .iter()
        .map(|line| {
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            if indent <= 3 {
                if let Some(marker) = ['`', '~']
                    .into_iter()
                    .find(|marker| trimmed.starts_with(&marker.to_string().repeat(3)))
                {
                    match fence {
                        None => fence = Some(marker),
                        Some(open) if open == marker => fence = None,
                        Some(_) => {}
                    }
                    return None;
                }
            }
            if fence.is_some() || indent > 3 {
                return None;
            }
            let hashes = trimmed.chars().take_while(|character| *character == '#').count();
            let rest = &trimmed[hashes..];
            ((1..=6).contains(&hashes) && (rest.is_empty() || rest.starts_with([' ', '\t'])))
                .then_some(hashes)
        })
        .collect()
}

// ----------------------------------------------------------------- Code symbols

struct Symbol {
    first: usize,
    last: usize,
    children: Vec<Symbol>,
}

/// Symbol spans nested by containment. A span outside `1..=count`, backwards, or straddling the
/// symbol it starts inside is dropped.
fn nest(spans: &[(usize, usize)], count: usize) -> Vec<Symbol> {
    let mut spans = spans
        .iter()
        .copied()
        .filter(|&(first, last)| first >= 1 && first <= last && last <= count)
        .collect::<Vec<_>>();
    spans.sort_by(|left, right| left.0.cmp(&right.0).then(right.1.cmp(&left.1)));
    spans.dedup();
    fn attach(stack: &mut [Symbol], roots: &mut Vec<Symbol>, done: Symbol) {
        match stack.last_mut() {
            Some(parent) => parent.children.push(done),
            None => roots.push(done),
        }
    }
    let mut roots = Vec::new();
    let mut stack: Vec<Symbol> = Vec::new();
    for (first, last) in spans {
        while stack.last().is_some_and(|top| top.last < first) {
            let done = stack.pop().expect("a symbol");
            attach(&mut stack, &mut roots, done);
        }
        if stack.last().is_some_and(|top| last > top.last) {
            continue;
        }
        stack.push(Symbol {
            first,
            last,
            children: Vec::new(),
        });
    }
    while let Some(done) = stack.pop() {
        attach(&mut stack, &mut roots, done);
    }
    roots
}

/// The blocks of positions `first..=last`, which `symbols` lie inside: a symbol with nothing
/// nested in it whole, with its annotations; a symbol with symbols in it opened, its own lines
/// cut by indentation between them.
fn symbol_spans(
    texts: &[String],
    first: usize,
    last: usize,
    symbols: &[Symbol],
    out: &mut Vec<(usize, usize)>,
) {
    let mut cursor = first;
    for symbol in symbols {
        // Only at the symbol's own indentation: an indented comment above it ends the block
        // before, whatever it says.
        let indent = indent_of(&texts[symbol.first - 1]);
        let mut start = symbol.first;
        while start > cursor
            && is_annotation(&texts[start - 2])
            && indent_of(&texts[start - 2]) == indent
        {
            start -= 1;
        }
        if start > cursor {
            out.extend(text_spans(texts, cursor, start - 1));
        }
        if symbol.children.is_empty() {
            out.push((start, symbol.last));
        } else {
            // A container's annotations go with its header, the first of its own lines.
            if start < symbol.first {
                out.extend(block_spans(texts, start, symbol.first).into_iter().take(1));
                symbol_spans(texts, symbol.first + 1, symbol.last, &symbol.children, out);
            } else {
                symbol_spans(texts, symbol.first, symbol.last, &symbol.children, out);
            }
        }
        cursor = symbol.last + 1;
    }
    if cursor <= last {
        out.extend(text_spans(texts, cursor, last));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blocks never overlap and are in order; what they leave out is blank.
    fn assert_well_formed(blocks: &Blocks) {
        let mut cursor = 1;
        for &(first, last) in blocks.spans() {
            assert!(
                first >= cursor && first <= last && last <= blocks.len(),
                "{:?}",
                blocks.spans()
            );
            for position in cursor..first {
                assert!(is_blank(&blocks.texts[position - 1]), "line {position} is in no block");
            }
            cursor = last + 1;
        }
        for position in cursor..=blocks.len() {
            assert!(is_blank(&blocks.texts[position - 1]), "line {position} is in no block");
        }
    }

    #[test]
    fn the_request_budget_grows_with_the_input_up_to_the_ceiling() {
        assert_eq!(request_budget(100), MIN_REQUEST_CHARS);
        assert_eq!(request_budget(600_000), 4_000);
        assert_eq!(request_budget(50_000_000), MAX_REQUEST_CHARS);
    }

    /// Size decides how many blocks share a request, up to the question cap; a block larger than
    /// the budget goes alone.
    #[test]
    fn batches_pack_blocks_by_size_and_count() {
        assert_eq!(batches(&[], 100), Vec::<Range<usize>>::new());
        assert_eq!(batches(&[40, 40, 40, 300, 10], 100), vec![0..2, 2..3, 3..4, 4..5]);
        let tiny = vec![1; 100];
        let packed = batches(&tiny, 10_000);
        assert_eq!(packed.len(), 4);
        assert!(packed
            .iter()
            .all(|batch| batch.len() <= MAX_QUESTIONS_PER_REQUEST));
        assert_eq!(packed.last().unwrap().end, 100);
    }

    #[test]
    fn plain_text_is_cut_into_paragraphs_of_blocks() {
        let text = "\
use a;
use b;

/// Adds one.
#[inline]
fn add(a: u32) -> u32 {
    a + 1
}

fn long(
    a: u32,
) -> u32 {

    a
}
    // an indented tail
";
        let blocks = Blocks::text(text);
        assert_well_formed(&blocks);
        assert_eq!(blocks.spans(), &[(1, 2), (4, 8), (10, 16)]);
        assert_eq!(blocks.text_of(1, 2), "use a;\nuse b;");
        assert!(Blocks::text("").spans().is_empty());
        assert_eq!(Blocks::text("a\nb\r\nc").text_of(1, 3), "a\nb\nc");

        // A block with a body stands alone even with no blank line around it.
        let text = "one();\ntwo();\nfn f() {\n    body();\n}\nthree();";
        assert_eq!(Blocks::text(text).spans(), &[(1, 2), (3, 5), (6, 6)]);

        // A container too long for one question is opened: its head, its members, its closing
        // line. A long block that holds no members is kept whole, to be scored in parts.
        let body = (0..150)
            .map(|index| format!("        let value_{index} = compute(input, {index});"))
            .collect::<Vec<_>>()
            .join("\n");
        let module = format!(
            "#[cfg(test)]\nmod tests {{\n    use super::*;\n\n    /// First.\n    fn first() {{\n{body}\n    }}\n\n    fn second() {{\n{body}\n    }}\n}}"
        );
        let blocks = Blocks::text(&module);
        assert_well_formed(&blocks);
        assert_eq!(
            blocks.spans(),
            &[(1, 2), (3, 3), (5, 157), (159, 310), (311, 311)]
        );
        let long_function = format!("fn long() {{\n{body}\n{body}\n{body}\n{body}\n}}");
        assert_eq!(Blocks::text(&long_function).spans(), &[(1, 602)]);

        // A long run of one-line blocks is data, not prose: a block per line.
        let rows = (0..200)
            .map(|index| format!("{index},alpha,beta,gamma"))
            .collect::<Vec<_>>()
            .join("\n");
        let blocks = Blocks::text(&rows);
        assert_eq!(blocks.spans().len(), 200);
        assert_eq!(blocks.spans()[7], (8, 8));
    }

    /// A block longer than one question takes is cut into parts along its lines, and a line
    /// longer than that into slices; every part says where it came from.
    #[test]
    fn a_long_block_is_cut_into_parts_for_scoring() {
        let long_lines = (0..30)
            .map(|index| format!("{index:04}{}", "x".repeat(996)))
            .collect::<Vec<_>>()
            .join("\n");
        let blocks = Blocks::text(&long_lines);
        let parts = blocks.parts(1, 30);
        assert_eq!(parts.len(), 3);
        assert_eq!((parts[0].1, parts[0].2), (1, 11));
        assert_eq!((parts[2].1, parts[2].2), (23, 30));
        assert!(parts
            .iter()
            .all(|part| part.0.chars().count() <= MAX_REQUEST_CHARS));

        let monster = format!("short\n{}\nafter", "y".repeat(MAX_REQUEST_CHARS * 2 + 5));
        let blocks = Blocks::text(&monster);
        let parts = blocks.parts(1, 3);
        let ranges = parts.iter().map(|part| (part.1, part.2)).collect::<Vec<_>>();
        assert_eq!(ranges, vec![(1, 1), (2, 2), (2, 2), (2, 2), (3, 3)]);
        assert_eq!(parts[3].0.chars().count(), 5);

        let small = Blocks::text("one\ntwo");
        assert_eq!(small.parts(1, 2), vec![("one\ntwo".to_owned(), 1, 2)]);
    }

    #[test]
    fn log_records_keep_their_continuations() {
        let text = "\
[info] starting
TypeError: boom
    at render (App.tsx:4:5)
    at commit (react-dom.js:9:1)
Caused by: something else

Traceback (most recent call last):
  File \"a.py\", line 1, in <module>
    foo()
NameError: name 'foo' is not defined
[info] done";
        let blocks = Blocks::log_records(text);
        assert_well_formed(&blocks);
        assert_eq!(blocks.spans(), &[(1, 1), (2, 5), (7, 10), (11, 11)]);
    }

    /// A read is a block; a read that stops mid-line joins the next, and a read's trailing
    /// newline adds no empty line.
    #[test]
    fn server_reads_are_blocks_and_split_lines_are_rejoined() {
        let reads = [
            "[vite] ready\n".to_owned(),
            "Error: failed\nFile: src/App.tsx\n".to_owned(),
            "\n".to_owned(),
            "partial ".to_owned(),
            "line\nnext\n".to_owned(),
        ];
        let blocks = Blocks::reads(&reads);
        assert_eq!(blocks.len(), 6);
        assert_eq!(blocks.text_of(5, 5), "partial line");
        assert_eq!(blocks.spans(), &[(1, 1), (2, 3), (5, 6)]);
    }

    #[test]
    fn entries_are_blocks_however_many_lines_they_span() {
        let blocks = Blocks::entries(vec![
            "[error] boom\n    at a".to_owned(),
            "[log] ok".to_owned(),
        ]);
        assert_eq!(blocks.spans(), &[(1, 1), (2, 2)]);
        assert_eq!(blocks.text_of(1, 1), "[error] boom\n    at a");
    }

    #[test]
    fn markdown_sections_run_to_the_next_heading_and_ignore_fenced_hashes() {
        let text = "\
intro

# One
text
## One.a
```sh
# not a heading
```

# Two
end";
        let blocks = Blocks::markdown(text);
        assert_well_formed(&blocks);
        assert_eq!(blocks.spans(), &[(1, 1), (3, 4), (5, 8), (10, 11)]);
    }

    /// The innermost symbols are the blocks: a container is opened into its members, its own
    /// header and closing lines are blocks of their own, and the comments directly above a symbol
    /// at its indentation go with it.
    #[test]
    fn innermost_symbols_are_the_blocks() {
        let text = "\
use std::fmt;

/// Adds.
#[inline]
fn add(a: u32) -> u32 {
    a + 1
}

/// A thing.
impl Thing {
    /// One.
    fn one(&self) {}

    fn two(&self) {}
}";
        let blocks = Blocks::symbols(
            text,
            &[(5, 7), (10, 15), (12, 12), (14, 14), (6, 9), (40, 50)],
        );
        assert_well_formed(&blocks);
        assert_eq!(
            blocks.spans(),
            &[(1, 1), (3, 7), (9, 10), (11, 12), (14, 14), (15, 15)]
        );

        // An indented comment belongs to the block above, not to the next symbol as its doc.
        let text = "fn a() { one(); }\n    // about a\nfn b() {}";
        let blocks = Blocks::symbols(text, &[(1, 1), (3, 3)]);
        assert_eq!(blocks.spans(), &[(1, 1), (2, 2), (3, 3)]);
    }
}
