use crate::{
    Bounds, FontId, GlyphId, Pixels, PlatformTextSystem, Point, SharedString, Size, TextAlign,
    point, px, size,
};
use collections::{FxHashMap, FxHashSet};
use itertools::Itertools;
use smallvec::SmallVec;
use std::{
    borrow::Borrow,
    cell::RefCell,
    hash::{Hash, Hasher},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::{LineWrapper, bidi};

/// A laid out and styled line of text
#[derive(Default, Debug)]
pub struct LineLayout {
    /// The font size for this line
    pub font_size: Pixels,
    /// The width of the line
    pub width: Pixels,
    /// The ascent of the line
    pub ascent: Pixels,
    /// The descent of the line
    pub descent: Pixels,
    /// The shaped runs that make up this line
    pub runs: Vec<ShapedRun>,
    /// The length of the line in utf-8 bytes
    pub len: usize,
}

/// A run of text that has been shaped .
#[derive(Debug, Clone)]
pub struct ShapedRun {
    /// The font id for this run
    pub font_id: FontId,
    /// The glyphs that make up this run
    pub glyphs: Vec<ShapedGlyph>,
    /// Whether the run reads right to left (an odd UAX #9 embedding level). Its glyphs are in
    /// visual order, so their indices decrease. GPUI sets it after shaping; a platform text
    /// system leaves it false.
    pub is_rtl: bool,
    /// The run's own font size, from a [`crate::TextRun`] that set one. `None` draws at the
    /// line's size. GPUI sets it after shaping; a platform text system leaves it `None`.
    pub font_size: Option<Pixels>,
}

/// A single glyph, ready to paint.
#[derive(Clone, Debug)]
pub struct ShapedGlyph {
    /// The ID for this glyph, as determined by the text system.
    pub id: GlyphId,

    /// The position of this glyph in its containing line.
    pub position: Point<Pixels>,

    /// The index of this glyph in the original text.
    pub index: usize,

    /// Whether this glyph is an emoji
    pub is_emoji: bool,
}

impl LineLayout {
    /// The index for the character at the given x coordinate
    pub fn index_for_x(&self, x: Pixels) -> Option<usize> {
        if x >= self.width {
            None
        } else {
            for run in self.runs.iter().rev() {
                for glyph in run.glyphs.iter().rev() {
                    if glyph.position.x <= x {
                        return Some(glyph.index);
                    }
                }
            }
            Some(0)
        }
    }

    /// Whether a run reads right to left. Only such a line takes the bidi paths below.
    pub fn has_rtl(&self) -> bool {
        self.runs.iter().any(|run| run.is_rtl)
    }

    /// The visual x-spans that a byte range covers. A range across a change of direction is
    /// several spans.
    pub fn selection_spans(&self, range: Range<usize>) -> SmallVec<[Range<Pixels>; 2]> {
        let edges = bidi::edges(self, (0, 0), None, self.width);
        bidi::selection_spans(&edges, &range)
    }

    /// closest_index_for_x returns the character boundary closest to the given x coordinate
    /// (e.g. to handle aligning up/down arrow keys)
    pub fn closest_index_for_x(&self, x: Pixels) -> usize {
        if self.has_rtl() {
            let edges = bidi::edges(self, (0, 0), None, self.width);
            return bidi::closest_index(&edges, x, &(0..self.len));
        }
        let mut prev_index = 0;
        let mut prev_x = px(0.);

        for run in self.runs.iter() {
            for glyph in run.glyphs.iter() {
                if glyph.position.x >= x {
                    if glyph.position.x - x < x - prev_x {
                        return glyph.index;
                    } else {
                        return prev_index;
                    }
                }
                prev_index = glyph.index;
                prev_x = glyph.position.x;
            }
        }

        if self.len == 1 {
            if x > self.width / 2. {
                return 1;
            } else {
                return 0;
            }
        }

        self.len
    }

    /// The x position of the character at the given index: the caret before it, which is the
    /// right edge of a right-to-left character.
    pub fn x_for_index(&self, index: usize) -> Pixels {
        if self.has_rtl() {
            let edges = bidi::edges(self, (0, 0), None, self.width);
            return bidi::caret_x(&edges, index, &(0..self.len));
        }
        for run in &self.runs {
            for glyph in &run.glyphs {
                if glyph.index >= index {
                    return glyph.position.x;
                }
            }
        }
        self.width
    }

    /// The corresponding Font at the given index
    pub fn font_id_for_index(&self, index: usize) -> Option<FontId> {
        for run in &self.runs {
            for glyph in &run.glyphs {
                if glyph.index >= index {
                    return Some(run.font_id);
                }
            }
        }
        None
    }

    /// Split this layout at a byte index, returning `(prefix, suffix)`.
    ///
    /// - `prefix` contains glyphs for bytes `[0, byte_index)` with original positions.
    ///   Its width equals the x-advance up to the split point.
    /// - `suffix` contains glyphs for bytes `[byte_index, len)` with positions
    ///   shifted left so the first glyph starts at x=0, and byte indices rebased to 0.
    /// - `font_size`, `ascent`, and `descent` are copied to both halves.
    pub fn split_at(&self, byte_index: usize) -> (LineLayout, LineLayout) {
        let x_offset = self.x_for_index(byte_index);

        // Partition glyph runs. A single run may contribute glyphs to both halves.
        let mut left_runs = Vec::new();
        let mut right_runs = Vec::new();

        for run in &self.runs {
            let split_pos = run.glyphs.partition_point(|g| g.index < byte_index);

            if split_pos > 0 {
                left_runs.push(ShapedRun {
                    font_id: run.font_id,
                    glyphs: run.glyphs[..split_pos].to_vec(),
                    is_rtl: run.is_rtl,
                    font_size: run.font_size,
                });
            }

            if split_pos < run.glyphs.len() {
                let right_glyphs = run.glyphs[split_pos..]
                    .iter()
                    .map(|g| ShapedGlyph {
                        id: g.id,
                        position: point(g.position.x - x_offset, g.position.y),
                        index: g.index - byte_index,
                        is_emoji: g.is_emoji,
                    })
                    .collect();
                right_runs.push(ShapedRun {
                    font_id: run.font_id,
                    glyphs: right_glyphs,
                    is_rtl: run.is_rtl,
                    font_size: run.font_size,
                });
            }
        }

        let left = LineLayout {
            font_size: self.font_size,
            width: x_offset,
            ascent: self.ascent,
            descent: self.descent,
            runs: left_runs,
            len: byte_index,
        };

        let right = LineLayout {
            font_size: self.font_size,
            width: self.width - x_offset,
            ascent: self.ascent,
            descent: self.descent,
            runs: right_runs,
            len: self.len - byte_index,
        };

        (left, right)
    }

    fn compute_wrap_boundaries(
        &self,
        text: &str,
        wrap_width: Pixels,
        max_lines: Option<usize>,
        scratch: &mut WrapScratch,
    ) -> SmallVec<[WrapBoundary; 1]> {
        let mut boundaries = SmallVec::new();
        let mut current_segment: Option<Range<usize>> = None;
        let mut last_candidate_ix = None;
        let mut last_candidate_x = px(0.);
        let mut last_boundary = WrapBoundary {
            run_ix: 0,
            glyph_ix: 0,
        };
        let mut last_boundary_x = px(0.);
        // Shaping can combine characters across a line break opportunity, so we ignore those
        // break opportunities that would separate them.
        let WrapScratch {
            glyph_indices,
            break_indices,
        } = scratch;
        glyph_indices.clear();
        glyph_indices.extend(
            self.runs
                .iter()
                .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.index)),
        );
        break_indices.clear();
        break_indices
            .extend(LineWrapper::break_indices(text).filter(|index| glyph_indices.contains(index)));
        let mut glyphs = self
            .runs
            .iter()
            .enumerate()
            .flat_map(move |(run_ix, run)| {
                run.glyphs.iter().enumerate().map(move |(glyph_ix, glyph)| {
                    (
                        WrapBoundary { run_ix, glyph_ix },
                        glyph.index,
                        glyph.position.x,
                    )
                })
            })
            .dedup_by(|(_, previous_index, ..), (_, index, ..)| previous_index == index)
            .peekable();

        while let Some((boundary, index, x)) = glyphs.next() {
            let byte = text.as_bytes()[index];
            if byte == b'\n' {
                continue;
            }

            if byte != b' '
                && current_segment
                    .as_ref()
                    .is_none_or(|segment| !segment.contains(&index))
            {
                // Detect transitions between line-break segments rather than matching glyph
                // indices directly, to accommodate RTL text. Workaround for the lack of complete
                // UAX #9 support.
                let segment = break_indices.partition_point(|break_index| *break_index <= index);
                if current_segment.is_some() {
                    last_candidate_ix = Some(boundary);
                    last_candidate_x = x;
                }
                let start = segment
                    .checked_sub(1)
                    .and_then(|previous| break_indices.get(previous))
                    .copied()
                    .unwrap_or(0);
                let end = break_indices.get(segment).copied().unwrap_or(text.len());
                current_segment = Some(start..end);
            }

            let next_x = glyphs.peek().map_or(self.width, |(_, _, x)| *x);
            let width = next_x - last_boundary_x;

            if width > wrap_width && boundary > last_boundary {
                // When used line_clamp, we should limit the number of lines.
                if let Some(max_lines) = max_lines
                    && boundaries.len() >= max_lines.saturating_sub(1)
                {
                    break;
                }

                if let Some(last_candidate_ix) = last_candidate_ix.take() {
                    last_boundary = last_candidate_ix;
                    last_boundary_x = last_candidate_x;
                } else {
                    last_boundary = boundary;
                    last_boundary_x = x;
                }
                boundaries.push(last_boundary);
            }
        }

        boundaries
    }
}

/// A line of text that has been wrapped to fit a given width
#[derive(Default, Debug)]
pub struct WrappedLineLayout {
    /// The line layout, pre-wrapping.
    pub unwrapped_layout: Arc<LineLayout>,

    /// The boundaries at which the line was wrapped
    pub wrap_boundaries: SmallVec<[WrapBoundary; 1]>,

    /// The width of the line, if it was wrapped
    pub wrap_width: Option<Pixels>,

    /// For text with right-to-left characters, the byte index that starts each visual line, top
    /// to bottom. Such a line was broken in logical order and shaped on its own, so a line's
    /// glyphs follow one another in the unwrapped layout but their indices are not in order.
    /// Empty for other text.
    pub(crate) line_starts: SmallVec<[usize; 1]>,

    /// Whether the paragraph reads right to left (UAX #9 P2 and P3).
    pub(crate) rtl: bool,
}

/// A boundary at which a line was wrapped
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WrapBoundary {
    /// The index in the run just before the line was wrapped
    pub run_ix: usize,
    /// The index of the glyph just before the line was wrapped
    pub glyph_ix: usize,
}

impl WrappedLineLayout {
    /// Whether the paragraph reads right to left (UAX #9 P2 and P3): its first strong character
    /// does.
    pub fn is_rtl(&self) -> bool {
        self.rtl
    }

    /// `align` with [`TextAlign::Start`] resolved for this paragraph's direction.
    pub fn resolve_align(&self, align: TextAlign) -> TextAlign {
        match align {
            TextAlign::Start if self.rtl => TextAlign::Right,
            TextAlign::Start => TextAlign::Left,
            align => align,
        }
    }

    /// The x at which visual line `line` starts when the line paints with `align` in
    /// `align_width` (by default, the wrap width), relative to the paint origin. Positions from
    /// this layout (carets, selection spans, hit tests) are relative to the line's own start,
    /// so a caller that aligns adds this offset.
    pub fn aligned_line_x(
        &self,
        line: usize,
        align: TextAlign,
        align_width: Option<Pixels>,
    ) -> Pixels {
        let layout = &self.unwrapped_layout;
        let glyph_x = |boundary: &WrapBoundary| {
            layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix]
                .position
                .x
        };
        let start = line
            .checked_sub(1)
            .and_then(|previous| self.wrap_boundaries.get(previous))
            .map_or(px(0.), glyph_x);
        let end = self.wrap_boundaries.get(line).map_or(layout.width, glyph_x);
        let width = align_width.or(self.wrap_width).unwrap_or(layout.width);
        match self.resolve_align(align) {
            TextAlign::Center => (width - (end - start)) / 2.,
            TextAlign::Right => width - (end - start),
            TextAlign::Left | TextAlign::Start => px(0.),
        }
    }

    /// The length of the underlying text, in utf8 bytes.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.unwrapped_layout.len
    }

    /// The width of this line, in pixels, whether or not it was wrapped.
    pub fn width(&self) -> Pixels {
        self.wrap_width
            .unwrap_or(Pixels::MAX)
            .min(self.unwrapped_layout.width)
    }

    /// The size of the whole wrapped text, for the given line_height.
    /// can span multiple lines if there are multiple wrap boundaries.
    pub fn size(&self, line_height: Pixels) -> Size<Pixels> {
        Size {
            width: self.width(),
            height: line_height * (self.wrap_boundaries.len() + 1),
        }
    }

    /// The ascent of a line in this layout
    pub fn ascent(&self) -> Pixels {
        self.unwrapped_layout.ascent
    }

    /// The descent of a line in this layout
    pub fn descent(&self) -> Pixels {
        self.unwrapped_layout.descent
    }

    /// The wrap boundaries in this layout
    pub fn wrap_boundaries(&self) -> &[WrapBoundary] {
        &self.wrap_boundaries
    }

    /// The font size of this layout
    pub fn font_size(&self) -> Pixels {
        self.unwrapped_layout.font_size
    }

    /// The runs in this layout, sans wrapping
    pub fn runs(&self) -> &[ShapedRun] {
        &self.unwrapped_layout.runs
    }

    /// The byte range of each visual line, top to bottom.
    pub fn line_ranges(&self) -> SmallVec<[Range<usize>; 1]> {
        let starts: SmallVec<[usize; 4]> = if self.line_starts.is_empty() {
            std::iter::once(0)
                .chain(self.wrap_boundaries.iter().map(|boundary| {
                    self.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index
                }))
                .collect()
        } else {
            self.line_starts.iter().copied().collect()
        };
        starts
            .iter()
            .enumerate()
            .map(|(line, start)| *start..starts.get(line + 1).copied().unwrap_or(self.len()))
            .collect()
    }

    /// The visual x-spans, with each line's row, that a byte range covers: the selection
    /// highlight. Spans are relative to the line's own start, as glyphs paint.
    pub fn selection_spans(
        &self,
        range: Range<usize>,
        line_height: Pixels,
    ) -> SmallVec<[Bounds<Pixels>; 2]> {
        let mut spans = SmallVec::new();
        for (line, bytes) in self.line_ranges().into_iter().enumerate() {
            let overlap = range.start.max(bytes.start)..range.end.min(bytes.end);
            if overlap.start >= overlap.end {
                continue;
            }
            let (edges, start_x) = self.line_edges(line);
            for span in bidi::selection_spans(&edges, &overlap) {
                spans.push(Bounds::new(
                    point(span.start - start_x, line_height * line as f32),
                    size(span.end - span.start, line_height),
                ));
            }
        }
        spans
    }

    /// The glyph extents of one visual line and the x where that line starts.
    fn line_edges(&self, line: usize) -> (Vec<bidi::Edge>, Pixels) {
        let layout = &self.unwrapped_layout;
        let glyph_x = |boundary: &WrapBoundary| {
            layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix]
                .position
                .x
        };
        let start = line
            .checked_sub(1)
            .and_then(|previous| self.wrap_boundaries.get(previous));
        let end = self.wrap_boundaries.get(line);
        let start_x = start.map_or(px(0.), glyph_x);
        let end_x = end.map_or(layout.width, glyph_x);
        let edges = bidi::edges(
            layout,
            start.map_or((0, 0), |boundary| (boundary.run_ix, boundary.glyph_ix)),
            end.map(|boundary| (boundary.run_ix, boundary.glyph_ix)),
            end_x,
        );
        (edges, start_x)
    }

    /// Hit testing and carets for text laid out by line in logical order.
    fn bidi_index_for_position(
        &self,
        position: Point<Pixels>,
        line_height: Pixels,
        closest: bool,
    ) -> Result<usize, usize> {
        let line = (position.y / line_height) as usize;
        let lines = self.line_ranges();
        let Some(bytes) = lines.get(line) else {
            return Err(0);
        };
        let (edges, start_x) = self.line_edges(line);
        let x = position.x + start_x;
        let inside = edges
            .first()
            .zip(edges.last())
            .is_some_and(|(first, last)| first.left <= x && x < last.right);
        if !inside {
            return Err(bidi::closest_index(&edges, x, bytes));
        }
        if closest {
            Ok(bidi::closest_index(&edges, x, bytes))
        } else {
            bidi::index_at(&edges, x).ok_or(bytes.end)
        }
    }

    /// The index corresponding to a given position in this layout for the given line height.
    ///
    /// See also [`Self::closest_index_for_position`].
    pub fn index_for_position(
        &self,
        position: Point<Pixels>,
        line_height: Pixels,
    ) -> Result<usize, usize> {
        self._index_for_position(position, line_height, false)
    }

    /// The closest index to a given position in this layout for the given line height.
    ///
    /// Closest means the character boundary closest to the given position.
    ///
    /// See also [`LineLayout::closest_index_for_x`].
    pub fn closest_index_for_position(
        &self,
        position: Point<Pixels>,
        line_height: Pixels,
    ) -> Result<usize, usize> {
        self._index_for_position(position, line_height, true)
    }

    fn _index_for_position(
        &self,
        position: Point<Pixels>,
        line_height: Pixels,
        closest: bool,
    ) -> Result<usize, usize> {
        if !self.line_starts.is_empty() {
            return self.bidi_index_for_position(position, line_height, closest);
        }
        let wrapped_line_ix = (position.y / line_height) as usize;

        let wrapped_line_start_index;
        let wrapped_line_start_x;
        if wrapped_line_ix > 0 {
            let Some(line_start_boundary) = self.wrap_boundaries.get(wrapped_line_ix - 1) else {
                return Err(0);
            };
            let run = &self.unwrapped_layout.runs[line_start_boundary.run_ix];
            let glyph = &run.glyphs[line_start_boundary.glyph_ix];
            wrapped_line_start_index = glyph.index;
            wrapped_line_start_x = glyph.position.x;
        } else {
            wrapped_line_start_index = 0;
            wrapped_line_start_x = Pixels::ZERO;
        };

        let wrapped_line_end_index;
        let wrapped_line_end_x;
        if wrapped_line_ix < self.wrap_boundaries.len() {
            let next_wrap_boundary_ix = wrapped_line_ix;
            let next_wrap_boundary = self.wrap_boundaries[next_wrap_boundary_ix];
            let run = &self.unwrapped_layout.runs[next_wrap_boundary.run_ix];
            let glyph = &run.glyphs[next_wrap_boundary.glyph_ix];
            wrapped_line_end_index = glyph.index;
            wrapped_line_end_x = glyph.position.x;
        } else {
            wrapped_line_end_index = self.unwrapped_layout.len;
            wrapped_line_end_x = self.unwrapped_layout.width;
        };

        let mut position_in_unwrapped_line = position;
        position_in_unwrapped_line.x += wrapped_line_start_x;
        if position_in_unwrapped_line.x < wrapped_line_start_x {
            Err(wrapped_line_start_index)
        } else if position_in_unwrapped_line.x >= wrapped_line_end_x {
            Err(wrapped_line_end_index)
        } else {
            if closest {
                Ok(self
                    .unwrapped_layout
                    .closest_index_for_x(position_in_unwrapped_line.x))
            } else {
                // The shaper can place a trailing zero-width wrap boundary glyph slightly past
                // the line's width, so the row can extend past where `index_for_x` has glyphs.
                self.unwrapped_layout
                    .index_for_x(position_in_unwrapped_line.x)
                    .ok_or(wrapped_line_end_index)
            }
        }
    }

    /// Returns the pixel position for the given byte index: the caret before that character,
    /// which is its right edge when it reads right to left.
    pub fn position_for_index(&self, index: usize, line_height: Pixels) -> Option<Point<Pixels>> {
        if !self.line_starts.is_empty() {
            let lines = self.line_ranges();
            let line = lines.iter().position(|bytes| index <= bytes.end)?;
            if index < lines[line].start {
                return None;
            }
            let (edges, start_x) = self.line_edges(line);
            let x = bidi::caret_x(&edges, index, &lines[line]) - start_x;
            return Some(point(x, line_height * line as f32));
        }
        let mut line_start_ix = 0;
        let line_end_indices = self
            .wrap_boundaries
            .iter()
            .map(|wrap_boundary| {
                let run = &self.unwrapped_layout.runs[wrap_boundary.run_ix];
                let glyph = &run.glyphs[wrap_boundary.glyph_ix];
                glyph.index
            })
            .chain([self.len()])
            .enumerate();
        for (ix, line_end_ix) in line_end_indices {
            let line_y = ix as f32 * line_height;
            if index < line_start_ix {
                break;
            } else if index > line_end_ix {
                line_start_ix = line_end_ix;
                continue;
            } else {
                let line_start_x = self.unwrapped_layout.x_for_index(line_start_ix);
                let x = self.unwrapped_layout.x_for_index(index) - line_start_x;
                return Some(point(x, line_y));
            }
        }

        None
    }
}

pub(crate) struct LineLayoutCache {
    state: RefCell<LineLayoutCacheState>,
    wrap_scratch: RefCell<WrapScratch>,
    platform_text_system: Arc<dyn PlatformTextSystem>,
    /// Advances when [`TextSystem::add_fonts`] successfully changes the font database.
    font_generation: Arc<AtomicUsize>,
    /// Records the generation represented by both frame caches.
    cached_font_generation: AtomicUsize,
}

#[derive(Default)]
struct WrapScratch {
    glyph_indices: FxHashSet<usize>,
    break_indices: Vec<usize>,
}

#[derive(Default)]
struct LineLayoutCacheState {
    previous_frame: FrameCache,
    current_frame: FrameCache,
}

#[derive(Default)]
struct FrameCache {
    lines: FxHashMap<Arc<CacheKey>, Arc<LineLayout>>,
    wrapped_lines: FxHashMap<Arc<CacheKey>, Arc<WrappedLineLayout>>,
    used_lines: Vec<Arc<CacheKey>>,
    used_wrapped_lines: Vec<Arc<CacheKey>>,
}

#[derive(Clone, Default)]
pub(crate) struct LineLayoutIndex {
    font_generation: usize,
    lines_index: usize,
    wrapped_lines_index: usize,
}

impl LineLayoutCache {
    pub fn new(
        platform_text_system: Arc<dyn PlatformTextSystem>,
        font_generation: Arc<AtomicUsize>,
    ) -> Self {
        let cached_font_generation = font_generation.load(Ordering::Acquire);
        Self {
            state: RefCell::default(),
            wrap_scratch: RefCell::default(),
            platform_text_system,
            font_generation,
            cached_font_generation: AtomicUsize::new(cached_font_generation),
        }
    }

    pub fn layout_index(&self) -> LineLayoutIndex {
        let font_generation = self.clear_if_font_generation_changed();
        let state = self.state.borrow();
        let frame = &state.current_frame;
        LineLayoutIndex {
            font_generation,
            lines_index: frame.used_lines.len(),
            wrapped_lines_index: frame.used_wrapped_lines.len(),
        }
    }

    pub fn reuse_layouts(&self, range: Range<LineLayoutIndex>) {
        let font_generation = self.clear_if_font_generation_changed();
        if range.start.font_generation != font_generation
            || range.end.font_generation != font_generation
        {
            return;
        }
        let mut state = self.state.borrow_mut();
        let LineLayoutCacheState {
            previous_frame,
            current_frame,
        } = &mut *state;

        for key in &previous_frame.used_lines[range.start.lines_index..range.end.lines_index] {
            if let Some((key, line)) = previous_frame.lines.remove_entry(key) {
                current_frame.lines.insert(key, line);
            }
            current_frame.used_lines.push(key.clone());
        }

        for key in &previous_frame.used_wrapped_lines
            [range.start.wrapped_lines_index..range.end.wrapped_lines_index]
        {
            if let Some((key, line)) = previous_frame.wrapped_lines.remove_entry(key) {
                current_frame.wrapped_lines.insert(key, line);
            }
            current_frame.used_wrapped_lines.push(key.clone());
        }
    }

    pub fn truncate_layouts(&self, index: LineLayoutIndex) {
        let font_generation = self.clear_if_font_generation_changed();
        if index.font_generation != font_generation {
            return;
        }
        let mut state = self.state.borrow_mut();
        let current_frame = &mut state.current_frame;
        current_frame.used_lines.truncate(index.lines_index);
        current_frame
            .used_wrapped_lines
            .truncate(index.wrapped_lines_index);
    }

    pub fn finish_frame(&self) {
        let _font_generation = self.clear_if_font_generation_changed();
        let LineLayoutCacheState {
            previous_frame,
            current_frame,
        } = &mut *self.state.borrow_mut();
        std::mem::swap(previous_frame, current_frame);
        current_frame.lines.clear();
        current_frame.wrapped_lines.clear();
        current_frame.used_lines.clear();
        current_frame.used_wrapped_lines.clear();
    }

    pub fn layout_wrapped_line<Text>(
        &self,
        text: Text,
        font_size: Pixels,
        runs: &[FontRun],
        wrap_width: Option<Pixels>,
        max_lines: Option<usize>,
    ) -> Arc<WrappedLineLayout>
    where
        Text: AsRef<str>,
        SharedString: From<Text>,
    {
        let _font_generation = self.clear_if_font_generation_changed();
        let key = &CacheKeyRef {
            text: text.as_ref(),
            font_size,
            runs,
            wrap_width,
            force_width: None,
        } as &dyn AsCacheKeyRef;

        let state = self.state.borrow();
        if let Some(layout) = state.current_frame.wrapped_lines.get(key) {
            return layout.clone();
        }
        drop(state);

        let mut state = self.state.borrow_mut();
        let previous_frame_entry = state.previous_frame.wrapped_lines.remove_entry(key);
        if let Some((key, layout)) = previous_frame_entry {
            state
                .current_frame
                .wrapped_lines
                .insert(key.clone(), layout.clone());
            state.current_frame.used_wrapped_lines.push(key);
            layout
        } else {
            drop(state);
            let text = SharedString::from(text);
            let unwrapped_layout = self.layout_line::<&SharedString>(&text, font_size, runs, None);
            let layout = if bidi::has_rtl(&text) {
                Arc::new(self.layout_bidi_paragraph(
                    &text,
                    font_size,
                    runs,
                    unwrapped_layout,
                    wrap_width,
                    max_lines,
                ))
            } else {
                let wrap_boundaries = if let Some(wrap_width) = wrap_width {
                    unwrapped_layout.compute_wrap_boundaries(
                        text.as_ref(),
                        wrap_width,
                        max_lines,
                        &mut self.wrap_scratch.borrow_mut(),
                    )
                } else {
                    SmallVec::new()
                };
                Arc::new(WrappedLineLayout {
                    unwrapped_layout,
                    wrap_boundaries,
                    wrap_width,
                    line_starts: SmallVec::new(),
                    rtl: false,
                })
            };
            let key = Arc::new(CacheKey {
                text,
                font_size,
                runs: SmallVec::from(runs),
                wrap_width,
                force_width: None,
            });

            let mut state = self.state.borrow_mut();
            state
                .current_frame
                .wrapped_lines
                .insert(key.clone(), layout.clone());
            state.current_frame.used_wrapped_lines.push(key);

            layout
        }
    }

    pub fn layout_line<Text>(
        &self,
        text: Text,
        font_size: Pixels,
        runs: &[FontRun],
        force_width: Option<Pixels>,
    ) -> Arc<LineLayout>
    where
        Text: AsRef<str>,
        SharedString: From<Text>,
    {
        let _font_generation = self.clear_if_font_generation_changed();
        let key = &CacheKeyRef {
            text: text.as_ref(),
            font_size,
            runs,
            wrap_width: None,
            force_width,
        } as &dyn AsCacheKeyRef;

        let state = self.state.borrow();
        if let Some(layout) = state.current_frame.lines.get(key) {
            return layout.clone();
        }
        drop(state);

        let mut state = self.state.borrow_mut();
        let LineLayoutCacheState {
            previous_frame,
            current_frame,
        } = &mut *state;
        if let Some((key, layout)) = previous_frame.lines.remove_entry(key) {
            current_frame.lines.insert(key.clone(), layout.clone());
            current_frame.used_lines.push(key);
            layout
        } else {
            let text = SharedString::from(text);
            let mut layout = self.shape(&text, font_size, runs);

            if let Some(force_width) = force_width {
                apply_force_width_to_layout(&mut layout, force_width);
            }

            let key = Arc::new(CacheKey {
                text,
                font_size,
                runs: SmallVec::from(runs),
                wrap_width: None,
                force_width,
            });
            let layout = Arc::new(layout);
            current_frame.lines.insert(key.clone(), layout.clone());
            current_frame.used_lines.push(key);
            layout
        }
    }

    /// Shapes one line. Text with a right-to-left character is shaped in its paragraph's base
    /// direction (UAX #9 P2 and P3), and its runs record their directions.
    fn shape(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        if !bidi::has_rtl(text) {
            let mut layout = self
                .platform_text_system
                .layout_line(text, font_size, runs);
            apply_run_sizes(&mut layout, runs);
            apply_run_padding(&mut layout, runs);
            return layout;
        }
        let rtl = bidi::paragraph_is_rtl(text);
        let mut layout = self
            .platform_text_system
            .layout_line_in_direction(text, font_size, runs, rtl);
        apply_run_sizes(&mut layout, runs);
        apply_run_padding(&mut layout, runs);
        bidi::mark_run_directions(&mut layout, text, rtl);
        layout
    }

    /// Wraps a paragraph with right-to-left text. Lines break in logical order, measured on the
    /// unwrapped layout; then each line is shaped on its own in the paragraph's direction, so
    /// the shaper reorders it within the line (UAX #9 L1 to L4). The lines' glyphs follow one
    /// another in one layout, so painting and alignment work as for any wrapped line.
    fn layout_bidi_paragraph(
        &self,
        text: &str,
        font_size: Pixels,
        runs: &[FontRun],
        unwrapped: Arc<LineLayout>,
        wrap_width: Option<Pixels>,
        max_lines: Option<usize>,
    ) -> WrappedLineLayout {
        let rtl = bidi::paragraph_is_rtl(text);
        let single = |unwrapped_layout| WrappedLineLayout {
            unwrapped_layout,
            wrap_boundaries: SmallVec::new(),
            wrap_width,
            line_starts: SmallVec::from_buf([0]),
            rtl,
        };
        let Some(wrap_width) = wrap_width.filter(|width| unwrapped.width > *width) else {
            return single(unwrapped);
        };
        let lines = bidi::wrap_logically(
            text,
            &unwrapped,
            wrap_width,
            max_lines,
            LineWrapper::break_indices(text),
        );
        if lines.len() < 2 {
            return single(unwrapped);
        }
        let mut combined = LineLayout {
            font_size,
            width: px(0.),
            ascent: unwrapped.ascent,
            descent: unwrapped.descent,
            runs: Vec::new(),
            len: text.len(),
        };
        let mut wrap_boundaries = SmallVec::new();
        let mut line_starts = SmallVec::new();
        for range in lines {
            // Trailing spaces hang past the line's end, as in CSS, so they take no width and do
            // not move a right-aligned line. CoreText also widens a right-to-left line that
            // starts with a Latin word by its trailing space.
            let shaped = range.start..range.start + text[range.clone()].trim_end_matches(' ').len();
            let line_runs = bidi::runs_in(runs, shaped.clone());
            let line_text = &text[shaped];
            let mut line = self.platform_text_system.layout_line_in_direction(
                line_text,
                font_size,
                &line_runs,
                rtl,
            );
            apply_run_sizes(&mut line, &line_runs);
            apply_run_padding(&mut line, &line_runs);
            bidi::mark_run_directions(&mut line, line_text, rtl);
            if line.runs.iter().all(|run| run.glyphs.is_empty()) {
                continue;
            }
            if !combined.runs.is_empty() {
                wrap_boundaries.push(WrapBoundary {
                    run_ix: combined.runs.len(),
                    glyph_ix: 0,
                });
            }
            line_starts.push(range.start);
            let offset = combined.width;
            for mut run in line.runs.into_iter().filter(|run| !run.glyphs.is_empty()) {
                for glyph in &mut run.glyphs {
                    glyph.position.x += offset;
                    glyph.index += range.start;
                }
                combined.runs.push(run);
            }
            combined.width += line.width;
            combined.ascent = combined.ascent.max(line.ascent);
            combined.descent = combined.descent.max(line.descent);
        }
        WrappedLineLayout {
            unwrapped_layout: Arc::new(combined),
            wrap_boundaries,
            wrap_width: Some(wrap_width),
            line_starts,
            rtl,
        }
    }

    fn clear_if_font_generation_changed(&self) -> usize {
        let font_generation = self.font_generation.load(Ordering::Acquire);
        if self.cached_font_generation.load(Ordering::Acquire) == font_generation {
            return font_generation;
        }

        let mut state = self.state.borrow_mut();
        if self.cached_font_generation.load(Ordering::Acquire) == font_generation {
            return font_generation;
        }

        *state = LineLayoutCacheState::default();
        self.cached_font_generation
            .store(font_generation, Ordering::Release);
        font_generation
    }
}

// Combining marks (e.g. Thai vowel signs, Arabic diacritics) are shaped by
// HarfBuzz at the same x position as their base character. The force-width
// loop must not advance the cell counter for these zero-advance glyphs,
// otherwise they get displaced into the next cell. We detect them by checking
// whether shaped x has advanced by at least half a cell beyond the last base.
fn apply_force_width_to_layout(layout: &mut LineLayout, force_width: Pixels) {
    let mut glyph_pos: usize = 0;
    // NEG_INFINITY ensures the first glyph is always classified as a base.
    let mut last_base_shaped_x = px(f32::NEG_INFINITY);
    let mut last_base_actual_x = px(0.);

    for run in layout.runs.iter_mut() {
        for glyph in run.glyphs.iter_mut() {
            let shaped_x = glyph.position.x;

            if shaped_x > last_base_shaped_x + force_width * 0.5 {
                let forced_x = glyph_pos * force_width;
                if (shaped_x - forced_x).abs() > px(1.) {
                    glyph.position.x = forced_x;
                }
                last_base_shaped_x = shaped_x;
                last_base_actual_x = glyph.position.x;
                glyph_pos += 1;
            } else {
                glyph.position.x = last_base_actual_x + (shaped_x - last_base_shaped_x);
            }
        }
    }
}

/// Gives each shaped run the size of the text runs that its glyphs come from, and splits a
/// shaped run where that size changes: the shaper merges adjacent glyphs of one font. Platforms
/// shape each [`FontRun`] at its own size, so only painting needs this.
fn apply_run_sizes(layout: &mut LineLayout, runs: &[FontRun]) {
    if runs.iter().all(|run| run.font_size.is_none()) {
        return;
    }
    let mut ends = SmallVec::<[(usize, Option<Pixels>); 8]>::new();
    let mut end = 0;
    for run in runs {
        end += run.len;
        ends.push((end, run.font_size));
    }
    let size_at = |index: usize| {
        let run = ends.partition_point(|(end, _)| *end <= index);
        ends.get(run).and_then(|(_, size)| *size)
    };
    let mut sized: Vec<ShapedRun> = Vec::with_capacity(layout.runs.len());
    for run in layout.runs.drain(..) {
        let mut current: Option<ShapedRun> = None;
        for glyph in run.glyphs {
            let font_size = size_at(glyph.index);
            match &mut current {
                Some(current) if current.font_size == font_size => current.glyphs.push(glyph),
                _ => {
                    sized.extend(current.take());
                    current = Some(ShapedRun {
                        font_id: run.font_id,
                        glyphs: vec![glyph],
                        is_rtl: run.is_rtl,
                        font_size,
                    });
                }
            }
        }
        sized.extend(current);
    }
    layout.runs = sized;
}

/// Spaces the glyphs of each padded box ([`crate::TextRun::padding`]): its padding goes before
/// the box's first glyph and after its last, in visual order, so a box that a direction change
/// or a wrap splits is padded where each piece starts and ends on screen. The line widens by
/// all of its padding, so wrapping and alignment see it.
fn apply_run_padding(layout: &mut LineLayout, runs: &[FontRun]) {
    if runs.iter().all(|run| run.padding.is_none()) {
        return;
    }
    // Each run's end and its box: adjacent runs with the same padding are one box.
    let mut ends = SmallVec::<[(usize, Option<(usize, Pixels)>); 8]>::new();
    let mut end = 0;
    let mut boxes = 0;
    let mut previous: Option<Pixels> = None;
    for run in runs {
        end += run.len;
        let padded = run.padding.map(|padding| {
            if previous != Some(padding) {
                boxes += 1;
            }
            (boxes, padding)
        });
        previous = run.padding;
        ends.push((end, padded));
    }
    let box_at = |index: usize| {
        let run = ends.partition_point(|(end, _)| *end <= index);
        ends.get(run).and_then(|(_, padded)| *padded)
    };
    let mut shift = px(0.);
    let mut current: Option<(usize, Pixels)> = None;
    for run in &mut layout.runs {
        for glyph in &mut run.glyphs {
            let padded = box_at(glyph.index);
            if padded.map(|(id, _)| id) != current.map(|(id, _)| id) {
                if let Some((_, padding)) = current {
                    shift += padding;
                }
                if let Some((_, padding)) = padded {
                    shift += padding;
                }
                current = padded;
            }
            glyph.position.x += shift;
        }
    }
    if let Some((_, padding)) = current {
        shift += padding;
    }
    layout.width += shift;
}

/// A run of text with a single font.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[expect(missing_docs)]
pub struct FontRun {
    pub len: usize,
    pub font_id: FontId,
    /// The run's own size; `None` uses the line's size.
    pub font_size: Option<Pixels>,
    /// The run's inline padding ([`crate::TextRun::padding`]).
    pub padding: Option<Pixels>,
}

trait AsCacheKeyRef {
    fn as_cache_key_ref(&self) -> CacheKeyRef<'_>;
}

#[derive(Clone, Debug, Eq)]
struct CacheKey {
    text: SharedString,
    font_size: Pixels,
    runs: SmallVec<[FontRun; 1]>,
    wrap_width: Option<Pixels>,
    force_width: Option<Pixels>,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct CacheKeyRef<'a> {
    text: &'a str,
    font_size: Pixels,
    runs: &'a [FontRun],
    wrap_width: Option<Pixels>,
    force_width: Option<Pixels>,
}

impl PartialEq for dyn AsCacheKeyRef + '_ {
    fn eq(&self, other: &dyn AsCacheKeyRef) -> bool {
        self.as_cache_key_ref() == other.as_cache_key_ref()
    }
}

impl Eq for dyn AsCacheKeyRef + '_ {}

impl Hash for dyn AsCacheKeyRef + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_cache_key_ref().hash(state)
    }
}

impl AsCacheKeyRef for CacheKey {
    fn as_cache_key_ref(&self) -> CacheKeyRef<'_> {
        CacheKeyRef {
            text: &self.text,
            font_size: self.font_size,
            runs: self.runs.as_slice(),
            wrap_width: self.wrap_width,
            force_width: self.force_width,
        }
    }
}

impl PartialEq for CacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.as_cache_key_ref().eq(&other.as_cache_key_ref())
    }
}

impl Hash for CacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_cache_key_ref().hash(state);
    }
}

impl<'a> Borrow<dyn AsCacheKeyRef + 'a> for Arc<CacheKey> {
    fn borrow(&self) -> &(dyn AsCacheKeyRef + 'a) {
        self.as_ref() as &dyn AsCacheKeyRef
    }
}

impl AsCacheKeyRef for CacheKeyRef<'_> {
    fn as_cache_key_ref(&self) -> CacheKeyRef<'_> {
        *self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GlyphId;

    fn glyph_at(x: f32, index: usize) -> ShapedGlyph {
        ShapedGlyph {
            id: GlyphId(0),
            position: point(px(x), px(0.)),
            index,
            is_emoji: false,
        }
    }

    fn make_layout(glyphs: Vec<ShapedGlyph>) -> LineLayout {
        LineLayout {
            font_size: px(16.),
            width: px(100.),
            ascent: px(12.),
            descent: px(4.),
            runs: vec![ShapedRun {
                font_id: FontId(0),
                glyphs,
                is_rtl: false,
                font_size: None,
            }],
            len: 0,
        }
    }

    fn glyph_x_positions(layout: &LineLayout) -> Vec<f32> {
        layout.runs[0]
            .glyphs
            .iter()
            .map(|g| f32::from(g.position.x))
            .collect()
    }

    #[test]
    fn test_wrap_preserves_shaped_clusters() {
        let mut layout = make_layout(vec![
            glyph_at(0., 0),
            glyph_at(5., 0),
            glyph_at(10., 3),
            glyph_at(15., 3),
        ]);
        layout.width = px(20.);
        layout.len = "你好".len();
        assert_eq!(
            layout
                .compute_wrap_boundaries("你好", px(15.), None, &mut WrapScratch::default())
                .as_slice(),
            &[WrapBoundary {
                run_ix: 0,
                glyph_ix: 2
            }],
        );

        let mut layout = make_layout(vec![glyph_at(0., 0), glyph_at(10., 0)]);
        layout.width = px(20.);
        layout.len = 1;
        assert!(
            layout
                .compute_wrap_boundaries("a", px(10.), None, &mut WrapScratch::default())
                .is_empty()
        );
    }

    #[test]
    fn test_wrap_right_to_left_text() {
        for (text, indices, expected_boundaries) in [
            ("אבג דהו", vec![11, 9, 7, 6, 4, 2, 0], vec![4]),
            ("a אבג b", vec![0, 1, 6, 4, 2, 8, 9], vec![2, 6]),
        ] {
            let mut layout = make_layout(
                indices
                    .iter()
                    .enumerate()
                    .map(|(position, index)| glyph_at(position as f32 * 10., *index))
                    .collect(),
            );
            layout.width = px(indices.len() as f32 * 10.);
            layout.len = text.len();
            assert_eq!(
                layout.compute_wrap_boundaries(text, px(40.), None, &mut WrapScratch::default()),
                expected_boundaries
                    .into_iter()
                    .map(|glyph_ix| WrapBoundary {
                        run_ix: 0,
                        glyph_ix
                    })
                    .collect::<SmallVec<[WrapBoundary; 1]>>(),
                "{text:?}",
            );
        }
    }

    #[test]
    fn test_wrap_ignores_breaks_inside_shaped_clusters() {
        let text = "x a-bcde";
        let mut layout = make_layout(
            [0, 1, 2, 5, 6, 7]
                .into_iter()
                .map(|index| glyph_at(index as f32 * 10., index))
                .collect(),
        );
        layout.width = px(80.);
        layout.len = text.len();
        assert_eq!(
            layout
                .compute_wrap_boundaries(text, px(60.), None, &mut WrapScratch::default())
                .as_slice(),
            &[WrapBoundary {
                run_ix: 0,
                glyph_ix: 2
            }],
        );
    }

    #[test]
    fn test_force_width_latin_unchanged() {
        let cell_width = px(8.);
        let mut layout = make_layout(vec![glyph_at(0., 0), glyph_at(8., 1), glyph_at(16., 2)]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0., 8., 16.]);
    }

    #[test]
    fn test_force_width_combining_marks_not_advanced() {
        let cell_width = px(8.);
        // Simulates Thai "กี" — base consonant at x=0, combining vowel also at x=0
        let mut layout = make_layout(vec![
            glyph_at(0., 0), // ก (base)
            glyph_at(0., 3), // ี (combining mark, same x)
        ]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0., 0.]);
    }

    #[test]
    fn test_force_width_base_after_combining_mark() {
        let cell_width = px(8.);
        let mut layout = make_layout(vec![glyph_at(0., 0), glyph_at(0., 3), glyph_at(8., 6)]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0., 0., 8.]);
    }

    #[test]
    fn test_force_width_multiple_combining_marks() {
        let cell_width = px(8.);
        // Simulates "ก้" — base + vowel + tone mark (two combining marks stacked)
        let mut layout = make_layout(vec![
            glyph_at(0., 0), // ก (base)
            glyph_at(0., 3), // vowel (combining)
            glyph_at(0., 6), // tone mark (combining)
            glyph_at(8., 9), // next base
        ]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0., 0., 0., 8.]);
    }

    #[test]
    fn test_force_width_corrects_drifted_base_positions() {
        let cell_width = px(8.);
        // Font metrics don't perfectly match cell grid — glyphs drift >1px from cell boundary
        let mut layout = make_layout(vec![
            glyph_at(0.5, 0),  // within 1px tolerance, kept as-is
            glyph_at(10.2, 1), // >1px off from 8.0, corrected
            glyph_at(19.8, 2), // >1px off from 16.0, corrected
        ]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0.5, 8., 16.]);
    }

    #[test]
    fn test_force_width_combining_mark_after_within_tolerance_base() {
        let cell_width = px(8.);
        // Base glyph is within 1px of grid so it keeps its shaped position.
        // The combining mark must align to the base's actual position, not the grid slot.
        let mut layout = make_layout(vec![glyph_at(0.5, 0), glyph_at(0.5, 3)]);

        apply_force_width_to_layout(&mut layout, cell_width);

        let positions = glyph_x_positions(&layout);
        assert_eq!(positions, vec![0.5, 0.5]);
    }
}
