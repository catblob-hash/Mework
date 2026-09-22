//! Cutting bulk text and lists into the candidates a decision model scores.
//!
//! The cut is adaptive: the chunk count is capped so one tool call stays within a bounded
//! number of requests, and chunk size grows with the material to keep under that cap. Line
//! numbers are 1-based and inclusive, matching what `read` reports, so a hit's range can be
//! fed straight back into `read` or `find_content` to narrow further.

/// Stage-one requests one tool call may spend. Together with [`MIN_COARSE_LINES`] this decides
/// how coarse the first pass is for a large input.
pub const MAX_COARSE_CHUNKS: usize = 48;
/// A coarse chunk is never smaller than this many lines, so short inputs are not sliced into
/// fragments too small to judge.
pub const MIN_COARSE_LINES: usize = 16;
/// Upper bound on one chunk's text. Jev's state limit is 32k tokens; this keeps a chunk near
/// 3k tokens so the query and the rubric stay in proportion to it.
pub const MAX_CHUNK_CHARS: usize = 12_000;
/// A single line longer than this is cut. Minified bundles and log lines with embedded blobs
/// would otherwise make one line the whole chunk.
pub const MAX_LINE_CHARS: usize = 2_000;
/// How many pieces a winning coarse chunk is refined into, and how small a piece may be.
pub const REFINE_PARTS: usize = 6;
pub const MIN_REFINE_LINES: usize = 3;

/// A run of consecutive lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// 1-based, inclusive.
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
    /// Some line inside was cut to [`MAX_LINE_CHARS`].
    pub truncated: bool,
}

impl Chunk {
    pub fn label(&self) -> String {
        if self.start_line == self.end_line {
            format!("line {}", self.start_line)
        } else {
            format!("lines {}-{}", self.start_line, self.end_line)
        }
    }

    pub fn line_count(&self) -> usize {
        self.end_line + 1 - self.start_line
    }
}

/// The coarse cut of one text, plus what it left out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkPlan {
    pub chunks: Vec<Chunk>,
    /// Lines the plan covers, from the first. Less than `total_lines` when the cap bit.
    pub covered_lines: usize,
    pub total_lines: usize,
}

fn cut_line(line: &str) -> (String, bool) {
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.chars().count() <= MAX_LINE_CHARS {
        return (line.to_owned(), false);
    }
    let mut cut = line.chars().take(MAX_LINE_CHARS).collect::<String>();
    cut.push('…');
    (cut, true)
}

/// Cuts `text` into at most [`MAX_COARSE_CHUNKS`] chunks of consecutive lines, each at least
/// [`MIN_COARSE_LINES`] long unless the character bound or the end of the text stops it.
/// Very long inputs with many long lines can still exceed the cap; the tail is dropped and
/// reported through `covered_lines`.
pub fn chunk_lines(text: &str) -> ChunkPlan {
    chunk_lines_from(text, 1)
}

/// [`chunk_lines`] for a slice of a larger text whose first line is `first_line`.
pub fn chunk_lines_from(text: &str, first_line: usize) -> ChunkPlan {
    let lines = text.lines().collect::<Vec<_>>();
    let total_lines = lines.len();
    if total_lines == 0 {
        return ChunkPlan {
            chunks: Vec::new(),
            covered_lines: 0,
            total_lines: 0,
        };
    }
    let lines_per_chunk = MIN_COARSE_LINES.max(total_lines.div_ceil(MAX_COARSE_CHUNKS));
    let mut chunks = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_chars = 0;
    let mut current_truncated = false;
    let mut current_start = first_line;
    let flush = |chunks: &mut Vec<Chunk>,
                 current: &mut Vec<String>,
                 current_chars: &mut usize,
                 current_truncated: &mut bool,
                 current_start: &mut usize,
                 next_start: usize| {
        if current.is_empty() {
            return;
        }
        chunks.push(Chunk {
            start_line: *current_start,
            end_line: next_start - 1,
            text: current.join("\n"),
            truncated: *current_truncated,
        });
        current.clear();
        *current_chars = 0;
        *current_truncated = false;
        *current_start = next_start;
    };
    for (offset, line) in lines.into_iter().enumerate() {
        let line_number = first_line + offset;
        let (line, truncated) = cut_line(line);
        let line_chars = line.chars().count() + 1;
        let full = current.len() >= lines_per_chunk
            || (!current.is_empty() && current_chars + line_chars > MAX_CHUNK_CHARS);
        if full {
            flush(
                &mut chunks,
                &mut current,
                &mut current_chars,
                &mut current_truncated,
                &mut current_start,
                line_number,
            );
        }
        current_chars += line_chars;
        current_truncated |= truncated;
        current.push(line);
    }
    flush(
        &mut chunks,
        &mut current,
        &mut current_chars,
        &mut current_truncated,
        &mut current_start,
        first_line + total_lines,
    );
    chunks.truncate(MAX_COARSE_CHUNKS);
    let covered_lines = chunks
        .last()
        .map(|chunk| chunk.end_line + 1 - first_line)
        .unwrap_or(0);
    ChunkPlan {
        chunks,
        covered_lines,
        total_lines,
    }
}

/// Cuts a winning chunk into up to [`REFINE_PARTS`] pieces of at least [`MIN_REFINE_LINES`]
/// lines. Empty when the chunk is already too small to refine: the caller keeps the chunk.
pub fn refine_chunk(chunk: &Chunk) -> Vec<Chunk> {
    let lines = chunk.text.lines().collect::<Vec<_>>();
    if lines.len() <= MIN_REFINE_LINES {
        return Vec::new();
    }
    let per_part = MIN_REFINE_LINES.max(lines.len().div_ceil(REFINE_PARTS));
    lines
        .chunks(per_part)
        .enumerate()
        .map(|(index, part)| Chunk {
            start_line: chunk.start_line + index * per_part,
            end_line: chunk.start_line + index * per_part + part.len() - 1,
            text: part.join("\n"),
            truncated: false,
        })
        .collect()
}

/// Groups a list of short entries (paths, element lines) into at most `max_groups` runs of at
/// least `min_per_group` consecutive entries, keeping the list's order. Returns index ranges.
pub fn group_entries(
    count: usize,
    max_groups: usize,
    min_per_group: usize,
) -> Vec<std::ops::Range<usize>> {
    if count == 0 {
        return Vec::new();
    }
    let per_group = min_per_group.max(1).max(count.div_ceil(max_groups.max(1)));
    (0..count)
        .step_by(per_group)
        .map(|start| start..(start + per_group).min(count))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(count: usize) -> String {
        (1..=count)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_short_text_is_one_chunk_with_exact_line_numbers() {
        let plan = chunk_lines("a\nb\r\nc");
        assert_eq!(plan.total_lines, 3);
        assert_eq!(plan.covered_lines, 3);
        assert_eq!(plan.chunks.len(), 1);
        assert_eq!(plan.chunks[0].start_line, 1);
        assert_eq!(plan.chunks[0].end_line, 3);
        assert_eq!(plan.chunks[0].text, "a\nb\nc");
        assert_eq!(plan.chunks[0].label(), "lines 1-3");
        assert!(chunk_lines("").chunks.is_empty());
    }

    #[test]
    fn a_medium_text_is_cut_into_minimum_sized_chunks_that_tile_it() {
        let plan = chunk_lines(&numbered(40));
        assert_eq!(plan.chunks.len(), 3);
        assert_eq!(plan.chunks[0].line_count(), MIN_COARSE_LINES);
        assert_eq!(plan.chunks[1].start_line, MIN_COARSE_LINES + 1);
        assert_eq!(plan.chunks[2].end_line, 40);
        assert_eq!(plan.covered_lines, 40);
        // Every line is in exactly one chunk.
        let rebuilt = plan
            .chunks
            .iter()
            .map(|chunk| chunk.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(rebuilt, numbered(40));
    }

    /// The chunk count is what is bounded; chunk size grows with the input.
    #[test]
    fn a_large_text_grows_its_chunks_instead_of_its_chunk_count() {
        let plan = chunk_lines(&numbered(10_000));
        assert!(plan.chunks.len() <= MAX_COARSE_CHUNKS);
        assert_eq!(plan.covered_lines, 10_000);
        assert_eq!(plan.chunks[0].line_count(), 10_000_usize.div_ceil(MAX_COARSE_CHUNKS));
        assert_eq!(plan.chunks.last().unwrap().end_line, 10_000);
    }

    #[test]
    fn a_slice_of_a_file_keeps_the_file_line_numbers() {
        let plan = chunk_lines_from("x\ny", 120);
        assert_eq!(plan.chunks[0].start_line, 120);
        assert_eq!(plan.chunks[0].end_line, 121);
    }

    /// Long lines close a chunk early, and a single monster line is cut rather than sent whole.
    #[test]
    fn long_lines_bound_a_chunk_by_characters() {
        let long = "x".repeat(MAX_LINE_CHARS);
        let text = (0..7).map(|_| long.as_str()).collect::<Vec<_>>().join("\n");
        let plan = chunk_lines(&text);
        assert_eq!(plan.chunks.len(), 2, "{} chars a line fill a chunk in five", MAX_LINE_CHARS);
        assert_eq!(plan.chunks[0].line_count(), 5);
        assert_eq!(plan.chunks[1].start_line, 6);
        assert!(plan.chunks.iter().all(|chunk| !chunk.truncated));

        let monster = "y".repeat(MAX_LINE_CHARS + 5);
        let plan = chunk_lines(&format!("short\n{monster}"));
        assert_eq!(plan.chunks.len(), 1);
        assert!(plan.chunks[0].truncated);
        assert!(plan.chunks[0].text.ends_with('…'));
        assert_eq!(plan.chunks[0].text.chars().count(), "short\n".len() + MAX_LINE_CHARS + 1);
    }

    #[test]
    fn a_pathological_input_is_capped_and_reports_its_coverage() {
        let long = "z".repeat(MAX_LINE_CHARS);
        let lines = (0..250).map(|_| long.as_str()).collect::<Vec<_>>().join("\n");
        let plan = chunk_lines(&lines);
        assert_eq!(plan.chunks.len(), MAX_COARSE_CHUNKS);
        assert_eq!(plan.covered_lines, MAX_COARSE_CHUNKS * 5);
        assert_eq!(plan.total_lines, 250);
    }

    #[test]
    fn refinement_splits_a_chunk_into_addressable_pieces() {
        let chunk = Chunk {
            start_line: 101,
            end_line: 130,
            text: numbered(30),
            truncated: false,
        };
        let pieces = refine_chunk(&chunk);
        assert_eq!(pieces.len(), REFINE_PARTS);
        assert_eq!(pieces[0].start_line, 101);
        assert_eq!(pieces[0].end_line, 105);
        assert_eq!(pieces[5].end_line, 130);
        assert_eq!(pieces[3].text, "line 16\nline 17\nline 18\nline 19\nline 20");
        // Too small to refine: the caller keeps the chunk itself.
        assert!(refine_chunk(&Chunk {
            start_line: 1,
            end_line: 3,
            text: "a\nb\nc".into(),
            truncated: false
        })
        .is_empty());
        // Uneven splits still tile the chunk.
        let pieces = refine_chunk(&chunk_lines(&numbered(7)).chunks[0]);
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[2].start_line, 7);
        assert_eq!(pieces[2].end_line, 7);
    }

    #[test]
    fn entry_groups_tile_a_list_in_order() {
        assert!(group_entries(0, 48, 8).is_empty());
        assert_eq!(group_entries(5, 48, 8), vec![0..5]);
        let groups = group_entries(100, 48, 8);
        assert_eq!(groups.len(), 13);
        assert_eq!(groups[0], 0..8);
        assert_eq!(groups[12], 96..100);
        let groups = group_entries(1000, 48, 8);
        assert_eq!(groups.len(), 48);
        assert_eq!(groups[47].end, 1000);
    }
}
