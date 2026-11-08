//! Bidirectional text (UAX #9) for line layout.
//!
//! The platform shaper reorders the glyphs of one line into visual order. This module adds
//! what GPUI needs around it:
//!
//! - the paragraph's base direction (P2 and P3), passed to the shaper;
//! - each shaped run's direction, from the resolved embedding levels (L1);
//! - line breaking in logical order, so that a wrapped right-to-left paragraph reads its lines
//!   from top to bottom, each one shaped and reordered on its own;
//! - carets, hit tests, and selection spans that follow each run's direction.
//!
//! Text without a right-to-left character never enters this module, so its layout is unchanged.

use std::ops::Range;

use smallvec::SmallVec;
use unicode_bidi::{BidiClass, Level, ParagraphBidiInfo, bidi_class};

use crate::{FontRun, LineLayout, Pixels, ShapedRun, px};

/// Whether `text` holds a right-to-left character or control.
pub(crate) fn has_rtl(text: &str) -> bool {
    text.chars().any(|character| {
        matches!(
            bidi_class(character),
            BidiClass::R | BidiClass::AL | BidiClass::RLE | BidiClass::RLO | BidiClass::RLI
        )
    })
}

/// UAX #9 P2 and P3: the paragraph is right-to-left when its first strong character is.
pub(crate) fn paragraph_is_rtl(text: &str) -> bool {
    unicode_bidi::get_base_direction(text) == unicode_bidi::Direction::Rtl
}

/// Splits the shaped runs where the resolved direction changes and records each run's
/// direction. The shaper merges runs of one font across directions.
pub(crate) fn mark_run_directions(layout: &mut LineLayout, text: &str, rtl: bool) {
    let base = if rtl { Level::rtl() } else { Level::ltr() };
    let info = ParagraphBidiInfo::new(text, Some(base));
    let levels = info.reordered_levels(0..text.len());
    let level_rtl = |index: usize| {
        levels
            .get(index)
            .map_or(rtl, |level: &Level| level.is_rtl())
    };
    let mut runs: Vec<ShapedRun> = Vec::with_capacity(layout.runs.len());
    for run in layout.runs.drain(..) {
        let mut current: Option<ShapedRun> = None;
        for glyph in run.glyphs {
            let glyph_rtl = level_rtl(glyph.index);
            match &mut current {
                Some(current) if current.is_rtl == glyph_rtl => current.glyphs.push(glyph),
                _ => {
                    runs.extend(current.take());
                    current = Some(ShapedRun {
                        font_id: run.font_id,
                        glyphs: vec![glyph],
                        is_rtl: glyph_rtl,
                        font_size: run.font_size,
                    });
                }
            }
        }
        runs.extend(current);
    }
    layout.runs = runs;
}

/// The font runs that cover `range` of a paragraph's text.
pub(crate) fn runs_in(runs: &[FontRun], range: Range<usize>) -> SmallVec<[FontRun; 2]> {
    let mut sliced = SmallVec::new();
    let mut start = 0;
    for run in runs {
        let end = start + run.len;
        let overlap = start.max(range.start)..end.min(range.end);
        if overlap.start < overlap.end {
            sliced.push(FontRun {
                len: overlap.end - overlap.start,
                font_id: run.font_id,
                font_size: run.font_size,
                padding: run.padding,
                letter_spacing: run.letter_spacing,
            });
        }
        start = end;
    }
    sliced
}

/// One glyph's visual extent and the character it starts.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Edge {
    pub index: usize,
    pub left: Pixels,
    pub right: Pixels,
    pub rtl: bool,
}

/// The visual extents of the glyphs from `start` (run, glyph) up to `end`, in visual order. A
/// glyph reaches to the next glyph's origin, the last one to `end_x`.
pub(crate) fn edges(
    layout: &LineLayout,
    start: (usize, usize),
    end: Option<(usize, usize)>,
    end_x: Pixels,
) -> Vec<Edge> {
    let mut edges: Vec<Edge> = Vec::new();
    for (run_ix, run) in layout.runs.iter().enumerate() {
        for (glyph_ix, glyph) in run.glyphs.iter().enumerate() {
            if (run_ix, glyph_ix) < start {
                continue;
            }
            if end.is_some_and(|end| (run_ix, glyph_ix) >= end) {
                break;
            }
            if let Some(previous) = edges.last_mut() {
                previous.right = glyph.position.x;
            }
            edges.push(Edge {
                index: glyph.index,
                left: glyph.position.x,
                right: end_x,
                rtl: run.is_rtl,
            });
        }
    }
    edges
}

/// The first character index after the cluster that starts at `index`, within `line`.
fn next_index(edges: &[Edge], index: usize, line: &Range<usize>) -> usize {
    edges
        .iter()
        .map(|edge| edge.index)
        .filter(|candidate| *candidate > index)
        .min()
        .unwrap_or(line.end)
}

/// The caret's x before the character at `index`: the leading edge of that character's glyph,
/// which is its right edge in a right-to-left run. At the end of the line, the trailing edge of
/// the last character.
pub(crate) fn caret_x(edges: &[Edge], index: usize, line: &Range<usize>) -> Pixels {
    let at = edges
        .iter()
        .filter(|edge| edge.index >= index)
        .min_by_key(|edge| edge.index);
    match at {
        Some(edge) if index < line.end => {
            if edge.rtl {
                // A cluster of several glyphs (marks, ligature parts) leads from its rightmost.
                edges
                    .iter()
                    .filter(|other| other.index == edge.index)
                    .map(|other| other.right)
                    .fold(edge.right, Pixels::max)
            } else {
                edge.left
            }
        }
        _ => match edges.iter().max_by_key(|edge| edge.index) {
            Some(last) if last.rtl => edges
                .iter()
                .filter(|other| other.index == last.index)
                .map(|other| other.left)
                .fold(last.left, Pixels::min),
            Some(last) => last.right,
            None => px(0.),
        },
    }
}

/// The character boundary nearest to `x`: inside a glyph, its leading half gives the index
/// before it and its trailing half the index after it. Past either end, the boundary at that
/// visual end.
pub(crate) fn closest_index(edges: &[Edge], x: Pixels, line: &Range<usize>) -> usize {
    let Some(first) = edges.first() else {
        return line.start;
    };
    let last = edges.last().unwrap_or(first);
    let edge = if x < first.left {
        first
    } else if x >= last.right {
        last
    } else {
        edges
            .iter()
            .find(|edge| edge.left <= x && x < edge.right)
            .unwrap_or(last)
    };
    let middle = (edge.left + edge.right) / 2.;
    let leading = if edge.rtl { x >= middle } else { x < middle };
    if leading {
        edge.index
    } else {
        next_index(edges, edge.index, line)
    }
}

/// The index of the character under `x`, if a glyph is there.
pub(crate) fn index_at(edges: &[Edge], x: Pixels) -> Option<usize> {
    edges
        .iter()
        .find(|edge| edge.left <= x && x < edge.right)
        .map(|edge| edge.index)
}

/// The visual x-spans that `range` covers. A selection across a direction change is several
/// spans; adjacent spans merge.
pub(crate) fn selection_spans(
    edges: &[Edge],
    range: &Range<usize>,
) -> SmallVec<[Range<Pixels>; 2]> {
    let mut spans: SmallVec<[Range<Pixels>; 2]> = SmallVec::new();
    for edge in edges {
        if !range.contains(&edge.index) || edge.right <= edge.left {
            continue;
        }
        match spans.last_mut() {
            Some(span) if span.end >= edge.left => span.end = span.end.max(edge.right),
            _ => spans.push(edge.left..edge.right),
        }
    }
    spans
}

/// Line breaks in logical order: each line is the longest run of whole break segments that fits
/// `wrap_width`, measured with the advances of the unwrapped paragraph. A segment wider than a
/// line breaks between characters. Returns each line's byte range.
pub(crate) fn wrap_logically(
    text: &str,
    unwrapped: &LineLayout,
    wrap_width: Pixels,
    max_lines: Option<usize>,
    break_indices: impl Iterator<Item = usize>,
) -> SmallVec<[Range<usize>; 1]> {
    // The advance of each character cluster, at the byte that starts it.
    let edges = edges(unwrapped, (0, 0), None, unwrapped.width);
    let mut advance = vec![px(0.); text.len() + 1];
    let mut starts = vec![false; text.len() + 1];
    for edge in &edges {
        advance[edge.index] += edge.right - edge.left;
        starts[edge.index] = true;
    }
    starts[text.len()] = true;
    let mut prefix = Vec::with_capacity(text.len() + 1);
    let mut sum = px(0.);
    for advance in &advance {
        prefix.push(sum);
        sum += *advance;
    }
    // Trailing spaces hang past the line's end, as in CSS.
    let width = |range: Range<usize>| {
        let trimmed = text[range.clone()].trim_end_matches(' ').len();
        prefix[range.start + trimmed] - prefix[range.start]
    };
    let breaks: Vec<usize> = break_indices
        .filter(|index| *index > 0 && *index <= text.len() && starts[*index])
        .collect();

    let mut lines: SmallVec<[Range<usize>; 1]> = SmallVec::new();
    let mut start = 0;
    let mut fitted: Option<usize> = None;
    let mut candidates = breaks.into_iter().peekable();
    while let Some(&candidate) = candidates.peek() {
        if max_lines.is_some_and(|max| lines.len() + 1 >= max) {
            break;
        }
        if width(start..candidate) <= wrap_width {
            fitted = Some(candidate);
            candidates.next();
            continue;
        }
        let end = match fitted.take() {
            Some(end) if end > start => end,
            // One segment is wider than the line: break between characters.
            _ => {
                let mut end = start;
                for index in start + 1..candidate {
                    if starts[index] && width(start..index) <= wrap_width {
                        end = index;
                    } else if starts[index] {
                        break;
                    }
                }
                if end == start {
                    end = (start + 1..=candidate)
                        .find(|index| starts[*index])
                        .unwrap_or(candidate);
                }
                end
            }
        };
        lines.push(start..end);
        start = end;
    }
    if start < text.len() || lines.is_empty() {
        lines.push(start..text.len());
    }
    lines
}
